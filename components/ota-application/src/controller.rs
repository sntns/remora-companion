use std::path::Path;

use error_stack::{FrameKind, Report, ResultExt};
use remora_context::{
    application::ContextService,
    model::{ContextOverride, ResolvedContext},
};
use remora_ota::{
    adapter::gateway::{self, InitialUpload, OtaGatewayAdapterService},
    application::{Error, OtaServiceInterface, Result},
    model::{
        DeployRequest, Deployment, DeploymentFilter, DeploymentSummary, Labels, LogEntry, Planned,
        Release, ReleaseSummary, Targets, UploadOutcome, UploadRequest,
    },
};
use remora_progress::OperationContext;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// One message per chunk: big enough to keep a fast link busy, small enough
/// that a dropped stream loses little and progress moves smoothly.
const CHUNK: usize = 1024 * 1024;

/// How many times a dropped upload is resumed before giving up (the
/// operator can still resume it later with the printed token).
const UPLOAD_ATTEMPTS: usize = 4;

/// The ota vertical's use case: the gateway port, and the context
/// vertical's application port for which platform, as whom.
pub struct OtaControllerImpl {
    contexts: ContextService,
    gateway: OtaGatewayAdapterService,
}

impl OtaControllerImpl {
    pub fn new(contexts: ContextService, gateway: OtaGatewayAdapterService) -> Self {
        Self { contexts, gateway }
    }

    async fn resolve(&self, over: Option<&ContextOverride>) -> Result<ResolvedContext> {
        self.contexts.resolve(over).await.map_err(|report| {
            let message = report.current_context().to_string();
            report.change_context(Error::Context(message))
        })
    }

    /// One attempt: from wherever the platform got to, to the end.
    async fn upload_from(
        &self,
        context: &ResolvedContext,
        initial: &InitialUpload,
        path: &Path,
        offset: u64,
        total: u64,
        ctx: &OperationContext,
    ) -> gateway::Result<()> {
        let mut file = tokio::fs::File::open(path)
            .await
            .map_err(|e| Report::new(gateway::Error::Call).attach(e.to_string()))?;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| Report::new(gateway::Error::Call).attach(e.to_string()))?;
        let mut sink = self.gateway.begin_upload(context, initial.clone()).await?;
        let mut done = offset;
        ctx.sink.progress(done, total);
        let mut buffer = vec![0; CHUNK];
        loop {
            if ctx.cancel.is_cancelled() {
                return Err(Report::new(gateway::Error::Call).attach("cancelled"));
            }
            let read = file
                .read(&mut buffer)
                .await
                .map_err(|e| Report::new(gateway::Error::Call).attach(e.to_string()))?;
            if read == 0 {
                break;
            }
            sink.send(buffer[..read].to_vec()).await?;
            done += read as u64;
            ctx.sink.progress(done, total);
        }
        sink.finish().await
    }
}

/// A fresh resume token: 128 random bits, hex.
fn content_id() -> String {
    use rand_core::RngCore;
    let mut bytes = [0u8; 16];
    rand_core::OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What a RAUC/SWUpdate client expects to be told; anything else is opaque.
fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("json") => "application/json",
        Some("gz" | "tgz") => "application/gzip",
        Some("tar") => "application/x-tar",
        _ => "application/octet-stream",
    }
}

/// Errors worth resuming after: the link, not the request.
fn retryable(report: &Report<gateway::Error>) -> bool {
    matches!(
        report.current_context(),
        gateway::Error::Unreachable | gateway::Error::Call
    ) && !format!("{report:?}").contains("cancelled")
}

/// The platform's own words for a failure, when it gave any: the last
/// printable attachment (the gRPC status), else the error itself.
fn reason<C: std::fmt::Display + Send + Sync + 'static>(report: &Report<C>) -> String {
    report
        .frames()
        .filter_map(|frame| match frame.kind() {
            FrameKind::Attachment(error_stack::AttachmentKind::Printable(p)) => Some(p.to_string()),
            _ => None,
        })
        .last()
        .unwrap_or_else(|| report.current_context().to_string())
}

