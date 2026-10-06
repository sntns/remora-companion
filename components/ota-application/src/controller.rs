use std::{collections::BTreeSet, path::Path, time::Duration};

use error_stack::{FrameKind, Report, ResultExt};
use remora_context::{
    application::ContextService,
    model::{ContextOverride, ResolvedContext},
};
use remora_device::application::DeviceService;
use remora_ota::{
    adapter::{
        gateway::{self, ArtifactSink, InitialUpload, OtaGatewayAdapterService},
        source::{self, ArtifactSourceAdapterService},
    },
    application::{Error, OtaServiceInterface, Result},
    model::{
        DeployRequest, Deployment, DeploymentFilter, DeploymentProgress, DeploymentStatus,
        DeploymentSummary, Labels, LogEntry, Planned, PlannedOutcome, Release, ReleaseSummary,
        Targets, UploadOutcome, UploadRequest, WatchOutcome,
    },
};
use remora_progress::OperationContext;

/// One message per chunk: big enough to keep a fast link busy, small enough
/// that a dropped stream loses little and progress moves smoothly.
const CHUNK: usize = 1024 * 1024;

/// How many times a dropped upload is resumed before giving up (the
/// operator can still resume it later with the printed token).
const UPLOAD_ATTEMPTS: usize = 4;

/// The ota vertical's use case: the gateway port and where artifacts are
/// read from, the context vertical's application port for which platform,
/// as whom, and the device vertical's for which devices a selector picks.
pub struct OtaControllerImpl {
    contexts: ContextService,
    devices: DeviceService,
    gateway: OtaGatewayAdapterService,
    artifacts: ArtifactSourceAdapterService,
}

/// Why one upload attempt stopped short.
enum Stop {
    Cancelled,
    /// Reading the local file: retrying wouldn't help.
    Source(Report<source::Error>),
    Gateway(Report<gateway::Error>),
}

impl OtaControllerImpl {
    pub fn new(
        contexts: ContextService,
        devices: DeviceService,
        gateway: OtaGatewayAdapterService,
        artifacts: ArtifactSourceAdapterService,
    ) -> Self {
        Self {
            contexts,
            devices,
            gateway,
            artifacts,
        }
    }

    async fn resolve(&self, over: Option<&ContextOverride>) -> Result<ResolvedContext> {
        self.contexts
            .resolve(over)
            .await
            .change_context(Error::Context)
    }

    /// One attempt: from wherever the platform got to, to the end. Every
    /// way out short of the end aborts the sink, so that nothing partial
    /// is ever committed.
    async fn upload_from(
        &self,
        context: &ResolvedContext,
        initial: &InitialUpload,
        path: &Path,
        offset: u64,
        total: u64,
        ctx: &OperationContext,
    ) -> std::result::Result<(), Stop> {
        if ctx.cancel.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        let mut reader = self
            .artifacts
            .open(path, offset)
            .await
            .map_err(Stop::Source)?;
        let mut sink = self
            .gateway
            .begin_upload(context, initial.clone())
            .await
            .map_err(Stop::Gateway)?;
        let mut done = offset;
        ctx.sink.progress(done, total);
        loop {
            if ctx.cancel.is_cancelled() {
                return Err(abort(sink, Stop::Cancelled).await);
            }
            let chunk = match reader.read(CHUNK).await {
                Ok(chunk) => chunk,
                Err(report) => return Err(abort(sink, Stop::Source(report)).await),
            };
            if chunk.is_empty() {
                break;
            }
            done += chunk.len() as u64;
            // A slow link blocks here: Ctrl-C must not wait for it.
            let sent = tokio::select! {
                sent = sink.send(chunk) => sent,
                () = ctx.cancel.cancelled() => return Err(abort(sink, Stop::Cancelled).await),
            };
            if let Err(report) = sent {
                return Err(abort(sink, Stop::Gateway(report)).await);
            }
            ctx.sink.progress(done, total);
        }
        // The protocol carries no length: a file that shrank since it was
        // measured would otherwise be committed truncated.
        if done != total {
            let report = Report::new(source::Error::Read).attach(format!(
                "{} changed while uploading: {done} bytes instead of {total}",
                path.display()
            ));
            return Err(abort(sink, Stop::Source(report)).await);
        }
        sink.finish().await.map_err(Stop::Gateway)
    }

