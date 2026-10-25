use error_stack::{Report, ResultExt};
use remora_context::model::ResolvedContext;
use remora_ota::{
    adapter::gateway::{
        ArtifactChunks, ArtifactSink, DeploymentEvent, DeploymentFeed, Error, InitialUpload,
        OtaGatewayAdapter, Result,
    },
    model::{
        Artifact, Deployment, DeploymentFilter, DeploymentStatus, DeploymentSummary, Labels,
        LogEntry, LogProgress, Release, ReleaseSummary,
    },
};
use std::time::{Duration, Instant};

use remora_platform_http::{
    model::{self, artifact_upload, deployment_log, time_of, ResourceReference},
    status_summary, Body, Code, Connection, Gateway, Message, Method, Response, Status, WebSocket,
};
use tokio::{sync::mpsc, task::JoinHandle};

/// The remora gateway's releases and deployments, over its REST API.
pub struct OtaGatewayAdapterImpl;

async fn gateway(context: &ResolvedContext) -> Result<Gateway> {
    Gateway::new(&Connection::for_resolved(context))
        .await
        .change_context(Error::Unreachable)
}

/// Turns a refusal into this port's error, keeping the server's own
/// message ("the target device already has a non-terminal deployment").
fn classify(status: Status) -> Report<Error> {
    let error = match status.code {
        Code::Unauthenticated => Error::Unauthenticated,
        Code::PermissionDenied => Error::PermissionDenied,
        Code::NotFound => Error::NotFound,
        Code::AlreadyExists => Error::AlreadyExists,
        Code::FailedPrecondition => Error::FailedPrecondition,
        Code::InvalidArgument | Code::OutOfRange => Error::InvalidArgument,
        Code::Unavailable => Error::Unreachable,
        _ => Error::Call,
    };
    Report::new(error).attach(status_summary(&status))
}

fn labels(map: model::Labels) -> Labels {
    map.into_iter().collect()
}