impl OtaControllerImpl {
    async fn targets(&self, context: &ResolvedContext, targets: &Targets) -> Result<Vec<String>> {
        match targets {
            Targets::Devices(devices) => Ok(devices.clone()),
            Targets::Selector(labels) => {
                let devices = self
                    .gateway
                    .list_devices(context, labels)
                    .await
                    .change_context(Error::Targets)?;
                if devices.is_empty() {
                    let selector: Vec<_> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    return Err(Report::new(Error::NoTargets(selector.join(","))));
                }
                Ok(devices)
            }
        }
    }
}

#[async_trait::async_trait]
impl OtaServiceInterface for OtaControllerImpl {
    async fn resolve_targets(
        &self,
        over: Option<&ContextOverride>,
        targets: &Targets,
    ) -> Result<Vec<String>> {
        let context = self.resolve(over).await?;
        self.targets(&context, targets).await
    }

    async fn list_releases(
        &self,
        over: Option<&ContextOverride>,
        labels: &Labels,
    ) -> Result<Vec<ReleaseSummary>> {
        let context = self.resolve(over).await?;
        self.gateway
            .list_releases(&context, labels)
            .await
            .change_context(Error::ListReleases)
    }

    async fn get_release(&self, over: Option<&ContextOverride>, name: &str) -> Result<Release> {
        let context = self.resolve(over).await?;
        self.gateway
            .get_release(&context, name)
            .await
            .change_context_lazy(|| Error::GetRelease(name.to_owned()))
    }

    async fn create_release(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
        version: &str,
        labels: &Labels,
    ) -> Result<()> {
        let context = self.resolve(over).await?;
        self.gateway
            .create_release(&context, name, version, labels)
            .await
            .change_context_lazy(|| Error::CreateRelease(name.to_owned()))
    }

    async fn label_release(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
        set: &Labels,
        unset: &[String],
    ) -> Result<()> {
        let context = self.resolve(over).await?;
        self.gateway
            .update_release_labels(&context, name, set, unset)
            .await
            .change_context_lazy(|| Error::UpdateRelease(name.to_owned()))
    }

    async fn delete_release(&self, over: Option<&ContextOverride>, name: &str) -> Result<()> {
        let context = self.resolve(over).await?;
        self.gateway
            .delete_release(&context, name)
            .await
            .change_context_lazy(|| Error::DeleteRelease(name.to_owned()))
    }

    async fn upload(
        &self,
        over: Option<&ContextOverride>,
        request: UploadRequest,
        ctx: &OperationContext,
    ) -> Result<UploadOutcome> {
        let context = self.resolve(over).await?;
        let total = tokio::fs::metadata(&request.path)
            .await
            .change_context_lazy(|| Error::ReadArtifact(request.path.clone()))?
            .len();
        let file_name = match &request.file_name {
            Some(name) => name.clone(),
            None => request
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
                .ok_or_else(|| Report::new(Error::ReadArtifact(request.path.clone())))?,
        };
        let initial = InitialUpload {
            release: request.release.clone(),
            file_name: file_name.clone(),
            content_type: request
                .content_type
                .clone()
                .unwrap_or_else(|| content_type(&request.path).to_owned()),
            tag_condition: request.tag_condition.clone(),
            content_id: request.resume.clone().unwrap_or_else(content_id),
        };
        let failed = || Error::Upload {
            file: file_name.clone(),
            release: request.release.clone(),
            content_id: initial.content_id.clone(),
        };

        let mut resumed_from = None;
        let mut last_error = None;
        for attempt in 0..UPLOAD_ATTEMPTS {
            // A fresh token has nothing to resume; anything else asks the
            // platform how far it got, since a dropped stream may still
            // have persisted part of what was sent.
            let offset = if attempt == 0 && request.resume.is_none() {
                0
            } else {
                self.gateway
                    .upload_offset(&context, &initial.content_id)
                    .await
                    .change_context_lazy(failed)?
            };
            if offset > total {
                return Err(Report::new(failed()).attach(format!(
                    "the platform holds {offset} bytes for this token, more than the file's {total}: not the same file"
                )));
            }
            resumed_from.get_or_insert(offset);
            if attempt > 0 {
                ctx.sink
                    .log(format!("connection dropped, resuming at byte {offset}"));
            }
            match self
                .upload_from(&context, &initial, &request.path, offset, total, ctx)
                .await
            {
                Ok(()) => {
                    return Ok(UploadOutcome {
                        file_name,
                        content_type: initial.content_type,
                        bytes: total,
                        resumed_from: resumed_from.unwrap_or(0),
                        resumes: attempt,
                        content_id: initial.content_id,
                    })
                }
                Err(report) if ctx.cancel.is_cancelled() => {
                    return Err(report.change_context(Error::Cancelled))
                }
                Err(report) if retryable(&report) => last_error = Some(report),
                Err(report) => return Err(report.change_context(failed())),
            }
        }
        Err(last_error
            .expect("at least one attempt ran")
            .change_context(failed()))
    }

