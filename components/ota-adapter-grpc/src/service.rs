use std::time::{Duration, SystemTime};

use error_stack::{Report, ResultExt};
use remora_context::model::ResolvedContext;
use remora_ota::{
    adapter::gateway::{ArtifactSink, Error, InitialUpload, OtaGatewayAdapter, Result},
    model::{
        Artifact, Deployment, DeploymentFilter, DeploymentStatus, DeploymentSummary, Labels,
        LogEntry, LogProgress, Release, ReleaseSummary,
    },
};
use remora_platform_grpc::{
    connect, sntns::service::remora::v1 as pb, status_summary, Connection, GatewayChannel,
};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_stream::wrappers::ReceiverStream;

type Releases = pb::release_service_client::ReleaseServiceClient<GatewayChannel>;
type Deployments = pb::deployment_service_client::DeploymentServiceClient<GatewayChannel>;
type Devices = pb::device_service_client::DeviceServiceClient<GatewayChannel>;

/// The remora gateway's release, deployment and device services over gRPC.
pub struct OtaGatewayAdapterImpl;

async fn channel(context: &ResolvedContext) -> Result<GatewayChannel> {
    connect(&Connection::for_context(
        &context.context,
        Some(&context.credentials),
    ))
    .await
    .change_context(Error::Unreachable)
}

