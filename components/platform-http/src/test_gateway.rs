//! A fake platform gateway, in process, for tests -- its REST API and the
//! pieces a test's own small fake needs ([`rpc_error`], [`text_error`],
//! [`accept_channel`]).
//!
//! [`TestGateway`] holds releases and resumable uploads in memory, a fixed
//! set of labelled devices, factory devices, and deployments that move
//! PENDING → RUNNING → SUCCEEDED as they're polled (FAILED for a device
//! named `BROKEN`), reporting progress as log entries on the way, and
//! answers IAM's `whoami` calls for any token (until `revoked`). A
//! deployment's log WebSocket moves it on by itself, a step at a time.
//! Like the platform, it takes an upload in requests that each store their
//! bytes as they arrive (one at a time per content id, at the offset the
//! client says), and commits it on request only if it has the announced
//! length and checksum.
//! Never built into a shipped binary (the `test-gateway` feature is only
//! enabled from `[dev-dependencies]`).

// Handlers answer `Result<_, Response>`, axum's own idiom for a refusal.
#![allow(clippy::result_large_err)]

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

pub use axum;
use axum::{
    body::Body,
    extract::{
        ws::{CloseFrame, Message as WsMessage, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path, RawQuery, State as Shared,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use remora_context::model::{
    Context, Credentials, Endpoint, ResolvedContext, Secret, Selection, Tls,
};

use crate::{
    model::{self, artifact_upload, channel, Labels, ResourceReference},
    Code,
};

/// The account every token authenticates into: the manufacturer.
pub const TENANT: &str = "6f1c2a52-6a8e-4c39-9d7e-3c5f0b1e2a10";

/// An error the way the generated (grpc-gateway) routes answer it.
pub fn rpc_error(code: Code, message: impl Into<String>) -> Response {
    let body = serde_json::json!({
        "code": code.number(),
        "message": message.into(),
        "details": [],
    });
    (http_status(code), Json(body)).into_response()
}

/// An error the way the hand-written routes (artifacts, WebSockets)
/// answer it: plain text.
pub fn text_error(code: Code, message: impl Into<String>) -> Response {
    (http_status(code), format!("{}\n", message.into())).into_response()
}

fn http_status(code: Code) -> StatusCode {
    StatusCode::from_u16(code.http_status()).expect("a valid status")
}

/// A request header, as text.
pub fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// A query string's `<field>[key]=value` pairs: how a REST route takes a
/// map, `releaseLabels[board]=rp5`.
pub fn query_labels(query: Option<&str>, field: &str) -> Labels {
    let prefix = format!("{field}[");
    url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .filter_map(|(key, value)| {
            let key = key.strip_prefix(&prefix)?.strip_suffix(']')?.to_owned();
            Some((key, value.into_owned()))
        })
        .collect()
}

/// A query string's `field=value`.
pub fn query_value(query: Option<&str>, field: &str) -> Option<String> {
    url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .find(|(key, _)| key == field)
        .map(|(_, value)| value.into_owned())
}

/// Accepts a device channel, as the gateway does once the device said yes:
/// the upgrade answers its kind and the device's URN, then `device` plays
/// the device on the socket -- binary messages for bytes, a text
/// [`channel::EOF`] for a half-close, a close frame to end it.
pub fn accept_channel<F, Fut>(
    upgrade: WebSocketUpgrade,
    kind: &str,
    urn: &str,
    device: F,
) -> Response
where
    F: FnOnce(WebSocket) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut response = upgrade
        .protocols([channel::SUBPROTOCOL])
        .on_upgrade(device)
        .into_response();
    let headers = response.headers_mut();
    for (name, value) in [(channel::KIND_HEADER, kind), (channel::DEVICE_HEADER, urn)] {
        if let Ok(value) = HeaderValue::from_str(value) {
            headers.insert(name, value);
        }
    }
    response
}

/// Serves a router on a free local port; returns its `host:port`.
pub async fn serve_router(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move { axum::serve(listener, router).await });
    address
}

/// A context for a gateway at `address`: plaintext, an access key.
pub fn context(address: String, token: &str) -> ResolvedContext {
    ResolvedContext {
        context: Context {
            name: "test".into(),
            description: None,
            endpoint: Endpoint {
                address,
                tls: Tls {
                    disabled: true,
                    ..Tls::default()
                },
            },
            roles: Default::default(),
            assumed_role: None,
            login: None,
        },
        credentials: Credentials {
            secret: Secret::AccessKey {
                token: token.into(),
            },
        },
        selection: Selection::Flag,
        role: None,
    }
}

fn reference(name: &str) -> Option<ResourceReference> {
    Some(ResourceReference {
        name: name.to_owned(),
        urn: format!("urn:test:{name}"),
        ..Default::default()
    })
}

struct StoredDeployment {
    release: String,
    target: String,
    labels: Labels,
    status: String,
    polls: u32,
    logs: Vec<model::DeploymentLogEntry>,
    /// Log WebSockets open on it: they move it on, polls then don't.
    followers: usize,
}

impl StoredDeployment {
    /// One step of a running deployment: 50 %, 100 %, then done -- failed
    /// for a device named BROKEN; a cancellation lands.
    fn advance(&mut self) {
        if self.status == "RUNNING" {
            self.polls += 1;
            let progress = |current| model::DeploymentLogEntry {
                execution: "proceeding".into(),
                details: vec![format!("installing {current}%")],
                progress: Some(model::DeploymentLogEntryProgress {
                    current,
                    max: 100,
                    unit: "%".into(),
                    speed: 0.0,
                }),
                ..Default::default()
            };
            match self.polls {
                1 => self.logs.push(progress(50)),
                2 => self.logs.push(progress(100)),
                _ => {
                    let failed = self.target == "BROKEN";
                    self.status = if failed { "FAILED" } else { "SUCCEEDED" }.into();
                    self.logs.push(model::DeploymentLogEntry {
                        execution: "closed".into(),
                        result: if failed { "failure" } else { "success" }.into(),
                        details: vec![if failed {
                            "bundle signature invalid"
                        } else {
                            "installed, rebooting"
                        }
                        .into()],
                        ..Default::default()
                    });
                }
            }
        }
        if self.status == "CANCELING" {
            self.status = "CANCELED".into();
        }
    }

    fn details(&self) -> Labels {
        if self.status == "FAILED" {
            Labels::from([("reason".to_owned(), "bundle signature invalid".to_owned())])
        } else {
            Labels::new()
        }
    }
}

/// When a deployment last changed, for every one of them.
const STATUS_TIMESTAMP: &str = "2026-09-21T14:13:20Z";

/// How often a deployment's log WebSocket moves it on.
const LOG_STREAM_STEP: Duration = Duration::from_millis(20);

/// What the fake holds; inspect it from a test.
#[derive(Default)]
pub struct State {
    pub releases: BTreeMap<String, model::ReleaseDescriptor>,
    /// Stored bytes per content id, committed or not.
    uploads: HashMap<String, Vec<u8>>,
    /// Bytes upload requests carried in, stored or not: what a test waits
    /// on to know an upload is under way.
    pub received_upload_bytes: usize,
    /// The next upload request fails after storing this many bytes, once:
    /// storage gone mid-way, with what was stored kept for a resume.
    pub fail_next_upload_after: Option<usize>,
    /// How long each received piece of an upload takes to store: a slow
    /// link, so that a test can interrupt an upload while it runs.
    pub upload_chunk_delay: Option<Duration>,
    /// Upload requests whose body didn't end cleanly (failed, cut short by
    /// the client, or the client gone).
    pub abandoned_uploads: usize,
    /// Upload requests that started writing.
    pub upload_requests: usize,
    /// Content ids being written by a request: one at a time.
    writing: HashSet<String>,
    /// Content ids committed: never written again.
    committed_uploads: HashSet<String>,
    /// The next download fails after sending this many bytes, once: a
    /// dropped connection.
    pub fail_next_download_after: Option<usize>,
    /// Every download, as `(file name, offset)`.
    pub downloads: Vec<(String, u64)>,
    /// The next deployment log WebSocket fails after sending this many
    /// frames, once: the gateway losing its event feed.
    pub fail_next_log_stream_after: Option<usize>,
    /// Every deployment log WebSocket opened, by deployment.
    pub log_streams: Vec<String>,
    pub devices: BTreeMap<String, Labels>,
    deployments: BTreeMap<String, StoredDeployment>,
    /// Manufactured serials, with how many times each was signed.
    pub factory_devices: BTreeMap<String, u32>,
    /// Assigned serials, with the account each was last assigned to, as
    /// asked (an id or a URN).
    pub factory_owners: BTreeMap<String, String>,
    /// Every call is refused as unauthenticated, as for credentials
    /// revoked after login.
    pub revoked: bool,
}

impl State {
    /// How many bytes are stored for a content id, committed or not.
    pub fn upload_offset(&self, content_id: &str) -> usize {
        self.uploads.get(content_id).map_or(0, Vec::len)
    }
}

#[derive(Clone)]
pub struct TestGateway(pub Arc<Mutex<State>>);

type Answer<T> = Result<Json<T>, Response>;

impl TestGateway {
    /// Serves on a free local port; returns the gateway and a context
    /// pointing at it (plaintext, any token).
    pub async fn serve() -> (Self, ResolvedContext) {
        let gateway = Self(Arc::new(Mutex::new(State::default())));
        {
            let mut state = gateway.state();
            for (name, board) in [
                ("DEV1", "rp5"),
                ("DEV2", "rp5"),
                ("DEV3", "hdc"),
                ("BROKEN", "rp5"),
            ] {
                state
                    .devices
                    .insert(name.into(), Labels::from([("board".into(), board.into())]));
            }
        }
        let address = serve_router(gateway.router()).await;
        (gateway, context(address, "t"))
    }

    /// Its routes, to serve or to merge with a test's own.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/iam/v1/current-user", get(current_user))
            .route("/iam/v1/current-account", get(current_account))
            .route(
                "/remora/v1/release",
                get(list_releases).post(create_release),
            )
            .route(
                "/remora/v1/release/{release_name}",
                get(get_release)
                    .patch(update_release)
                    .delete(delete_release),
            )
            // A request carries as much as the client sends: no cap.
            .route(
                "/remora/v1/release/{release_name}/artifact/{artifact_file_name}/{artifact_content_id}",
                put(upload_put)
                    .head(upload_head)
                    .post(upload_commit)
                    .layer(DefaultBodyLimit::disable()),
            )
            .route(
                "/remora/v1/release/{release_name}/artifact/{artifact_file_name}",
                get(download_artifact),
            )
            .route(
                "/remora/v1/release-artifact-upload-offset/{artifact_content_id}",
                get(upload_offset),
            )
            .route("/remora/v1/device", get(list_devices))
            .route("/remora/v1/device/{device_name}", get(get_device))
            .route("/remora/v1/factory-device", post(create_factory_device))
            .route(
                "/remora/v1/factory-device/{device_name}/assignment",
                post(assign_factory_device),
            )
            .route(
                "/remora/v1/deployment",
                get(list_deployments).post(create_deployment),
            )
            .route(
                "/remora/v1/deployment/{deployment_name}",
                get(get_deployment).delete(delete_deployment),
            )
            .route(
                "/remora/v1/deployment/{deployment_name}/start",
                post(start_deployment),
            )
            .route(
                "/remora/v1/deployment/{deployment_name}/cancel",
                post(cancel_deployment),
            )
            .route(
                "/remora/v1/deployment/{deployment_name}/log",
                get(deployment_log),
            )
            .route(
                "/remora/v1/deployment/{deployment_name}/log/stream",
                get(deployment_log_stream),
            )
            .with_state(self.clone())
    }

    /// The bytes an artifact of `release` was uploaded with.
    pub fn artifact_bytes(&self, release: &str, file_name: &str) -> Option<Vec<u8>> {
        let state = self.state();
        let descriptor = state.releases.get(release)?;
        let artifact = descriptor
            .artifacts
            .iter()
            .find(|a| a.file_name == file_name)?;
        let id = artifact.object_reference.as_ref()?.id.clone();
        state.uploads.get(&id).cloned()
    }

    /// Forces a deployment's status, e.g. one this version doesn't know.
    pub fn set_deployment_status(&self, name: &str, status: &str) {
        if let Some(deployment) = self.state().deployments.get_mut(name) {
            deployment.status = status.to_owned();
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.0.lock().unwrap()
    }

    fn authenticated(&self) -> Result<(), Response> {
        if self.state().revoked {
            return Err(rpc_error(Code::Unauthenticated, "invalid access key"));
        }
        Ok(())
    }
}

fn not_found(what: &str) -> Response {
    rpc_error(Code::NotFound, format!("the {what} has not been found"))
}

fn terminal(status: &str) -> bool {
    matches!(
        status,
        "SUCCEEDED" | "FAILED" | "CANCELED" | "REJECTED" | "TIMED_OUT"
    )
}

// --- iam ---------------------------------------------------------------

/// Whoever the token, the same user -- unless `revoked`.
async fn current_user(
    Shared(gateway): Shared<TestGateway>,
) -> Answer<model::GetCurrentUserResponse> {
    gateway.authenticated()?;
    Ok(Json(model::GetCurrentUserResponse {
        user_descriptor: Some(model::UserDescriptor {
            resource: Some(ResourceReference {
                id: "1".into(),
                urn: "urn:test:user:factory".into(),
                name: "factory".into(),
            }),
            ..Default::default()
        }),
    }))
}

async fn current_account(
    Shared(gateway): Shared<TestGateway>,
) -> Answer<model::GetCurrentAccountResponse> {
    gateway.authenticated()?;
    Ok(Json(model::GetCurrentAccountResponse {
        account_descriptor: Some(model::AccountDescriptor {
            resource: reference("test"),
            ..Default::default()
        }),
    }))
}

// --- releases ----------------------------------------------------------

async fn create_release(
    Shared(gateway): Shared<TestGateway>,
    Json(request): Json<model::CreateReleaseRequest>,
) -> Answer<model::ReleaseReferenceResponse> {
    if request.release_name.is_empty() || request.release_version.is_empty() {
        return Err(rpc_error(
            Code::InvalidArgument,
            "release-name: cannot be blank; release-version: cannot be blank.",
        ));
    }
    let mut state = gateway.state();
    if state.releases.contains_key(&request.release_name) {
        return Err(rpc_error(
            Code::AlreadyExists,
            "a release with the same name already exists",
        ));
    }
    state.releases.insert(
        request.release_name.clone(),
        model::ReleaseDescriptor {
            resource: reference(&request.release_name),
            labels: request.release_labels,
            version: request.release_version,
            artifacts: vec![],
        },
    );
    Ok(Json(model::ReleaseReferenceResponse {
        release_reference: reference(&request.release_name),
    }))
}

async fn get_release(
    Shared(gateway): Shared<TestGateway>,
    Path(name): Path<String>,
) -> Answer<model::GetReleaseResponse> {
    let descriptor = gateway
        .state()
        .releases
        .get(&name)
        .cloned()
        .ok_or_else(|| not_found("release"))?;
    Ok(Json(model::GetReleaseResponse {
        release_descriptor: Some(descriptor),
    }))
}

async fn update_release(
    Shared(gateway): Shared<TestGateway>,
    Path(name): Path<String>,
    Json(request): Json<model::UpdateReleaseRequest>,
) -> Answer<model::ReleaseReferenceResponse> {
    let mut state = gateway.state();
    let release = state
        .releases
        .get_mut(&name)
        .ok_or_else(|| not_found("release"))?;
    if let Some(labels) = request.release_labels {
        if !labels.merge {
            release.labels.clear();
        }
        release.labels.extend(labels.set);
        for key in labels.unset {
            release.labels.remove(&key);
        }
    }
    Ok(Json(model::ReleaseReferenceResponse {
        release_reference: reference(&name),
    }))
}

async fn list_releases(
    Shared(gateway): Shared<TestGateway>,
    RawQuery(query): RawQuery,
) -> Answer<model::ListReleasesResponse> {
    let wanted = query_labels(query.as_deref(), "releaseLabels");
    let summaries = gateway
        .state()
        .releases
        .values()
        .filter(|r| wanted.iter().all(|(k, v)| r.labels.get(k) == Some(v)))
        .map(|r| model::ReleaseSummary {
            resource: r.resource.clone(),
            version: r.version.clone(),
        })
        .collect();
    Ok(Json(model::ListReleasesResponse {
        release_summaries: summaries,
    }))
}

async fn delete_release(
    Shared(gateway): Shared<TestGateway>,
    Path(name): Path<String>,
) -> Answer<model::ReleaseReferenceResponse> {
    gateway
        .state()
        .releases
        .remove(&name)
        .ok_or_else(|| not_found("release"))?;
    Ok(Json(model::ReleaseReferenceResponse {
        release_reference: reference(&name),
    }))
}

async fn upload_offset(
    Shared(gateway): Shared<TestGateway>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
) -> Answer<model::UploadOffsetResponse> {
    // As the platform: an upload's offset is read within its release.
    if query_value(query.as_deref(), "release_name").is_none_or(|name| name.is_empty()) {
        return Err(rpc_error(
            Code::InvalidArgument,
            "release-name: cannot be blank.",
        ));
    }
    let offset = gateway.state().upload_offset(&id);
    Ok(Json(model::UploadOffsetResponse {
        artifact_content_offset: offset as i64,
    }))
}

/// The platform's rules for an upload's resource: a UUID content id, and a
/// file name of one segment of `[A-Za-z0-9._+~@-]`, not starting with a
/// dot.
fn upload_refused(file_name: &str, content_id: &str) -> Option<Response> {
    let uuid = content_id.len() == 36
        && content_id.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    if !uuid {
        return Some(text_error(
            Code::InvalidArgument,
            "the content id must be a UUID",
        ));
    }
    let name_ok = !file_name.is_empty()
        && !file_name.starts_with('.')
        && file_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '~' | '@' | '-'));
    (!name_ok).then(|| text_error(Code::InvalidArgument, "an invalid artifact file name"))
}