    async fn deploy(
        &self,
        over: Option<&ContextOverride>,
        request: DeployRequest,
    ) -> Result<Vec<Planned>> {
        let context = self.resolve(over).await?;
        let devices = self.targets(&context, &request.targets).await?;
        if request.name.is_some() && devices.len() != 1 {
            return Err(Report::new(Error::NameForMany));
        }

        let mut planned = Vec::with_capacity(devices.len());
        for device in devices {
            let name = request
                .name
                .clone()
                .unwrap_or_else(|| format!("{}-{device}", request.release).to_lowercase());
            let outcome = match self
                .gateway
                .create_deployment(&context, &name, &request.release, &device, &request.labels)
                .await
            {
                Err(report) => Err(reason(&report)),
                Ok(()) if !request.start => Ok(false),
                Ok(()) => match self.gateway.start_deployment(&context, &name).await {
                    Ok(()) => Ok(true),
                    Err(report) => Err(format!("created, but not started: {}", reason(&report))),
                },
            };
            planned.push(Planned {
                device,
                deployment: name,
                outcome,
            });
        }
        Ok(planned)
    }

    async fn list_deployments(
        &self,
        over: Option<&ContextOverride>,
        filter: &DeploymentFilter,
    ) -> Result<Vec<DeploymentSummary>> {
        let context = self.resolve(over).await?;
        self.gateway
            .list_deployments(&context, filter)
            .await
            .change_context(Error::ListDeployments)
    }

    async fn get_deployment(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
    ) -> Result<Deployment> {
        let context = self.resolve(over).await?;
        self.gateway
            .get_deployment(&context, name)
            .await
            .change_context_lazy(|| Error::GetDeployment(name.to_owned()))
    }

    async fn deployment_logs(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
    ) -> Result<Vec<LogEntry>> {
        let context = self.resolve(over).await?;
        self.gateway
            .deployment_logs(&context, name)
            .await
            .change_context_lazy(|| Error::GetDeployment(name.to_owned()))
    }

    async fn start_deployment(&self, over: Option<&ContextOverride>, name: &str) -> Result<()> {
        let context = self.resolve(over).await?;
        self.gateway
            .start_deployment(&context, name)
            .await
            .change_context_lazy(|| Error::StartDeployment(name.to_owned()))
    }

    async fn cancel_deployment(&self, over: Option<&ContextOverride>, name: &str) -> Result<()> {
        let context = self.resolve(over).await?;
        self.gateway
            .cancel_deployment(&context, name)
            .await
            .change_context_lazy(|| Error::CancelDeployment(name.to_owned()))
    }