/// Turns a gRPC status into this port's error, keeping the server's own
/// message ("the target device already has a non-terminal deployment").
fn classify(status: tonic::Status) -> Report<Error> {
    use tonic::Code;
    let error = match status.code() {
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

fn labels(map: std::collections::HashMap<String, String>) -> Labels {
    map.into_iter().collect()
}

fn hash_map(labels: &Labels) -> std::collections::HashMap<String, String> {
    labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn name_of(
    reference: Option<remora_platform_grpc::sntns::service::v1::ResourceReference>,
) -> String {
    reference
        .map(|reference| reference.name)
        .unwrap_or_default()
}

fn time_of(timestamp: Option<prost_types::Timestamp>) -> Option<SystemTime> {
    let timestamp = timestamp?;
    let seconds = u64::try_from(timestamp.seconds).ok()?;
    let nanos = u32::try_from(timestamp.nanos).ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::new(seconds, nanos))
}

#[async_trait::async_trait]
impl OtaGatewayAdapter for OtaGatewayAdapterImpl {
    async fn list_releases(
        &self,
        context: &ResolvedContext,
        labels: &Labels,
    ) -> Result<Vec<ReleaseSummary>> {
        let response = Releases::new(channel(context).await?)
            .list_releases(pb::ReleaseServiceListReleasesRequest {
                release_labels: hash_map(labels),
            })
            .await
            .map_err(classify)?
            .into_inner();
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
        let descriptor = Releases::new(channel(context).await?)
            .get_release(pb::ReleaseServiceGetReleaseRequest {
                release_name: name.to_owned(),
            })
            .await
            .map_err(classify)?
            .into_inner()
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
        Releases::new(channel(context).await?)
            .create_release(pb::ReleaseServiceCreateReleaseRequest {
                release_name: name.to_owned(),
                release_version: version.to_owned(),
                release_labels: hash_map(labels),
            })
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
        Releases::new(channel(context).await?)
            .update_release(pb::ReleaseServiceUpdateReleaseRequest {
                release_name: name.to_owned(),
                release_labels: Some(pb::release_service_update_release_request::Labels {
                    set: hash_map(set),
                    merge: true,
                    unset: unset.to_vec(),
                }),
            })
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn delete_release(&self, context: &ResolvedContext, name: &str) -> Result<()> {
        Releases::new(channel(context).await?)
            .delete_release(pb::ReleaseServiceDeleteReleaseRequest {
                release_name: name.to_owned(),
            })
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn upload_offset(&self, context: &ResolvedContext, content_id: &str) -> Result<u64> {
        let offset = Releases::new(channel(context).await?)
            .get_release_artifact_upload_offset(
                pb::ReleaseServiceGetReleaseArtifactUploadOffsetRequest {
                    artifact_content_id: content_id.to_owned(),
                },
            )
            .await
            .map_err(classify)?
            .into_inner()
            .artifact_content_offset;
        Ok(u64::try_from(offset).unwrap_or(0))
    }

    async fn begin_upload(
        &self,
        context: &ResolvedContext,
        upload: InitialUpload,
    ) -> Result<Box<dyn ArtifactSink>> {
        use pb::release_service_upload_release_artifact_request::Request;
        // An artifact is a whole bundle: no 4 MiB cap on what we send.
        let mut client =
            Releases::new(channel(context).await?).max_encoding_message_size(usize::MAX);
        let (chunks, outgoing) = mpsc::channel(4);
        chunks
            .send(pb::ReleaseServiceUploadReleaseArtifactRequest {
                request: Some(Request::InitialRequest(
                    pb::ReleaseServiceUploadReleaseArtifactInitialRequest {
                        release_name: upload.release,
                        artifact_file_name: upload.file_name,
                        artifact_content_type: upload.content_type,
                        artifact_tag_condition: upload.tag_condition,
                        artifact_content_id: upload.content_id,
                    },
                )),
            })
            .await
            .expect("the receiver is held just below");
        // Client streaming answers only once the request stream ends, so the
        // call runs alongside the sends and `finish` collects its answer.
        let call = tokio::spawn(async move {
            client
                .upload_release_artifact(ReceiverStream::new(outgoing))
                .await
                .map(|_| ())
                .map_err(classify)
        });
        Ok(Box::new(GrpcArtifactSink {
            chunks: Some(chunks),
            call,
        }))
    }

    async fn list_devices(
        &self,
        context: &ResolvedContext,
        labels: &Labels,
    ) -> Result<Vec<String>> {
        let response = Devices::new(channel(context).await?)
            .list_devices(pb::DeviceServiceListDevicesRequest {
                device_labels: hash_map(labels),
            })
            .await
            .map_err(classify)?
            .into_inner();
        let mut devices: Vec<_> = response
            .device_summaries
            .into_iter()
            .map(|summary| name_of(summary.resource))
            .filter(|name| !name.is_empty())
            .collect();
        devices.sort();
        Ok(devices)
    }

    async fn create_deployment(
        &self,
        context: &ResolvedContext,
        name: &str,
        release: &str,
        device: &str,
        labels: &Labels,
    ) -> Result<()> {
        Deployments::new(channel(context).await?)
            .create_deployment(pb::DeploymentServiceCreateDeploymentRequest {
                deployment_name: name.to_owned(),
                deployment_release_name: release.to_owned(),
                deployment_target_name: device.to_owned(),
                deployment_labels: hash_map(labels),
            })
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn get_deployment(&self, context: &ResolvedContext, name: &str) -> Result<Deployment> {
        let descriptor = Deployments::new(channel(context).await?)
            .get_deployment(pb::DeploymentServiceGetDeploymentRequest {
                deployment_name: name.to_owned(),
            })
            .await
            .map_err(classify)?
            .into_inner()
            .deployment_descriptor
            .ok_or_else(|| Report::new(Error::Call).attach("empty deployment descriptor"))?;
        Ok(Deployment {
            name: name_of(descriptor.resource),
            labels: labels(descriptor.labels),
            release: name_of(descriptor.release_reference),
            target: name_of(descriptor.target_reference),
            status: DeploymentStatus::parse(&descriptor.status_value),
            details: labels(descriptor.status_details),
            updated_at: time_of(descriptor.status_timestamp),
        })
    }

    async fn list_deployments(
        &self,
        context: &ResolvedContext,
        filter: &DeploymentFilter,
    ) -> Result<Vec<DeploymentSummary>> {
        let response = Deployments::new(channel(context).await?)
            .list_deployments(pb::DeploymentServiceListDeploymentsRequest {
                deployment_target_name: filter.device.clone().unwrap_or_default(),
                deployment_release_name: filter.release.clone().unwrap_or_default(),
                deployment_labels: hash_map(&filter.labels),
            })
            .await
            .map_err(classify)?
            .into_inner();
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
        Deployments::new(channel(context).await?)
            .start_deployment(pb::DeploymentServiceStartDeploymentRequest {
                deployment_name: name.to_owned(),
            })
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn cancel_deployment(&self, context: &ResolvedContext, name: &str) -> Result<()> {
        Deployments::new(channel(context).await?)
            .cancel_deployment(pb::DeploymentServiceCancelDeploymentRequest {
                deployment_name: name.to_owned(),
            })
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn delete_deployment(&self, context: &ResolvedContext, name: &str) -> Result<()> {
        Deployments::new(channel(context).await?)
            .delete_deployment(pb::DeploymentServiceDeleteDeploymentRequest {
                deployment_name: name.to_owned(),
            })
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn deployment_logs(
        &self,
        context: &ResolvedContext,
        name: &str,
    ) -> Result<Vec<LogEntry>> {
        let response = Deployments::new(channel(context).await?)
            .list_deployment_log_entries(pb::DeploymentServiceListDeploymentLogEntriesRequest {
                deployment_name: name.to_owned(),
            })
            .await
            .map_err(classify)?
            .into_inner();
        Ok(response
            .deployment_log_entries
            .into_iter()
            .map(|entry| LogEntry {
                execution: entry.execution,
                result: entry.result,
                code: entry.code,
                details: entry.details,
                recorded_at: time_of(entry.recorded_at),
                progress: entry.progress.map(|progress| LogProgress {
                    current: progress.current,
                    max: progress.max,
                    unit: progress.unit,
                    speed: progress.speed,
                }),
            })
            .collect())
    }
}

struct GrpcArtifactSink {
    chunks: Option<mpsc::Sender<pb::ReleaseServiceUploadReleaseArtifactRequest>>,
    call: JoinHandle<Result<()>>,
}

impl GrpcArtifactSink {
    /// The call's own outcome, once the request stream is over.
    async fn outcome(call: JoinHandle<Result<()>>) -> Result<()> {
        call.await
            .map_err(|e| Report::new(Error::Call).attach(e.to_string()))?
    }
}

#[async_trait::async_trait]
impl ArtifactSink for GrpcArtifactSink {
    async fn send(&mut self, chunk: Vec<u8>) -> Result<()> {
        use pb::release_service_upload_release_artifact_request::Request;
        let Some(chunks) = &self.chunks else {
            return Err(Report::new(Error::Call).attach("the upload has already failed"));
        };
        let request = pb::ReleaseServiceUploadReleaseArtifactRequest {
            request: Some(Request::SubsequentRequest(
                pb::ReleaseServiceUploadReleaseArtifactSubsequentRequest {
                    artifact_chunk: chunk,
                },
            )),
        };
        if chunks.send(request).await.is_ok() {
            return Ok(());
        }
        // The call ended early: report why it did, not that a send failed.
        self.chunks = None;
        let call = std::mem::replace(&mut self.call, tokio::spawn(async { Ok(()) }));
        Self::outcome(call).await?;
        Err(Report::new(Error::Call).attach("the gateway ended the upload early"))
    }

    async fn finish(mut self: Box<Self>) -> Result<()> {
        self.chunks = None;
        Self::outcome(self.call).await
    }
}

#[cfg(test)]
mod tests {
    use remora_ota::model::Labels;

    use super::*;
    use crate::test_gateway::TestGateway;

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

    #[tokio::test]
    async fn an_upload_lands_as_an_artifact() {
        let (gateway, context) = TestGateway::serve().await;
        let adapter = OtaGatewayAdapterImpl;
        adapter
            .create_release(&context, "r1", "1", &Labels::new())
            .await
            .unwrap();
        let mut sink = adapter
            .begin_upload(
                &context,
                InitialUpload {
                    release: "r1".into(),
                    file_name: "bundle.raucb".into(),
                    content_type: "application/octet-stream".into(),
                    tag_condition: "board:rp5".into(),
                    content_id: "id-1".into(),
                },
            )
            .await
            .unwrap();
        sink.send(b"hello ".to_vec()).await.unwrap();
        sink.send(b"bundle".to_vec()).await.unwrap();
        sink.finish().await.unwrap();

        assert_eq!(adapter.upload_offset(&context, "id-1").await.unwrap(), 12);
        assert_eq!(
            gateway.artifact_bytes("r1", "bundle.raucb").unwrap(),
            b"hello bundle"
        );
        let release = adapter.get_release(&context, "r1").await.unwrap();
        assert_eq!(release.artifacts[0].tag_condition, "board:rp5");
        assert_eq!(release.artifacts[0].content_length, 12);
    }

    #[tokio::test]
    async fn a_dropped_upload_reports_the_gateways_reason() {
        let (gateway, context) = TestGateway::serve().await;
        let adapter = OtaGatewayAdapterImpl;
        adapter
            .create_release(&context, "r1", "1", &Labels::new())
            .await
            .unwrap();
        gateway.0.lock().unwrap().fail_next_upload_after = Some(4);
        let mut sink = adapter
            .begin_upload(
                &context,
                InitialUpload {
                    release: "r1".into(),
                    file_name: "b".into(),
                    content_type: String::new(),
                    tag_condition: String::new(),
                    content_id: "id-2".into(),
                },
            )
            .await
            .unwrap();
        let mut failed = None;
        for _ in 0..50 {
            if let Err(report) = sink.send(vec![0; 1024]).await {
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

    #[tokio::test]
    async fn deployments_progress_and_refuse_a_second_one_per_device() {
        let (_, context) = TestGateway::serve().await;
        let adapter = OtaGatewayAdapterImpl;
        adapter
            .create_release(&context, "r1", "1", &Labels::new())
            .await
            .unwrap();
        assert_eq!(
            adapter
                .list_devices(&context, &Labels::from([("board".into(), "rp5".into())]))
                .await
                .unwrap(),
            ["BROKEN", "DEV1", "DEV2"]
        );

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
}