/// Counts an upload request as abandoned unless its body ended cleanly --
/// even when the server drops the handler outright, the client gone -- and
/// lets the next writer of its content id in once it is over.
struct UploadGuard {
    gateway: TestGateway,
    content_id: String,
    complete: bool,
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        let mut state = self.gateway.state();
        state.writing.remove(&self.content_id);
        if !self.complete {
            state.abandoned_uploads += 1;
        }
    }
}

/// A response carrying how many bytes a content id holds.
fn with_offset(mut response: Response, offset: usize) -> Response {
    response
        .headers_mut()
        .insert(artifact_upload::OFFSET_HEADER, HeaderValue::from(offset));
    response
}

/// The platform's `HEAD`: how far an upload got.
async fn upload_head(
    Shared(gateway): Shared<TestGateway>,
    Path((_release, file_name, content_id)): Path<(String, String, String)>,
) -> Response {
    if let Some(refused) = upload_refused(&file_name, &content_id) {
        return refused;
    }
    let offset = gateway.state().upload_offset(&content_id);
    with_offset(StatusCode::OK.into_response(), offset)
}

/// The platform's `PUT`: the body stored as it arrives after what the
/// content id holds, if that is the offset the client says; never
/// committed.
async fn upload_put(
    Shared(gateway): Shared<TestGateway>,
    Path((release, file_name, content_id)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Some(refused) = upload_refused(&file_name, &content_id) {
        return refused;
    }
    let Some(offset) = header(&headers, artifact_upload::OFFSET_HEADER)
        .and_then(|offset| offset.parse::<usize>().ok())
    else {
        return text_error(Code::InvalidArgument, "a missing or invalid Upload-Offset");
    };
    let (fail_after, delay) = {
        let mut state = gateway.state();
        if !state.releases.contains_key(&release) {
            return text_error(Code::NotFound, "the release has not been found");
        }
        let stored = state.upload_offset(&content_id);
        if state.committed_uploads.contains(&content_id) {
            return with_offset(
                (StatusCode::GONE, "the upload is committed\n").into_response(),
                stored,
            );
        }
        if state.writing.contains(&content_id) {
            return with_offset(
                (StatusCode::LOCKED, "an earlier request still writes\n").into_response(),
                stored,
            );
        }
        if offset != stored {
            return with_offset(
                (StatusCode::CONFLICT, "the offset is not what is stored\n").into_response(),
                stored,
            );
        }
        state.writing.insert(content_id.clone());
        state.upload_requests += 1;
        (
            state.fail_next_upload_after.take(),
            state.upload_chunk_delay,
        )
    };

    let mut guard = UploadGuard {
        gateway: gateway.clone(),
        content_id: content_id.clone(),
        complete: false,
    };
    let mut stored_here = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
        // Cut short: what arrived stays.
        let Ok(chunk) = chunk else {
            return text_error(Code::InvalidArgument, "the body was cut short");
        };
        {
            let mut state = gateway.state();
            state.received_upload_bytes += chunk.len();
            let stored = state.uploads.entry(content_id.clone()).or_default();
            if let Some(limit) = fail_after {
                let room = limit.saturating_sub(stored_here);
                if chunk.len() > room {
                    stored.extend_from_slice(&chunk[..room]);
                    return text_error(Code::Unavailable, "connection reset");
                }
            }
            stored.extend_from_slice(&chunk);
            stored_here += chunk.len();
        }
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
    }
    guard.complete = true;
    let offset = gateway.state().upload_offset(&content_id);
    with_offset(StatusCode::NO_CONTENT.into_response(), offset)
}