    async fn delete_deployment(&self, over: Option<&ContextOverride>, name: &str) -> Result<()> {
        let context = self.resolve(over).await?;
        self.gateway
            .delete_deployment(&context, name)
            .await
            .change_context_lazy(|| Error::DeleteDeployment(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use remora_context::{
        adapter::{
            credentials::CredentialStoreAdapterService,
            platform::{self, PlatformSessionAdapter, PlatformSessionAdapterService},
            store::ContextStoreAdapterService,
        },
        application::ContextServiceInterface,
        model::{Context, Credentials, Principal},
    };
    use remora_context_adapter_file::{FileContextStoreImpl, FileCredentialStoreImpl};
    use remora_context_application::ContextControllerImpl;
    use remora_ota::adapter::gateway::OtaGatewayAdapter;
    use remora_ota::model::DeploymentStatus;
    use remora_ota_adapter_grpc::{test_gateway::TestGateway, OtaGatewayAdapterImpl};

    use super::*;

    struct Platform;

    #[async_trait::async_trait]
    impl PlatformSessionAdapter for Platform {
        async fn whoami(&self, _: &Context, _: &Credentials) -> platform::Result<Principal> {
            Ok(Principal {
                user_urn: "urn:user:ada".into(),
                user_name: "ada".into(),
                account_name: None,
            })
        }

        async fn acting_account(
            &self,
            _: &Context,
            _: &Credentials,
            _: &str,
        ) -> platform::Result<Option<String>> {
            Ok(None)
        }
    }

    /// The real gRPC adapter against the in-process fake gateway, logged in
    /// through the real context controller on a temporary store.
    async fn controller(root: &Path) -> (OtaControllerImpl, TestGateway) {
        let (gateway, resolved) = TestGateway::serve().await;
        let contexts = ContextControllerImpl::new(
            ContextStoreAdapterService::new(FileContextStoreImpl::new(root)),
            CredentialStoreAdapterService::new(FileCredentialStoreImpl::new(root)),
            PlatformSessionAdapterService::new(Platform),
        );
        contexts
            .create(
                resolved.context.clone(),
                remora_context::model::RoleOverride::Keep,
                false,
            )
            .await
            .unwrap();
        contexts
            .login(
                None,
                resolved.credentials.clone(),
                remora_context::model::RoleOverride::Keep,
            )
            .await
            .unwrap();
        (
            OtaControllerImpl::new(
                ContextService::new(contexts),
                OtaGatewayAdapterService::new(OtaGatewayAdapterImpl),
            ),
            gateway,
        )
    }

    fn bundle(dir: &Path, size: usize) -> (std::path::PathBuf, Vec<u8>) {
        let bytes: Vec<u8> = (0..size).map(|i| (i * 7 % 251) as u8).collect();
        let path = dir.join("update-rp5.raucb");
        std::fs::write(&path, &bytes).unwrap();
        (path, bytes)
    }

    fn upload(path: &Path, resume: Option<String>) -> UploadRequest {
        UploadRequest {
            release: "r1".into(),
            path: path.to_path_buf(),
            file_name: None,
            content_type: None,
            tag_condition: "board:rp5".into(),
            resume,
        }
    }

    #[tokio::test]
    async fn an_upload_resumes_by_itself_after_a_dropped_stream() {
        let dir = tempfile::tempdir().unwrap();
        let (ota, gateway) = controller(dir.path()).await;
        ota.create_release(None, "r1", "1.0.0", &Labels::new())
            .await
            .unwrap();
        let (path, bytes) = bundle(dir.path(), 3 * CHUNK + 123);
        gateway.0.lock().unwrap().fail_next_upload_after = Some(CHUNK + 10);

        let (sink, events) = remora_progress::channel();
        let mut events = events.into_inner();
        let ctx = OperationContext::new(sink, tokio_util::sync::CancellationToken::new());
        let outcome = ota.upload(None, upload(&path, None), &ctx).await.unwrap();
        drop(ctx);

        assert_eq!(outcome.file_name, "update-rp5.raucb");
        assert_eq!(outcome.bytes, bytes.len() as u64);
        assert_eq!(
            gateway.artifact_bytes("r1", "update-rp5.raucb").unwrap(),
            bytes
        );

        let mut resumed = false;
        let mut last = None;
        while let Some(event) = events.recv().await {
            match event {
                remora_progress::OperationEvent::Log(line) => {
                    resumed |= line.contains("resuming at byte")
                }
                remora_progress::OperationEvent::Progress { done, total } => {
                    last = Some((done, total))
                }
                _ => {}
            }
        }
        assert!(resumed);
        assert_eq!(outcome.resumes, 1);
        assert_eq!(last, Some((bytes.len() as u64, bytes.len() as u64)));
    }

    #[tokio::test]
    async fn a_resume_token_continues_an_earlier_upload() {
        let dir = tempfile::tempdir().unwrap();
        let (ota, gateway) = controller(dir.path()).await;
        ota.create_release(None, "r1", "1.0.0", &Labels::new())
            .await
            .unwrap();
        let (path, bytes) = bundle(dir.path(), 2 * CHUNK);

        // An earlier run (another process) that died part-way through.
        let resolved = ota.resolve(None).await.unwrap();
        gateway.0.lock().unwrap().fail_next_upload_after = Some(CHUNK / 2);
        let mut sink = OtaGatewayAdapterImpl
            .begin_upload(
                &resolved,
                InitialUpload {
                    release: "r1".into(),
                    file_name: "update-rp5.raucb".into(),
                    content_type: "application/octet-stream".into(),
                    tag_condition: "board:rp5".into(),
                    content_id: "token-1".into(),
                },
            )
            .await
            .unwrap();
        let mut sent = 0;
        while sent < bytes.len() {
            let end = (sent + CHUNK).min(bytes.len());
            if sink.send(bytes[sent..end].to_vec()).await.is_err() {
                break;
            }
            sent = end;
        }
        // Wait for that call to end for good, as a process dying would.
        assert!(sink.finish().await.is_err());

        let outcome = ota
            .upload(
                None,
                upload(&path, Some("token-1".into())),
                &OperationContext::noop(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.content_id, "token-1");
        assert_eq!(outcome.resumed_from, (CHUNK / 2) as u64);
        assert_eq!(
            gateway.artifact_bytes("r1", "update-rp5.raucb").unwrap(),
            bytes
        );
    }

    #[tokio::test]
    async fn deploys_to_a_selector_and_keeps_going_past_a_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let (ota, _) = controller(dir.path()).await;
        ota.create_release(None, "R1", "1.0.0", &Labels::new())
            .await
            .unwrap();

        // DEV1 already has an update in flight.
        let first = ota
            .deploy(
                None,
                DeployRequest {
                    release: "R1".into(),
                    targets: Targets::Devices(vec!["DEV1".into()]),
                    name: Some("hotfix".into()),
                    labels: Labels::new(),
                    start: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(first[0].deployment, "hotfix");
        assert_eq!(first[0].outcome, Ok(false));

        let planned = ota
            .deploy(
                None,
                DeployRequest {
                    release: "R1".into(),
                    targets: Targets::Selector(Labels::from([("board".into(), "rp5".into())])),
                    name: None,
                    labels: Labels::new(),
                    start: true,
                },
            )
            .await
            .unwrap();
        let by_device: Vec<_> = planned
            .iter()
            .map(|p| (p.device.as_str(), p.deployment.as_str(), p.outcome.is_ok()))
            .collect();
        assert_eq!(
            by_device,
            [
                ("BROKEN", "r1-broken", true),
                ("DEV1", "r1-dev1", false),
                ("DEV2", "r1-dev2", true)
            ]
        );
        assert!(planned[1]
            .outcome
            .as_ref()
            .unwrap_err()
            .contains("non-terminal deployment"));
        assert_eq!(
            ota.get_deployment(None, "r1-dev2").await.unwrap().status,
            DeploymentStatus::Running
        );

        let report = ota
            .deploy(
                None,
                DeployRequest {
                    release: "R1".into(),
                    targets: Targets::Selector(Labels::from([("board".into(), "none".into())])),
                    name: None,
                    labels: Labels::new(),
                    start: true,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NoTargets(_)));

        let report = ota
            .deploy(
                None,
                DeployRequest {
                    release: "R1".into(),
                    targets: Targets::Devices(vec!["DEV2".into(), "DEV3".into()]),
                    name: Some("x".into()),
                    labels: Labels::new(),
                    start: false,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NameForMany));
    }
}