    /// The devices `targets` names, through the device vertical for a
    /// selector.
    async fn targets(
        &self,
        over: Option<&ContextOverride>,
        targets: &Targets,
    ) -> Result<Vec<String>> {
        match targets {
            Targets::Devices(devices) => Ok(devices.clone()),
            Targets::Selector(labels) => {
                let devices: Vec<String> = self
                    .devices
                    .list(over, labels, false)
                    .await
                    .change_context(Error::Targets)?
                    .into_iter()
                    .map(|device| device.name)
                    .collect();
                if devices.is_empty() {
                    let selector: Vec<_> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    return Err(Report::new(Error::NoTargets(selector.join(","))));
                }
                Ok(devices)
            }
        }
    }

    /// One deployment's progress, now. A failure to read its reports
    /// doesn't stop the watch (the status still says where it is) but is
    /// shown rather than hidden.
    async fn progress(&self, context: &ResolvedContext, name: &str) -> Result<DeploymentProgress> {
        let deployment = self
            .gateway
            .get_deployment(context, name)
            .await
            .change_context_lazy(|| Error::GetDeployment(name.to_owned()))?;
        let mut progress = match self.gateway.deployment_logs(context, name).await {
            Ok(logs) => DeploymentProgress::of(&deployment, &logs),
            Err(report) => DeploymentProgress {
                detail: format!("reports unavailable: {}", reason(&report)),
                ..DeploymentProgress::of(&deployment, &[])
            },
        };
        if let DeploymentStatus::Other(status) = &deployment.status {
            progress.detail =
                format!("status {status} is unknown to this version: stopped following it");
        }
        Ok(progress)
    }
}

/// Aborts an upload that stopped short, passing on why.
async fn abort(sink: Box<dyn ArtifactSink>, stop: Stop) -> Stop {
    sink.abort().await;
    stop
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
    )
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