/// The platform's commit: the artifact, if what the content id holds has
/// the announced length and checksum -- else what it holds is dropped.
async fn upload_commit(
    Shared(gateway): Shared<TestGateway>,
    Path((release, file_name, content_id)): Path<(String, String, String)>,
    Json(request): Json<artifact_upload::CommitRequest>,
) -> Answer<artifact_upload::CommitResponse> {
    use sha2::Digest;
    if let Some(refused) = upload_refused(&file_name, &content_id) {
        return Err(refused);
    }
    let mut state = gateway.state();
    if !state.releases.contains_key(&release) {
        return Err(not_found("release"));
    }
    if state.committed_uploads.contains(&content_id) {
        return Err(rpc_error(
            Code::AlreadyExists,
            "the upload is already committed",
        ));
    }
    if state.writing.contains(&content_id) {
        return Err(rpc_error(
            Code::FailedPrecondition,
            "an earlier request still writes",
        ));
    }
    let stored = state.uploads.get(&content_id).cloned().unwrap_or_default();
    let checksum: String = sha2::Sha256::digest(&stored)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if stored.len() as i64 != request.artifact_content_length
        || !checksum.eq_ignore_ascii_case(&request.artifact_content_checksum)
    {
        state.uploads.remove(&content_id);
        return Err(rpc_error(
            Code::InvalidArgument,
            format!(
                "the artifact does not match: {} bytes with sha256 {checksum}",
                stored.len()
            ),
        ));
    }
    if request.artifact_content_type.is_empty() {
        return Err(rpc_error(
            Code::InvalidArgument,
            "artifact-content-type: cannot be blank.",
        ));
    }
    state.committed_uploads.insert(content_id.clone());
    let descriptor = state.releases.get_mut(&release).expect("checked above");
    descriptor.artifacts.retain(|a| a.file_name != file_name);
    descriptor.artifacts.push(model::ReleaseArtifact {
        object_reference: Some(ResourceReference {
            id: content_id,
            ..Default::default()
        }),
        file_name,
        content_type: request.artifact_content_type,
        content_length: request.artifact_content_length,
        checksum_sha256: checksum,
        tag_condition: request.artifact_tag_condition,
    });
    Ok(Json(artifact_upload::CommitResponse {
        release_reference: reference(&release),
    }))
}