fn label_map(labels: &Labels) -> model::Labels {
    labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn name_of(reference: Option<ResourceReference>) -> String {
    reference
        .map(|reference| reference.name)
        .unwrap_or_default()
}

fn log_entry(entry: model::DeploymentLogEntry) -> LogEntry {
    LogEntry {
        execution: entry.execution,
        result: entry.result,
        code: entry.code,
        details: entry.details,
        recorded_at: time_of(entry.recorded_at.as_deref()),
        progress: entry.progress.map(|progress| LogProgress {
            current: progress.current,
            max: progress.max,
            unit: progress.unit,
            speed: progress.speed,
        }),
    }
}

/// A map filter, the way a REST route takes it: `<field>[key]=value`.
fn filter(url: &mut remora_platform_http::Url, field: &str, labels: &Labels) {
    for (key, value) in labels {
        url.query_pairs_mut()
            .append_pair(&format!("{field}[{key}]"), value);
    }
}

#[async_trait::async_trait]
impl OtaGatewayAdapter for OtaGatewayAdapterImpl {
    async fn list_releases(
        &self,
        context: &ResolvedContext,
        labels: &Labels,
    ) -> Result<Vec<ReleaseSummary>> {
        let gateway = gateway(context).await?;
        let mut url = gateway.url(["remora", "v1", "release"]);
        filter(&mut url, "releaseLabels", labels);
        let response: model::ListReleasesResponse = gateway.get(url).await.map_err(classify)?;
        let mut releases: Vec<_> = response
            .release_summaries
            .into_iter()
            .map(|summary| ReleaseSummary {
                name: name_of(summary.resource),
                version: summary.version,
            })
            .collect();
        releases.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(releases)
    }

    async fn get_release(&self, context: &ResolvedContext, name: &str) -> Result<Release> {
        let gateway = gateway(context).await?;
        let descriptor = gateway
            .get::<model::GetReleaseResponse>(gateway.url(["remora", "v1", "release", name]))
            .await
            .map_err(classify)?
            .release_descriptor
            .ok_or_else(|| Report::new(Error::Call).attach("empty release descriptor"))?;
        Ok(Release {
            name: name_of(descriptor.resource),
            version: descriptor.version,
            labels: labels(descriptor.labels),
            artifacts: descriptor
                .artifacts
                .into_iter()
                .map(|artifact| Artifact {
                    file_name: artifact.file_name,
                    content_type: artifact.content_type,
                    content_length: u64::try_from(artifact.content_length).unwrap_or(0),
                    checksum_sha256: artifact.checksum_sha256,
                    tag_condition: artifact.tag_condition,
                })
                .collect(),
        })
    }

    async fn create_release(
        &self,
        context: &ResolvedContext,
        name: &str,
        version: &str,
        labels: &Labels,
    ) -> Result<()> {
        let gateway = gateway(context).await?;
        gateway
            .post::<_, model::ReleaseReferenceResponse>(
                gateway.url(["remora", "v1", "release"]),
                &model::CreateReleaseRequest {
                    release_name: name.to_owned(),
                    release_version: version.to_owned(),
                    release_labels: label_map(labels),
                },
            )
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn update_release_labels(
        &self,
        context: &ResolvedContext,
        name: &str,
        set: &Labels,
        unset: &[String],
    ) -> Result<()> {
        let gateway = gateway(context).await?;
        gateway
            .patch::<_, model::ReleaseReferenceResponse>(
                gateway.url(["remora", "v1", "release", name]),
                &model::UpdateReleaseRequest {
                    release_labels: Some(model::ReleaseLabelsUpdate {
                        set: label_map(set),
                        merge: true,
                        unset: unset.to_vec(),
                    }),
                },
            )
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn delete_release(&self, context: &ResolvedContext, name: &str) -> Result<()> {
        let gateway = gateway(context).await?;
        gateway
            .delete::<model::ReleaseReferenceResponse>(
                gateway.url(["remora", "v1", "release", name]),
            )
            .await
            .map_err(classify)?;
        Ok(())
    }

    /// 128 random bits, hex.
    fn new_content_id(&self) -> String {
        use rand_core::RngCore;
        let mut bytes = [0u8; 16];
        rand_core::OsRng.fill_bytes(&mut bytes);
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    async fn upload_offset(&self, context: &ResolvedContext, content_id: &str) -> Result<u64> {
        let gateway = gateway(context).await?;
        let offset = gateway
            .get::<model::UploadOffsetResponse>(gateway.url([
                "remora",
                "v1",
                "release-artifact-upload-offset",
                content_id,
            ]))
            .await
            .map_err(classify)?
            .artifact_content_offset;
        Ok(u64::try_from(offset).unwrap_or(0))
    }

    /// The platform's upload in short requests: the chunks streamed into
    /// one `PUT` at a time, each ended after [`REQUEST_BYTES`] or
    /// [`REQUEST_TIME`], then a commit of the whole.
    async fn begin_upload(
        &self,
        context: &ResolvedContext,
        upload: InitialUpload,
        offset: u64,
    ) -> Result<Box<dyn ArtifactSink>> {
        let gateway = gateway(context).await?;
        let url = gateway.url([
            "remora",
            "v1",
            "release",
            &upload.release,
            "artifact-upload",
            &upload.content_id,
        ]);
        Ok(Box::new(HttpArtifactSink {
            gateway,
            url,
            upload,
            offset,
            request: None,
        }))
    }

    /// A ranged `GET` from `offset`: what a resumed download asks for.
    async fn download(
        &self,
        context: &ResolvedContext,
        release: &str,
        file_name: &str,
        offset: u64,
    ) -> Result<Box<dyn ArtifactChunks>> {
        let gateway = gateway(context).await?;
        let url = gateway.url(["remora", "v1", "release", release, "artifact", file_name]);
        let mut request = gateway.request(Method::GET, url);
        if offset > 0 {
            request = request.header("range", format!("bytes={offset}-"));
        }
        let response = gateway.execute(request).await.map_err(classify)?;
        let status = response.status().as_u16();
        if status == 416 {
            // Nothing from there on: done if that is where the artifact
            // ends (`bytes */<total>`), else asked for past its end.
            let total = response
                .headers()
                .get("content-range")
                .and_then(|value| value.to_str().ok())
                .and_then(|range| range.strip_prefix("bytes */"))
                .and_then(|total| total.parse::<u64>().ok());
            if total == Some(offset) {
                return Ok(Box::new(HttpArtifactChunks {
                    response: None,
                    skip: 0,
                }));
            }
            return Err(Report::new(Error::InvalidArgument).attach(format!(
                "offset {offset} is past the artifact's end ({})",
                total.map_or("unknown".to_owned(), |total| format!("{total} bytes"))
            )));
        }
        if !response.status().is_success() {
            return Err(classify(Status::of(response).await));
        }
        // A 200 to a ranged request is the whole artifact (the range was
        // ignored): what precedes the offset is skipped here.
        let skip = if status == 200 { offset } else { 0 };
        Ok(Box::new(HttpArtifactChunks {
            response: Some(response),
            skip,
        }))
    }

    async fn create_deployment(
        &self,
        context: &ResolvedContext,
        name: &str,
        release: &str,
        device: &str,
        labels: &Labels,
    ) -> Result<()> {
        let gateway = gateway(context).await?;
        gateway
            .post::<_, model::DeploymentReferenceResponse>(
                gateway.url(["remora", "v1", "deployment"]),
                &model::CreateDeploymentRequest {
                    deployment_name: name.to_owned(),
                    deployment_release_name: release.to_owned(),
                    deployment_target_name: device.to_owned(),
                    deployment_labels: label_map(labels),
                },
            )
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn get_deployment(&self, context: &ResolvedContext, name: &str) -> Result<Deployment> {
        let gateway = gateway(context).await?;
        let descriptor = gateway
            .get::<model::GetDeploymentResponse>(gateway.url(["remora", "v1", "deployment", name]))
            .await
            .map_err(classify)?
            .deployment_descriptor
            .ok_or_else(|| Report::new(Error::Call).attach("empty deployment descriptor"))?;
        Ok(Deployment {
            name: name_of(descriptor.resource),
            labels: labels(descriptor.labels),
            release: name_of(descriptor.release_reference),
            target: name_of(descriptor.target_reference),
            status: DeploymentStatus::parse(&descriptor.status_value),
            details: labels(descriptor.status_details),
            updated_at: time_of(descriptor.status_timestamp.as_deref()),
        })
    }

    async fn list_deployments(
        &self,
        context: &ResolvedContext,
        deployments: &DeploymentFilter,
    ) -> Result<Vec<DeploymentSummary>> {
        let gateway = gateway(context).await?;
        let mut url = gateway.url(["remora", "v1", "deployment"]);
        for (field, value) in [
            ("deploymentTargetName", &deployments.device),
            ("deploymentReleaseName", &deployments.release),
        ] {
            if let Some(value) = value {
                url.query_pairs_mut().append_pair(field, value);
            }
        }
        filter(&mut url, "deploymentLabels", &deployments.labels);
        let response: model::ListDeploymentsResponse = gateway.get(url).await.map_err(classify)?;
        let mut deployments: Vec<_> = response
            .deployment_summaries
            .into_iter()
            .map(|summary| DeploymentSummary {
                name: name_of(summary.resource),
                status: DeploymentStatus::parse(&summary.status_value),
            })
            .collect();
        deployments.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(deployments)
    }

    async fn start_deployment(&self, context: &ResolvedContext, name: &str) -> Result<()> {
        let gateway = gateway(context).await?;
        gateway
            .post::<_, model::DeploymentReferenceResponse>(
                gateway.url(["remora", "v1", "deployment", name, "start"]),
                &serde_json::json!({}),
            )
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn cancel_deployment(&self, context: &ResolvedContext, name: &str) -> Result<()> {
        let gateway = gateway(context).await?;
        gateway
            .post::<_, model::DeploymentReferenceResponse>(
                gateway.url(["remora", "v1", "deployment", name, "cancel"]),
                &serde_json::json!({}),
            )
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn delete_deployment(&self, context: &ResolvedContext, name: &str) -> Result<()> {
        let gateway = gateway(context).await?;
        gateway
            .delete::<model::DeploymentReferenceResponse>(gateway.url([
                "remora",
                "v1",
                "deployment",
                name,
            ]))
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn deployment_logs(
        &self,
        context: &ResolvedContext,
        name: &str,
    ) -> Result<Vec<LogEntry>> {
        let gateway = gateway(context).await?;
        let response: model::ListDeploymentLogEntriesResponse = gateway
            .get(gateway.url(["remora", "v1", "deployment", name, "log"]))
            .await
            .map_err(classify)?;
        Ok(response
            .deployment_log_entries
            .into_iter()
            .map(log_entry)
            .collect())
    }

    async fn follow_deployment(
        &self,
        context: &ResolvedContext,
        name: &str,
    ) -> Result<Box<dyn DeploymentFeed>> {
        let gateway = gateway(context).await?;
        let url = gateway.url(["remora", "v1", "deployment", name, "log", "stream"]);
        let (socket, _) = gateway
            .websocket(url, deployment_log::SUBPROTOCOL)
            .await
            .map_err(classify)?;
        Ok(Box::new(WebSocketDeploymentFeed {
            socket,
            ended: false,
        }))
    }
}

/// A deployment's log WebSocket, frame by frame.
struct WebSocketDeploymentFeed {
    socket: WebSocket,
    ended: bool,
}

#[async_trait::async_trait]
impl DeploymentFeed for WebSocketDeploymentFeed {
    async fn next(&mut self) -> Result<Option<DeploymentEvent>> {
        use futures_util::StreamExt;
        while !self.ended {
            // Reading is also what answers the gateway's pings.
            let message = match self.socket.next().await {
                Some(Ok(message)) => message,
                Some(Err(error)) => {
                    self.ended = true;
                    return Err(classify(Status::unavailable(&error)));
                }
                None => {
                    self.ended = true;
                    return Err(Report::new(Error::Unreachable)
                        .attach("the deployment's feed ended without a word"));
                }
            };
            match message {
                Message::Text(text) => {
                    match serde_json::from_str::<deployment_log::Frame>(text.as_str()) {
                        Ok(deployment_log::Frame::Status {
                            status_value,
                            status_details,
                            status_timestamp,
                        }) => {
                            return Ok(Some(DeploymentEvent::Status {
                                status: DeploymentStatus::parse(&status_value),
                                details: labels(status_details),
                                updated_at: time_of(status_timestamp.as_deref()),
                            }))
                        }
                        Ok(deployment_log::Frame::Log { entry }) => {
                            return Ok(Some(DeploymentEvent::Log(log_entry(entry))))
                        }
                        // A frame this version doesn't know: skipped.
                        Err(_) => {}
                    }
                }
                Message::Close(frame) => {
                    self.ended = true;
                    let (code, reason) = frame
                        .map(|frame| (u16::from(frame.code), frame.reason.to_string()))
                        .unwrap_or((1005, String::new()));
                    return match (code, reason.as_str()) {
                        (1000, deployment_log::DELETED) => Err(Report::new(Error::NotFound)
                            .attach("the deployment was deleted while followed")),
                        (1000, _) => Ok(None),
                        // The gateway lost track of it: worth opening again.
                        _ => Err(classify(Status::new(Code::Unavailable, reason))),
                    };
                }
                Message::Binary(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }
        Ok(None)
    }
}

/// A download's body, piece by piece.
struct HttpArtifactChunks {
    /// `None` when there is nothing to read.
    response: Option<Response>,
    /// Bytes still to drop before the ones asked for.
    skip: u64,
}

#[async_trait::async_trait]
impl ArtifactChunks for HttpArtifactChunks {
    async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        let Some(response) = &mut self.response else {
            return Ok(None);
        };
        loop {
            // A body cut short (fewer bytes than its length) is an error
            // here, not an end: the caller resumes from what it has.
            let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| classify(Status::unavailable(&error)))?
            else {
                return Ok(None);
            };
            let skipped = usize::try_from(self.skip)
                .unwrap_or(usize::MAX)
                .min(chunk.len());
            self.skip -= skipped as u64;
            if skipped < chunk.len() {
                return Ok(Some(chunk[skipped..].to_vec()));
            }
        }
    }
}

/// What one upload request carries at most -- whichever comes first, so
/// that each stays well under the platform ingress's 60 s even on a slow
/// link, and a request lost costs little.
const REQUEST_BYTES: u64 = 16 * 1024 * 1024;
const REQUEST_TIME: Duration = Duration::from_secs(30);

/// What the sink hands a request's body.
enum Outgoing {
    Chunk(Vec<u8>),
    /// This request is complete: end its body.
    End,
}

/// A request's body: chunks until [`Outgoing::End`]. Its sender going away
/// without one (a sink dropped or aborted mid-way) never ends it: the sink
/// drops the call, and with it the connection -- the platform keeps what
/// arrived.
struct UntilEnd(mpsc::Receiver<Outgoing>);

impl futures_util::Stream for UntilEnd {
    type Item = std::result::Result<Vec<u8>, std::io::Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        match self.0.poll_recv(cx) {
            Poll::Ready(Some(Outgoing::Chunk(chunk))) => Poll::Ready(Some(Ok(chunk))),
            Poll::Ready(Some(Outgoing::End)) => Poll::Ready(None),
            Poll::Ready(None) | Poll::Pending => Poll::Pending,
        }
    }
}

/// One `PUT` in flight.
struct Request {
    chunks: Option<mpsc::Sender<Outgoing>>,
    /// Answers the offset the platform holds once it took the body.
    call: Option<JoinHandle<Result<u64>>>,
    sent: u64,
    started: Instant,
}

impl Request {
    /// The call's own outcome, once the body is over.
    async fn outcome(&mut self) -> Result<u64> {
        let Some(call) = self.call.take() else {
            return Err(Report::new(Error::Call).attach("the upload request has already ended"));
        };
        call.await
            .map_err(|e| Report::new(Error::Call).attach(e.to_string()))?
    }

    /// Sends one item; when the call already ended, reports why it did
    /// rather than that a send failed.
    async fn push(&mut self, item: Outgoing) -> Result<()> {
        let Some(chunks) = &self.chunks else {
            return Err(Report::new(Error::Call).attach("the upload request has already failed"));
        };
        if chunks.send(item).await.is_ok() {
            return Ok(());
        }
        self.chunks = None;
        self.outcome().await?;
        Err(Report::new(Error::Call).attach("the gateway ended the upload request early"))
    }
}

impl Drop for Request {
    /// A request dropped before its end is cut off: what arrived stays.
    fn drop(&mut self) {
        if let Some(call) = self.call.take() {
            call.abort();
        }
    }
}

/// What a `PUT` answered: the offset the platform now holds, or why not.
async fn put_outcome(answer: std::result::Result<Response, Status>) -> Result<u64> {
    let response = answer.map_err(classify)?;
    let held = response
        .headers()
        .get(artifact_upload::OFFSET_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    match response.status().as_u16() {
        200 | 204 => held.ok_or_else(|| {
            Report::new(Error::Call).attach("the platform answered no Upload-Offset")
        }),
        // Both mean: ask how far it got, and go on from there.
        409 => Err(Report::new(Error::Call).attach(format!(
            "the platform holds {} bytes of this upload, not what was sent after",
            held.map_or("an unknown number of".to_owned(), |held| held.to_string())
        ))),
        423 => Err(Report::new(Error::Call)
            .attach("an earlier request of this upload is still being written")),
        410 => Err(Report::new(Error::AlreadyExists).attach("this upload is already committed")),
        _ => Err(classify(Status::of(response).await)),
    }
}

struct HttpArtifactSink {
    gateway: Gateway,
    /// `.../artifact-upload/{content_id}`.
    url: remora_platform_http::Url,
    upload: InitialUpload,
    /// What the platform holds, as of the last request that ended.
    offset: u64,
    request: Option<Request>,
}

impl HttpArtifactSink {
    /// Starts a `PUT` from where the platform is.
    fn start(&self) -> Request {
        let (chunks, outgoing) = mpsc::channel(4);
        let request = self
            .gateway
            .request(Method::PUT, self.url.clone())
            .header(artifact_upload::OFFSET_HEADER, self.offset)
            .header("content-type", "application/octet-stream")
            .body(Body::wrap_stream(UntilEnd(outgoing)));
        let gateway = self.gateway.clone();
        // The answer only comes once the body is over, so the call runs
        // alongside the sends.
        let call = tokio::spawn(async move { put_outcome(gateway.execute(request).await).await });
        Request {
            chunks: Some(chunks),
            call: Some(call),
            sent: 0,
            started: Instant::now(),
        }
    }

    /// Ends the request in flight, checking the platform took all of it.
    async fn end_request(&mut self) -> Result<()> {
        let Some(mut request) = self.request.take() else {
            return Ok(());
        };
        request.push(Outgoing::End).await?;
        let held = request.outcome().await?;
        let expected = self.offset + request.sent;
        if held != expected {
            return Err(Report::new(Error::Call).attach(format!(
                "the platform holds {held} bytes of this upload, not {expected}"
            )));
        }
        self.offset = held;
        Ok(())
    }
}

#[async_trait::async_trait]
impl ArtifactSink for HttpArtifactSink {
    async fn send(&mut self, chunk: Vec<u8>) -> Result<()> {
        if self.request.is_none() {
            self.request = Some(self.start());
        }
        let request = self.request.as_mut().expect("just started");
        let length = chunk.len() as u64;
        request.push(Outgoing::Chunk(chunk)).await?;
        request.sent += length;
        if request.sent >= REQUEST_BYTES || request.started.elapsed() >= REQUEST_TIME {
            self.end_request().await?;
        }
        Ok(())
    }

    async fn finish(mut self: Box<Self>) -> Result<()> {
        self.end_request().await?;
        // The platform requires a content type, as `curl -F` always sends.
        let content_type = match self.upload.content_type.as_str() {
            "" => "application/octet-stream".to_owned(),
            given => given.to_owned(),
        };
        let mut url = self.url.clone();
        url.path_segments_mut()
            .expect("a base URL has a host")
            .push("commit");
        self.gateway
            .post::<_, artifact_upload::CommitResponse>(
                url,
                &artifact_upload::CommitRequest {
                    artifact_file_name: self.upload.file_name.clone(),
                    artifact_content_type: content_type,
                    artifact_tag_condition: self.upload.tag_condition.clone(),
                    artifact_content_length: i64::try_from(self.upload.content_length)
                        .unwrap_or(i64::MAX),
                    artifact_content_checksum: self.upload.checksum_sha256.clone(),
                },
            )
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn abort(mut self: Box<Self>) {
        if let Some(mut request) = self.request.take() {
            if let Some(call) = request.call.take() {
                call.abort();
                // Wait for the call to be gone, so that the connection is
                // cut before the caller moves on (or the process exits).
                let _ = call.await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use remora_ota::model::Labels;

    use std::time::Duration;

    use remora_platform_http::test_gateway::{State, TestGateway};

    use super::*;

    #[tokio::test]
    async fn releases_round_trip() {
        let (_, context) = TestGateway::serve().await;
        let adapter = OtaGatewayAdapterImpl;
        let labels = Labels::from([("channel".into(), "beta".into())]);
        adapter
            .create_release(&context, "r1", "1.2.3", &labels)
            .await
            .unwrap();
        let report = adapter
            .create_release(&context, "r1", "1.2.3", &labels)
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::AlreadyExists));

        adapter
            .update_release_labels(
                &context,
                "r1",
                &Labels::from([("tested".into(), "yes".into())]),
                &["channel".into()],
            )
            .await
            .unwrap();
        let release = adapter.get_release(&context, "r1").await.unwrap();
        assert_eq!(release.version, "1.2.3");
        assert_eq!(
            release.labels,
            Labels::from([("tested".into(), "yes".into())])
        );
        assert_eq!(
            adapter
                .list_releases(&context, &Labels::new())
                .await
                .unwrap()
                .len(),
            1
        );

        adapter.delete_release(&context, "r1").await.unwrap();
        let report = adapter.get_release(&context, "r1").await.unwrap_err();
        assert!(matches!(report.current_context(), Error::NotFound));
    }

    fn sha256(bytes: &[u8]) -> String {
        use sha2::Digest;
        sha2::Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// The opening of an upload of `whole` as `bundle.raucb` to `r1`.
    fn initial(id: &str, whole: &[u8]) -> InitialUpload {
        InitialUpload {
            release: "r1".into(),
            file_name: "bundle.raucb".into(),
            content_type: String::new(),
            tag_condition: "board:rp5".into(),
            content_id: id.into(),
            content_length: whole.len() as u64,
            checksum_sha256: sha256(whole),
        }
    }

    async fn with_release() -> (TestGateway, remora_context::model::ResolvedContext) {
        let (gateway, context) = TestGateway::serve().await;
        OtaGatewayAdapterImpl
            .create_release(&context, "r1", "1", &Labels::new())
            .await
            .unwrap();
        (gateway, context)
    }

    #[tokio::test]
    async fn an_upload_lands_as_an_artifact() {
        let (gateway, context) = with_release().await;
        let adapter = OtaGatewayAdapterImpl;
        let whole = b"hello bundle";
        let mut sink = adapter
            .begin_upload(&context, initial("id-1", whole), 0)
            .await
            .unwrap();
        sink.send(b"hello ".to_vec()).await.unwrap();
        sink.send(b"bundle".to_vec()).await.unwrap();
        sink.finish().await.unwrap();

        assert_eq!(adapter.upload_offset(&context, "id-1").await.unwrap(), 12);
        assert_eq!(gateway.artifact_bytes("r1", "bundle.raucb").unwrap(), whole);
        let release = adapter.get_release(&context, "r1").await.unwrap();
        assert_eq!(release.artifacts[0].tag_condition, "board:rp5");
        assert_eq!(release.artifacts[0].content_length, 12);
        assert_eq!(release.artifacts[0].checksum_sha256, sha256(whole));
        // No content type given: the one the platform requires anyway.
        assert_eq!(
            release.artifacts[0].content_type,
            "application/octet-stream"
        );

        // Committed: never written again.
        let mut sink = adapter
            .begin_upload(&context, initial("id-1", whole), 12)
            .await
            .unwrap();
        sink.send(b"more".to_vec()).await.unwrap();
        let report = sink.finish().await.unwrap_err();
        assert!(matches!(report.current_context(), Error::AlreadyExists));
    }

    /// Each request carries at most so much: a big artifact goes in
    /// several, and arrives whole.
    #[tokio::test]
    async fn a_big_upload_goes_in_several_requests() {
        let (gateway, context) = with_release().await;
        let adapter = OtaGatewayAdapterImpl;
        let whole: Vec<u8> = (0..2 * REQUEST_BYTES as usize + 5)
            .map(|i| (i % 251) as u8)
            .collect();
        let mut sink = adapter
            .begin_upload(&context, initial("big", &whole), 0)
            .await
            .unwrap();
        for chunk in whole.chunks(1024 * 1024) {
            sink.send(chunk.to_vec()).await.unwrap();
        }
        sink.finish().await.unwrap();
        assert_eq!(gateway.0.lock().unwrap().upload_requests, 3);
        assert_eq!(gateway.artifact_bytes("r1", "bundle.raucb").unwrap(), whole);
    }

    #[tokio::test]
    async fn an_artifact_downloads_whole_or_from_an_offset() {
        let (_gateway, context) = with_release().await;
        let adapter = OtaGatewayAdapterImpl;
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut sink = adapter
            .begin_upload(&context, initial("id-1", &bytes), 0)
            .await
            .unwrap();
        sink.send(bytes.clone()).await.unwrap();
        sink.finish().await.unwrap();

        let read_all = |offset: u64| {
            let adapter = &adapter;
            let context = &context;
            async move {
                let mut chunks = adapter
                    .download(context, "r1", "bundle.raucb", offset)
                    .await
                    .unwrap();
                let mut out = Vec::new();
                while let Some(chunk) = chunks.next().await.unwrap() {
                    out.extend(chunk);
                }
                out
            }
        };
        assert_eq!(read_all(0).await, bytes);
        assert_eq!(read_all(150_000).await, bytes[150_000..]);
        // Resuming a download that was complete: nothing more.
        assert!(read_all(bytes.len() as u64).await.is_empty());
        let report = adapter
            .download(&context, "r1", "bundle.raucb", bytes.len() as u64 + 1)
            .await
            .err()
            .unwrap();
        assert!(matches!(report.current_context(), Error::InvalidArgument));

        let err = adapter
            .download(&context, "r1", "missing", 0)
            .await
            .err()
            .unwrap();
        assert!(matches!(err.current_context(), Error::NotFound));
    }

    #[tokio::test]
    async fn a_dropped_upload_reports_the_gateways_reason() {
        let (gateway, context) = with_release().await;
        let adapter = OtaGatewayAdapterImpl;
        let whole = vec![0; 50 * 1024];
        gateway.0.lock().unwrap().fail_next_upload_after = Some(4);
        let mut sink = adapter
            .begin_upload(&context, initial("id-2", &whole), 0)
            .await
            .unwrap();
        let mut failed = None;
        for chunk in whole.chunks(1024) {
            if let Err(report) = sink.send(chunk.to_vec()).await {
                failed = Some(report);
                break;
            }
        }
        let report = match failed {
            Some(report) => report,
            None => sink.finish().await.unwrap_err(),
        };
        assert!(
            format!("{report:?}").contains("connection reset"),
            "{report:?}"
        );
        assert_eq!(adapter.upload_offset(&context, "id-2").await.unwrap(), 4);
    }

    /// Waits until `done` holds of the fake's state.
    async fn until(gateway: &TestGateway, done: impl Fn(&State) -> bool) {
        for _ in 0..500 {
            if done(&gateway.0.lock().unwrap()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let state = gateway.0.lock().unwrap();
        panic!(
            "the gateway never got there: {:?}",
            (
                state.abandoned_uploads,
                state.upload_offset("a"),
                state.upload_offset("b")
            )
        );
    }

    #[tokio::test]
    async fn an_upload_stopped_short_is_never_committed() {
        let (gateway, context) = with_release().await;
        let adapter = OtaGatewayAdapterImpl;
        let whole = b"half a bundle";

        // Aborted on purpose: what arrived stays for a resume, uncommitted.
        let mut sink = adapter
            .begin_upload(&context, initial("a", whole), 0)
            .await
            .unwrap();
        sink.send(b"half a ".to_vec()).await.unwrap();
        until(&gateway, |state| state.upload_offset("a") == 7).await;
        sink.abort().await;
        until(&gateway, |state| state.abandoned_uploads == 1).await;

        // Dropped on an early return: same thing.
        let mut sink = adapter
            .begin_upload(&context, initial("b", whole), 0)
            .await
            .unwrap();
        sink.send(b"half".to_vec()).await.unwrap();
        until(&gateway, |state| state.upload_offset("b") == 4).await;
        drop(sink);
        until(&gateway, |state| state.abandoned_uploads == 2).await;

        assert!(gateway.artifact_bytes("r1", "bundle.raucb").is_none());
        let release = adapter.get_release(&context, "r1").await.unwrap();
        assert!(release.artifacts.is_empty());

        // From where it got, the rest; finishing commits the whole of it.
        let offset = adapter.upload_offset(&context, "a").await.unwrap();
        assert_eq!(offset, 7);
        let mut sink = adapter
            .begin_upload(&context, initial("a", whole), offset)
            .await
            .unwrap();
        sink.send(b"bundle".to_vec()).await.unwrap();
        sink.finish().await.unwrap();
        assert_eq!(gateway.artifact_bytes("r1", "bundle.raucb").unwrap(), whole);
    }

    /// Sent from an offset the platform doesn't hold, the bytes are
    /// refused: never stored twice, never stored with a gap.
    #[tokio::test]
    async fn an_upload_from_the_wrong_offset_is_refused() {
        let (gateway, context) = with_release().await;
        let adapter = OtaGatewayAdapterImpl;
        let whole = b"half a bundle";
        let mut sink = adapter
            .begin_upload(&context, initial("c", whole), 7)
            .await
            .unwrap();
        let _ = sink.send(b"bundle".to_vec()).await;
        let report = sink.finish().await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Call));
        assert!(
            format!("{report:?}").contains("holds 0 bytes"),
            "{report:?}"
        );
        assert_eq!(gateway.0.lock().unwrap().upload_offset("c"), 0);
    }

    /// The platform commits only the artifact announced: one whose bytes
    /// changed is refused, and what was stored dropped.
    #[tokio::test]
    async fn a_commit_refuses_an_artifact_that_isnt_the_one_announced() {
        let (gateway, context) = with_release().await;
        let adapter = OtaGatewayAdapterImpl;
        let mut sink = adapter
            .begin_upload(&context, initial("d", b"half a bundle"), 0)
            .await
            .unwrap();
        sink.send(b"half a bungle".to_vec()).await.unwrap();
        let report = sink.finish().await.unwrap_err();
        assert!(matches!(report.current_context(), Error::InvalidArgument));
        assert!(
            format!("{report:?}").contains("does not match"),
            "{report:?}"
        );
        assert!(gateway.artifact_bytes("r1", "bundle.raucb").is_none());
        assert_eq!(adapter.upload_offset(&context, "d").await.unwrap(), 0);
    }

    /// Storage gone mid-way: what was stored is where the rest goes.
    #[tokio::test]
    async fn an_upload_resumes_after_what_was_stored() {
        let (gateway, context) = with_release().await;
        let adapter = OtaGatewayAdapterImpl;
        let whole = b"half a bundle";
        gateway.0.lock().unwrap().fail_next_upload_after = Some(7);
        let mut sink = adapter
            .begin_upload(&context, initial("e", whole), 0)
            .await
            .unwrap();
        let _ = sink.send(whole.to_vec()).await;
        let report = sink.finish().await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Unreachable));

        let offset = adapter.upload_offset(&context, "e").await.unwrap();
        assert_eq!(offset, 7);
        let mut sink = adapter
            .begin_upload(&context, initial("e", whole), offset)
            .await
            .unwrap();
        sink.send(whole[offset as usize..].to_vec()).await.unwrap();
        sink.finish().await.unwrap();
        assert_eq!(gateway.artifact_bytes("r1", "bundle.raucb").unwrap(), whole);
    }

    #[tokio::test]
    async fn content_ids_are_fresh() {
        let adapter = OtaGatewayAdapterImpl;
        let id = adapter.new_content_id();
        assert_eq!(id.len(), 32);
        assert_ne!(id, adapter.new_content_id());
    }

    #[tokio::test]
    async fn deployments_progress_and_refuse_a_second_one_per_device() {
        let (_, context) = TestGateway::serve().await;
        let adapter = OtaGatewayAdapterImpl;
        adapter
            .create_release(&context, "r1", "1", &Labels::new())
            .await
            .unwrap();
        adapter
            .create_deployment(&context, "d1", "r1", "DEV1", &Labels::new())
            .await
            .unwrap();
        let report = adapter
            .create_deployment(&context, "d2", "r1", "DEV1", &Labels::new())
            .await
            .unwrap_err();
        assert!(matches!(
            report.current_context(),
            Error::FailedPrecondition
        ));
        assert!(format!("{report:?}").contains("non-terminal deployment"));

        assert_eq!(
            adapter.get_deployment(&context, "d1").await.unwrap().status,
            DeploymentStatus::Pending
        );
        adapter.start_deployment(&context, "d1").await.unwrap();
        let mut status = DeploymentStatus::Running;
        for _ in 0..5 {
            status = adapter.get_deployment(&context, "d1").await.unwrap().status;
            if status.is_terminal() {
                break;
            }
        }
        assert_eq!(status, DeploymentStatus::Succeeded);
        let logs = adapter.deployment_logs(&context, "d1").await.unwrap();
        assert_eq!(logs[0].progress.as_ref().unwrap().current, 50);
        assert_eq!(logs.last().unwrap().result, "success");

        let deployment = adapter.get_deployment(&context, "d1").await.unwrap();
        assert_eq!(
            (deployment.release.as_str(), deployment.target.as_str()),
            ("r1", "DEV1")
        );
        assert!(deployment.updated_at.is_some());
        let listed = adapter
            .list_deployments(
                &context,
                &DeploymentFilter {
                    device: Some("DEV1".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        adapter.delete_deployment(&context, "d1").await.unwrap();
    }

    async fn next_events(feed: &mut Box<dyn DeploymentFeed>) -> Vec<DeploymentEvent> {
        let mut events = Vec::new();
        while let Some(event) = feed.next().await.unwrap() {
            events.push(event);
        }
        events
    }

    /// The feed says where the deployment is, then each report and change
    /// as it happens, and ends once it is settled.
    #[tokio::test]
    async fn a_deployment_feed_carries_it_to_the_end() {
        let (_, context) = TestGateway::serve().await;
        let adapter = OtaGatewayAdapterImpl;
        adapter
            .create_release(&context, "r1", "1", &Labels::new())
            .await
            .unwrap();
        for (name, device) in [("d1", "DEV1"), ("d2", "BROKEN")] {
            adapter
                .create_deployment(&context, name, "r1", device, &Labels::new())
                .await
                .unwrap();
            adapter.start_deployment(&context, name).await.unwrap();
        }

        let mut feed = adapter.follow_deployment(&context, "d1").await.unwrap();
        let events = next_events(&mut feed).await;
        assert!(matches!(
            &events[0],
            DeploymentEvent::Status {
                status: DeploymentStatus::Running,
                ..
            }
        ));
        let reports: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                DeploymentEvent::Log(entry) => Some(entry.clone()),
                DeploymentEvent::Status { .. } => None,
            })
            .collect();
        assert_eq!(reports.len(), 3);
        assert_eq!(reports[0].progress.as_ref().unwrap().current, 50);
        assert_eq!(reports[2].result, "success");
        assert!(matches!(
            events.last().unwrap(),
            DeploymentEvent::Status {
                status: DeploymentStatus::Succeeded,
                ..
            }
        ));
        assert!(feed.next().await.unwrap().is_none());

        let mut feed = adapter.follow_deployment(&context, "d2").await.unwrap();
        match next_events(&mut feed).await.last().unwrap() {
            DeploymentEvent::Status {
                status, details, ..
            } => {
                assert_eq!(status, &DeploymentStatus::Failed);
                assert_eq!(details["reason"], "bundle signature invalid");
            }
            other => panic!("{other:?}"),
        }

        let report = match adapter.follow_deployment(&context, "nope").await {
            Ok(_) => panic!("no such deployment"),
            Err(report) => report,
        };
        assert!(matches!(report.current_context(), Error::NotFound));
    }

    /// A feed the gateway loses is said so, as worth opening again; one
    /// whose deployment is deleted, as gone.
    #[tokio::test]
    async fn a_deployment_feed_cut_short_or_deleted() {
        let (gateway, context) = TestGateway::serve().await;
        let adapter = OtaGatewayAdapterImpl;
        adapter
            .create_release(&context, "r1", "1", &Labels::new())
            .await
            .unwrap();
        for (name, device) in [("running", "DEV1"), ("draft", "DEV2")] {
            adapter
                .create_deployment(&context, name, "r1", device, &Labels::new())
                .await
                .unwrap();
        }
        adapter.start_deployment(&context, "running").await.unwrap();
        gateway.0.lock().unwrap().fail_next_log_stream_after = Some(1);
        let mut feed = adapter
            .follow_deployment(&context, "running")
            .await
            .unwrap();
        assert!(feed.next().await.unwrap().is_some());
        let report = feed.next().await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Unreachable));
        assert!(format!("{report:?}").contains("lost the event feed"));

        // A draft never moves: deleted while followed.
        let mut feed = adapter.follow_deployment(&context, "draft").await.unwrap();
        assert!(feed.next().await.unwrap().is_some());
        adapter.delete_deployment(&context, "draft").await.unwrap();
        let report = feed.next().await.unwrap_err();
        assert!(matches!(report.current_context(), Error::NotFound));
    }
}