#[async_trait::async_trait]
impl OtaServiceInterface for OtaControllerImpl {
    async fn resolve_targets(
        &self,
        over: Option<&ContextOverride>,
        targets: &Targets,
    ) -> Result<Vec<String>> {
        self.targets(over, targets).await
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
        let unreadable = || Error::ReadArtifact(request.path.clone());
        let total = self
            .artifacts
            .length(&request.path)
            .await
            .change_context_lazy(unreadable)?;
        let file_name = match &request.file_name {
            Some(name) => name.clone(),
            None => request
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
                .ok_or_else(|| Report::new(unreadable()))?,
        };
        let initial = InitialUpload {
            release: request.release.clone(),
            file_name: file_name.clone(),
            content_type: request
                .content_type
                .clone()
                .unwrap_or_else(|| content_type(&request.path).to_owned()),
            tag_condition: request.tag_condition.clone(),
            content_id: request
                .resume
                .clone()
                .unwrap_or_else(|| self.gateway.new_content_id()),
        };
        let failed = || Error::Upload {
            file: file_name.clone(),
            release: request.release.clone(),
            content_id: initial.content_id.clone(),
        };
        let cancelled = || Error::Cancelled {
            file: file_name.clone(),
            release: request.release.clone(),
            content_id: initial.content_id.clone(),
        };

        let mut resumed_from = None;
        let mut attempt = 0;
        loop {
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
                Err(Stop::Cancelled) => return Err(Report::new(cancelled())),
                Err(Stop::Gateway(report)) if ctx.cancel.is_cancelled() => {
                    return Err(report.change_context(cancelled()))
                }
                Err(Stop::Source(report)) => return Err(report.change_context(unreadable())),
                Err(Stop::Gateway(report))
                    if retryable(&report) && attempt + 1 < UPLOAD_ATTEMPTS =>
                {
                    attempt += 1
                }
                Err(Stop::Gateway(report)) => return Err(report.change_context(failed())),
            }
        }
    }

    async fn deploy(
        &self,
        over: Option<&ContextOverride>,
        request: DeployRequest,
    ) -> Result<Vec<Planned>> {
        let context = self.resolve(over).await?;
        let devices = self.targets(over, &request.targets).await?;
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
                Err(report) => PlannedOutcome::NotCreated(
                    report.change_context(Error::CreateDeployment(name.clone())),
                ),
                Ok(()) if !request.start => PlannedOutcome::Draft,
                Ok(()) => match self.gateway.start_deployment(&context, &name).await {
                    Ok(()) => PlannedOutcome::Started,
                    Err(report) => PlannedOutcome::NotStarted(
                        report.change_context(Error::StartDeployment(name.clone())),
                    ),
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

    async fn watch(
        &self,
        over: Option<&ContextOverride>,
        names: &[String],
        interval: Duration,
        ctx: &OperationContext,
        on_progress: &mut (dyn FnMut(DeploymentProgress) + Send),
    ) -> Result<WatchOutcome> {
        let context = self.resolve(over).await?;
        let mut outcome = WatchOutcome::default();
        let mut pending: BTreeSet<String> = names.iter().cloned().collect();
        loop {
            for name in pending.clone() {
                let progress = self.progress(&context, &name).await?;
                let status = progress.status.clone();
                on_progress(progress);
                if status.is_settled() {
                    pending.remove(&name);
                    match status {
                        DeploymentStatus::Succeeded => outcome.succeeded.push(name),
                        DeploymentStatus::Other(_) => outcome.unknown.push(name),
                        _ => outcome.failed.push(name),
                    }
                }
            }
            if pending.is_empty() {
                return Ok(outcome);
            }
            tokio::select! {
                () = tokio::time::sleep(interval) => {}
                () = ctx.cancel.cancelled() => {
                    outcome.pending = pending.into_iter().collect();
                    return Ok(outcome);
                }
            }
        }
    }

    async fn follow_logs(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
        interval: Duration,
        ctx: &OperationContext,
        on_entry: &mut (dyn FnMut(LogEntry) + Send),
    ) -> Result<Option<DeploymentStatus>> {
        let context = self.resolve(over).await?;
        let failed = || Error::GetDeployment(name.to_owned());
        let mut seen = 0;
        loop {
            let deployment = self
                .gateway
                .get_deployment(&context, name)
                .await
                .change_context_lazy(failed)?;
            // After the status, so that a settled deployment's last
            // reports are in.
            let logs = self
                .gateway
                .deployment_logs(&context, name)
                .await
                .change_context_lazy(failed)?;
            logs.into_iter().skip(seen).for_each(|entry| {
                seen += 1;
                on_entry(entry);
            });
            if deployment.status.is_settled() {
                return Ok(Some(deployment.status));
            }
            tokio::select! {
                () = tokio::time::sleep(interval) => {}
                () = ctx.cancel.cancelled() => return Ok(None),
            }
        }
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
    use remora_device::adapter::gateway::DeviceGatewayAdapterService;
    use remora_device_adapter_grpc::DeviceGatewayAdapterImpl;
    use remora_device_application::DeviceControllerImpl;
    use remora_ota::adapter::{
        gateway::OtaGatewayAdapter,
        source::{ArtifactReader, ArtifactSourceAdapter},
    };
    use remora_ota_adapter_file::FileArtifactSourceImpl;
    use remora_ota_adapter_grpc::{test_gateway::TestGateway, OtaGatewayAdapterImpl};
    use tokio_util::sync::CancellationToken;

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

    /// The real local files, with what happens on every read decided by
    /// the test: the file changing under the upload, or the operator
    /// pressing Ctrl-C at an exact point.
    struct Interfering {
        after_reads: usize,
        then: Interference,
    }

    #[derive(Clone)]
    enum Interference {
        Cancel(CancellationToken),
        Fail,
    }

    struct InterferingReader {
        inner: Box<dyn ArtifactReader>,
        reads: usize,
        after_reads: usize,
        then: Interference,
    }

    #[async_trait::async_trait]
    impl ArtifactSourceAdapter for Interfering {
        async fn length(&self, path: &Path) -> source::Result<u64> {
            FileArtifactSourceImpl.length(path).await
        }

        async fn open(&self, path: &Path, offset: u64) -> source::Result<Box<dyn ArtifactReader>> {
            Ok(Box::new(InterferingReader {
                inner: FileArtifactSourceImpl.open(path, offset).await?,
                reads: 0,
                after_reads: self.after_reads,
                then: self.then.clone(),
            }))
        }
    }

    #[async_trait::async_trait]
    impl ArtifactReader for InterferingReader {
        async fn read(&mut self, max: usize) -> source::Result<Vec<u8>> {
            if self.reads == self.after_reads {
                match &self.then {
                    Interference::Cancel(token) => token.cancel(),
                    Interference::Fail => {
                        return Err(Report::new(source::Error::Read).attach("disk on fire"))
                    }
                }
            }
            self.reads += 1;
            self.inner.read(max).await
        }
    }

    /// The real gRPC adapter against the in-process fake gateway, logged in
    /// through the real context controller on a temporary store.
    async fn controller(root: &Path) -> (OtaControllerImpl, TestGateway) {
        controller_reading(
            root,
            ArtifactSourceAdapterService::new(FileArtifactSourceImpl),
        )
        .await
    }

    async fn controller_reading(
        root: &Path,
        artifacts: ArtifactSourceAdapterService,
    ) -> (OtaControllerImpl, TestGateway) {
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
        let contexts = ContextService::new(contexts);
        let devices = DeviceService::new(DeviceControllerImpl::new(
            contexts.clone(),
            DeviceGatewayAdapterService::new(DeviceGatewayAdapterImpl),
        ));
        (
            OtaControllerImpl::new(
                contexts,
                devices,
                OtaGatewayAdapterService::new(OtaGatewayAdapterImpl),
                artifacts,
            ),
            gateway,
        )
    }

    /// Waits until `done` holds of the fake's state.
    async fn until(
        gateway: &TestGateway,
        done: impl Fn(&remora_ota_adapter_grpc::test_gateway::State) -> bool,
    ) {
        for _ in 0..500 {
            if done(&gateway.0.lock().unwrap()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the gateway never got there");
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
    async fn a_cancelled_upload_commits_nothing_and_resumes_with_its_token() {
        let dir = tempfile::tempdir().unwrap();
        let cancel = CancellationToken::new();
        // Ctrl-C right after the first chunk went out.
        let (ota, gateway) = controller_reading(
            dir.path(),
            ArtifactSourceAdapterService::new(Interfering {
                after_reads: 1,
                then: Interference::Cancel(cancel.clone()),
            }),
        )
        .await;
        ota.create_release(None, "r1", "1.0.0", &Labels::new())
            .await
            .unwrap();
        let (path, bytes) = bundle(dir.path(), 3 * CHUNK);

        let ctx = OperationContext::new(remora_progress::ProgressSink::noop(), cancel);
        let report = ota
            .upload(None, upload(&path, None), &ctx)
            .await
            .unwrap_err();
        let Error::Cancelled { content_id, .. } = report.current_context() else {
            panic!("{report:?}");
        };
        let token = content_id.clone();
        assert!(report.to_string().contains(&format!("--resume {token}")));
        assert_eq!(
            report.current_context().resume_token(),
            Some(token.as_str())
        );

        until(&gateway, |state| state.abandoned_uploads == 1).await;
        assert!(gateway.artifact_bytes("r1", "update-rp5.raucb").is_none());
        let held = gateway.0.lock().unwrap().upload_offset(&token);
        assert!(held <= CHUNK, "{held}");

        let outcome = ota
            .upload(
                None,
                upload(&path, Some(token.clone())),
                &OperationContext::noop(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.resumed_from, held as u64);
        assert_eq!(
            gateway.artifact_bytes("r1", "update-rp5.raucb").unwrap(),
            bytes
        );
    }

    #[tokio::test]
    async fn a_local_read_error_is_not_retried_nor_committed() {
        let dir = tempfile::tempdir().unwrap();
        let (ota, gateway) = controller_reading(
            dir.path(),
            ArtifactSourceAdapterService::new(Interfering {
                after_reads: 1,
                then: Interference::Fail,
            }),
        )
        .await;
        ota.create_release(None, "r1", "1.0.0", &Labels::new())
            .await
            .unwrap();
        let (path, _) = bundle(dir.path(), 2 * CHUNK);

        let report = ota
            .upload(None, upload(&path, None), &OperationContext::noop())
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::ReadArtifact(_)));
        assert!(format!("{report:?}").contains("disk on fire"));
        until(&gateway, |state| state.abandoned_uploads == 1).await;
        // One attempt only, and nothing committed.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(gateway.0.lock().unwrap().abandoned_uploads, 1);
        assert!(gateway.artifact_bytes("r1", "update-rp5.raucb").is_none());
    }

    #[tokio::test]
    async fn watches_deployments_until_each_is_settled() {
        let dir = tempfile::tempdir().unwrap();
        let (ota, gateway) = controller(dir.path()).await;
        ota.create_release(None, "R1", "1.0.0", &Labels::new())
            .await
            .unwrap();
        ota.deploy(
            None,
            DeployRequest {
                release: "R1".into(),
                targets: Targets::Devices(vec!["DEV1".into(), "BROKEN".into(), "DEV2".into()]),
                name: None,
                labels: Labels::new(),
                start: true,
            },
        )
        .await
        .unwrap();
        // A status a newer platform may have added: not followed forever.
        gateway.set_deployment_status("r1-dev2", "QUARANTINED");

        let mut seen = Vec::new();
        let names = ["r1-dev1".into(), "r1-broken".into(), "r1-dev2".into()];
        let outcome = ota
            .watch(
                None,
                &names,
                Duration::from_millis(1),
                &OperationContext::noop(),
                &mut |progress| seen.push(progress),
            )
            .await
            .unwrap();
        assert_eq!(
            outcome,
            WatchOutcome {
                succeeded: vec!["r1-dev1".into()],
                failed: vec!["r1-broken".into()],
                unknown: vec!["r1-dev2".into()],
                pending: vec![],
            }
        );
        let dev1: Vec<_> = seen
            .iter()
            .filter(|p| p.name == "r1-dev1")
            .map(|p| p.percent)
            .collect();
        assert_eq!(dev1, [Some(50), Some(100), Some(100)]);
        let broken = seen.iter().rfind(|p| p.name == "r1-broken").unwrap();
        assert_eq!(broken.detail, "reason=bundle signature invalid");
        let odd = seen.iter().find(|p| p.name == "r1-dev2").unwrap();
        assert!(odd.detail.contains("unknown"), "{}", odd.detail);
    }

    #[tokio::test]
    async fn a_cancelled_watch_leaves_the_rest_pending() {
        let dir = tempfile::tempdir().unwrap();
        let (ota, _) = controller(dir.path()).await;
        ota.create_release(None, "R1", "1.0.0", &Labels::new())
            .await
            .unwrap();
        ota.deploy(
            None,
            DeployRequest {
                release: "R1".into(),
                targets: Targets::Devices(vec!["DEV1".into()]),
                name: None,
                labels: Labels::new(),
                start: true,
            },
        )
        .await
        .unwrap();
        let ctx = OperationContext::noop();
        ctx.cancel.cancel();
        let outcome = ota
            .watch(
                None,
                &["r1-dev1".into()],
                Duration::from_secs(60),
                &ctx,
                &mut |_| {},
            )
            .await
            .unwrap();
        assert_eq!(outcome.pending, ["r1-dev1"]);
    }

    #[tokio::test]
    async fn follows_a_deployments_reports_to_the_end() {
        let dir = tempfile::tempdir().unwrap();
        let (ota, _) = controller(dir.path()).await;
        ota.create_release(None, "R1", "1.0.0", &Labels::new())
            .await
            .unwrap();
        ota.deploy(
            None,
            DeployRequest {
                release: "R1".into(),
                targets: Targets::Devices(vec!["DEV1".into()]),
                name: None,
                labels: Labels::new(),
                start: true,
            },
        )
        .await
        .unwrap();
        let mut entries = Vec::new();
        let status = ota
            .follow_logs(
                None,
                "r1-dev1",
                Duration::from_millis(1),
                &OperationContext::noop(),
                &mut |entry| entries.push(entry),
            )
            .await
            .unwrap();
        assert_eq!(status, Some(DeploymentStatus::Succeeded));
        let progress: Vec<_> = entries
            .iter()
            .map(|e| e.progress.as_ref().map(|p| p.current))
            .collect();
        assert_eq!(progress, [Some(50), Some(100), None]);
        assert_eq!(entries.last().unwrap().result, "success");
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
        assert!(matches!(first[0].outcome, PlannedOutcome::Draft));

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
            .map(|p| {
                (
                    p.device.as_str(),
                    p.deployment.as_str(),
                    p.outcome.failure().is_none(),
                )
            })
            .collect();
        assert_eq!(
            by_device,
            [
                ("BROKEN", "r1-broken", true),
                ("DEV1", "r1-dev1", false),
                ("DEV2", "r1-dev2", true)
            ]
        );
        let PlannedOutcome::NotCreated(report) = &planned[1].outcome else {
            panic!("{:?}", planned[1].outcome);
        };
        assert!(matches!(
            report.current_context(),
            Error::CreateDeployment(_)
        ));
        assert!(reason(report).contains("non-terminal deployment"));
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