/// How the fake streams a download: small pieces, so a test sees several.
const DOWNLOAD_CHUNK: usize = 64 * 1024;

/// The platform's download: a single `bytes=N-` range or the whole file.
async fn download_artifact(
    Shared(gateway): Shared<TestGateway>,
    Path((release, file_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(bytes) = gateway.artifact_bytes(&release, &file_name) else {
        return text_error(Code::NotFound, "artifact not found in release");
    };
    let total = bytes.len();
    let offset = match header(&headers, "range") {
        None => None,
        Some(range) => match range
            .strip_prefix("bytes=")
            .and_then(|range| range.strip_suffix('-'))
            .and_then(|first| first.parse::<usize>().ok())
        {
            Some(first) if first < total => Some(first),
            _ => {
                let mut response = text_error(Code::OutOfRange, "range not satisfiable");
                *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                response.headers_mut().insert(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(&format!("bytes */{total}")).unwrap(),
                );
                return response;
            }
        },
    };
    let fail_after = {
        let mut state = gateway.state();
        state
            .downloads
            .push((file_name.clone(), offset.unwrap_or(0) as u64));
        state.fail_next_download_after.take()
    };
    let start = offset.unwrap_or(0);
    let range = bytes[start..].to_vec();
    let length = range.len();
    let mut pieces: Vec<Result<Vec<u8>, std::io::Error>> = Vec::new();
    let mut sent = 0;
    for piece in range.chunks(DOWNLOAD_CHUNK) {
        if let Some(limit) = fail_after {
            if sent + piece.len() > limit {
                pieces.push(Ok(piece[..limit - sent].to_vec()));
                pieces.push(Err(std::io::Error::other("connection reset")));
                break;
            }
        }
        sent += piece.len();
        pieces.push(Ok(piece.to_vec()));
    }
    let body = Body::from_stream(futures_util::stream::iter(pieces));
    let mut response = Response::new(body);
    let headers = response.headers_mut();
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    if offset.is_some() {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
        response.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{}/{total}", total - 1)).unwrap(),
        );
    }
    response
}

// --- devices -----------------------------------------------------------

async fn get_device(
    Shared(gateway): Shared<TestGateway>,
    Path(name): Path<String>,
) -> Answer<model::GetDeviceResponse> {
    let labels = gateway
        .state()
        .devices
        .get(&name)
        .cloned()
        .ok_or_else(|| not_found("device"))?;
    Ok(Json(model::GetDeviceResponse {
        device_descriptor: Some(model::DeviceDescriptor {
            resource: reference(&name),
            labels,
            certificates: vec![],
        }),
    }))
}

async fn list_devices(
    Shared(gateway): Shared<TestGateway>,
    RawQuery(query): RawQuery,
) -> Answer<model::ListDevicesResponse> {
    let wanted = query_labels(query.as_deref(), "deviceLabels");
    let summaries = gateway
        .state()
        .devices
        .iter()
        .filter(|(_, labels)| wanted.iter().all(|(k, v)| labels.get(k) == Some(v)))
        .map(|(name, _)| model::DeviceSummary {
            resource: reference(name),
        })
        .collect();
    Ok(Json(model::ListDevicesResponse {
        device_summaries: summaries,
    }))
}

/// Manufactures like the platform: a policy allocates `<policy>-<n>`, an
/// existing name is refused unless `force`, which re-signs it.
async fn create_factory_device(
    Shared(gateway): Shared<TestGateway>,
    Json(request): Json<model::CreateFactoryDeviceRequest>,
) -> Answer<model::CreateFactoryDeviceResponse> {
    gateway.authenticated()?;
    let mut state = gateway.state();
    let serial = match (
        request.device_name.is_empty(),
        request.serial_number_policy_name.is_empty(),
    ) {
        (false, true) => request.device_name,
        (true, false) if !request.force => format!(
            "{}-{}",
            request.serial_number_policy_name,
            state.factory_devices.len() + 1
        ),
        _ => {
            return Err(rpc_error(
                Code::InvalidArgument,
                "device_name xor policy, force only with a name",
            ))
        }
    };
    let signed = state.factory_devices.entry(serial.clone()).or_default();
    if *signed > 0 && !request.force {
        return Err(rpc_error(
            Code::AlreadyExists,
            "a device already has this name",
        ));
    }
    *signed += 1;
    Ok(Json(model::CreateFactoryDeviceResponse {
        // As the platform renders it: no `urn:` prefix.
        factory_device_name: format!("sntns:remora:test:{TENANT}:factory-device:{serial}"),
        certificate_reference: reference(&format!("certificate-{serial}-{signed}")),
        certificate: format!("idevid {serial}").into_bytes(),
        certificate_authority_certificate: b"factory ca".to_vec(),
        server_certificate_authority_certificate: b"server ca".to_vec(),
        access_url: "https://access.test/access/v1".into(),
        serial_number: serial,
    }))
}

/// Assigns like the platform: a serial this account made, to an account
/// id or URN.
async fn assign_factory_device(
    Shared(gateway): Shared<TestGateway>,
    Path(device_name): Path<String>,
    Json(request): Json<model::AssignFactoryDeviceRequest>,
) -> Answer<model::AssignFactoryDeviceResponse> {
    gateway.authenticated()?;
    let mut state = gateway.state();
    if !state.factory_devices.contains_key(&device_name) {
        return Err(rpc_error(Code::NotFound, "no such device was manufactured"));
    }
    let owner = &request.owner_account;
    let urn = match owner {
        owner if owner.starts_with("urn:") => owner.clone(),
        owner
            if !owner.is_empty()
                && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') =>
        {
            format!("urn:sntns:iam:test:{owner}:account")
        }
        _ => {
            return Err(rpc_error(
                Code::InvalidArgument,
                "owner-account must be an account id or URN",
            ))
        }
    };
    state
        .factory_owners
        .insert(device_name.clone(), owner.clone());
    Ok(Json(model::AssignFactoryDeviceResponse {
        factory_device_reference: reference(&format!("factory-device-{device_name}")),
        owner_reference: Some(ResourceReference {
            urn,
            ..Default::default()
        }),
        voucher: format!("voucher {device_name}").into_bytes(),
    }))
}

// --- deployments -------------------------------------------------------

async fn create_deployment(
    Shared(gateway): Shared<TestGateway>,
    Json(request): Json<model::CreateDeploymentRequest>,
) -> Answer<model::DeploymentReferenceResponse> {
    let mut state = gateway.state();
    if !state
        .releases
        .contains_key(&request.deployment_release_name)
    {
        return Err(not_found("release"));
    }
    if !state.devices.contains_key(&request.deployment_target_name) {
        return Err(not_found("device"));
    }
    if state.deployments.contains_key(&request.deployment_name) {
        return Err(rpc_error(
            Code::FailedPrecondition,
            "a deployment with the same name already exists",
        ));
    }
    if state
        .deployments
        .values()
        .any(|d| d.target == request.deployment_target_name && !terminal(&d.status))
    {
        return Err(rpc_error(
            Code::FailedPrecondition,
            "the target device already has a non-terminal deployment",
        ));
    }
    state.deployments.insert(
        request.deployment_name.clone(),
        StoredDeployment {
            release: request.deployment_release_name,
            target: request.deployment_target_name,
            labels: request.deployment_labels,
            status: "PENDING".into(),
            polls: 0,
            logs: vec![],
            followers: 0,
        },
    );
    Ok(Json(model::DeploymentReferenceResponse {
        deployment_reference: reference(&request.deployment_name),
    }))
}

/// Every poll of a running deployment moves it on (see
/// [`StoredDeployment::advance`]) -- unless a log WebSocket follows it,
/// which does.
async fn get_deployment(
    Shared(gateway): Shared<TestGateway>,
    Path(name): Path<String>,
) -> Answer<model::GetDeploymentResponse> {
    let mut state = gateway.state();
    let deployment = state
        .deployments
        .get_mut(&name)
        .ok_or_else(|| not_found("deployment"))?;
    if deployment.followers == 0 {
        deployment.advance();
    }
    Ok(Json(model::GetDeploymentResponse {
        deployment_descriptor: Some(model::DeploymentDescriptor {
            resource: reference(&name),
            labels: deployment.labels.clone(),
            release_reference: reference(&deployment.release),
            target_reference: reference(&deployment.target),
            status_value: deployment.status.clone(),
            status_details: deployment.details(),
            status_timestamp: Some(STATUS_TIMESTAMP.into()),
        }),
    }))
}

async fn list_deployments(
    Shared(gateway): Shared<TestGateway>,
    RawQuery(query): RawQuery,
) -> Answer<model::ListDeploymentsResponse> {
    let query = query.as_deref();
    let target = query_value(query, "deploymentTargetName").unwrap_or_default();
    let release = query_value(query, "deploymentReleaseName").unwrap_or_default();
    let labels = query_labels(query, "deploymentLabels");
    let summaries = gateway
        .state()
        .deployments
        .iter()
        .filter(|(_, d)| {
            (target.is_empty() || d.target == target)
                && (release.is_empty() || d.release == release)
                && labels.iter().all(|(k, v)| d.labels.get(k) == Some(v))
        })
        .map(|(name, d)| model::DeploymentSummary {
            resource: reference(name),
            status_value: d.status.clone(),
        })
        .collect();
    Ok(Json(model::ListDeploymentsResponse {
        deployment_summaries: summaries,
    }))
}

async fn start_deployment(
    Shared(gateway): Shared<TestGateway>,
    Path(name): Path<String>,
) -> Answer<model::DeploymentReferenceResponse> {
    let mut state = gateway.state();
    let deployment = state
        .deployments
        .get_mut(&name)
        .ok_or_else(|| not_found("deployment"))?;
    if deployment.status != "PENDING" || deployment.polls != 0 || !deployment.logs.is_empty() {
        return Err(rpc_error(
            Code::FailedPrecondition,
            "it is not in a draft state",
        ));
    }
    deployment.status = "RUNNING".into();
    Ok(Json(model::DeploymentReferenceResponse {
        deployment_reference: reference(&name),
    }))
}

async fn cancel_deployment(
    Shared(gateway): Shared<TestGateway>,
    Path(name): Path<String>,
) -> Answer<model::DeploymentReferenceResponse> {
    let mut state = gateway.state();
    let deployment = state
        .deployments
        .get_mut(&name)
        .ok_or_else(|| not_found("deployment"))?;
    if terminal(&deployment.status) {
        return Err(rpc_error(
            Code::FailedPrecondition,
            "it is already in a terminal state",
        ));
    }
    deployment.status = "CANCELING".into();
    Ok(Json(model::DeploymentReferenceResponse {
        deployment_reference: reference(&name),
    }))
}

async fn delete_deployment(
    Shared(gateway): Shared<TestGateway>,
    Path(name): Path<String>,
) -> Answer<model::DeploymentReferenceResponse> {
    gateway
        .state()
        .deployments
        .remove(&name)
        .ok_or_else(|| not_found("deployment"))?;
    Ok(Json(model::DeploymentReferenceResponse {
        deployment_reference: reference(&name),
    }))
}

async fn deployment_log(
    Shared(gateway): Shared<TestGateway>,
    Path(name): Path<String>,
) -> Answer<model::ListDeploymentLogEntriesResponse> {
    let logs = gateway
        .state()
        .deployments
        .get(&name)
        .map(|d| d.logs.clone())
        .ok_or_else(|| not_found("deployment"))?;
    Ok(Json(model::ListDeploymentLogEntriesResponse {
        deployment_log_entries: logs,
    }))
}

/// The platform's deployment log WebSocket: the status, then each new
/// report and status change, moving the deployment on a step every
/// [`LOG_STREAM_STEP`]; closed normally once it is terminal or deleted.
async fn deployment_log_stream(
    Shared(gateway): Shared<TestGateway>,
    Path(name): Path<String>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let (mut sent, fail_after) = {
        let mut state = gateway.state();
        let Some(deployment) = state.deployments.get_mut(&name) else {
            return text_error(Code::NotFound, "the deployment has not been found");
        };
        // Starts now: what was reported before is not replayed.
        let sent = deployment.logs.len();
        deployment.followers += 1;
        state.log_streams.push(name.clone());
        (sent, state.fail_next_log_stream_after.take())
    };
    upgrade
        .protocols([model::deployment_log::SUBPROTOCOL])
        .on_upgrade(move |mut socket| async move {
            let _following = Following {
                gateway: gateway.clone(),
                name: name.clone(),
            };
            let mut status = None;
            let mut frames = 0;
            loop {
                let step = {
                    let mut state = gateway.state();
                    state.deployments.get_mut(&name).map(|deployment| {
                        let mut outgoing: Vec<model::deployment_log::Frame> = deployment.logs
                            [sent..]
                            .iter()
                            .map(|entry| model::deployment_log::Frame::Log {
                                entry: entry.clone(),
                            })
                            .collect();
                        sent = deployment.logs.len();
                        if status.as_ref() != Some(&deployment.status) {
                            status = Some(deployment.status.clone());
                            outgoing.push(model::deployment_log::Frame::Status {
                                status_value: deployment.status.clone(),
                                status_details: deployment.details(),
                                status_timestamp: Some(STATUS_TIMESTAMP.into()),
                            });
                        }
                        let end = terminal(&deployment.status);
                        deployment.advance();
                        (outgoing, end)
                    })
                };
                let Some((outgoing, end)) = step else {
                    let _ = socket
                        .send(close_frame(1000, model::deployment_log::DELETED))
                        .await;
                    return;
                };
                for frame in outgoing {
                    if fail_after == Some(frames) {
                        let _ = socket.send(close_frame(1011, "lost the event feed")).await;
                        return;
                    }
                    frames += 1;
                    let text = serde_json::to_string(&frame).expect("a frame serializes");
                    if socket.send(WsMessage::Text(text.into())).await.is_err() {
                        return;
                    }
                }
                if end {
                    let _ = socket
                        .send(close_frame(1000, model::deployment_log::TERMINAL))
                        .await;
                    return;
                }
                tokio::time::sleep(LOG_STREAM_STEP).await;
            }
        })
}

/// A log WebSocket following a deployment, until it ends however it ends.
struct Following {
    gateway: TestGateway,
    name: String,
}

impl Drop for Following {
    fn drop(&mut self) {
        if let Some(deployment) = self.gateway.state().deployments.get_mut(&self.name) {
            deployment.followers = deployment.followers.saturating_sub(1);
        }
    }
}

fn close_frame(code: u16, reason: &str) -> WsMessage {
    WsMessage::Close(Some(CloseFrame {
        code,
        reason: reason.into(),
    }))
}
