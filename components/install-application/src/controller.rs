use std::{
    path::Path,
    time::{Duration, Instant},
};

use error_stack::{Report, ResultExt};
use remora_channel::{
    application::{ChannelService, Error as ChannelError},
    model::{ExecInput, ExecOutcome, ExecRequest, SshRole},
};
use remora_format::human_size as bytes;
use remora_install::{
    application::{Error, InstallServiceInterface, Result},
    model::{Bundle, InstallOutcome, InstallRequest},
};
use remora_ota::{application::OtaService, model::DownloadRequest};
use remora_progress::{OperationContext, OperationEvent};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use tokio_stream::StreamExt;

use crate::remote::{self, RemotePaths};

/// How many upload attempts in a row may end without the device having
/// more of the bundle before the upload gives up.
const UPLOAD_ATTEMPTS: usize = 4;

/// How long a device may take to come back after rebooting.
const REBOOT_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const REBOOT_POLL: Duration = Duration::from_secs(5);

/// The install vertical's use case: the channel vertical's application port
/// to run each step on the device, and the ota vertical's to fetch a
/// release's bundle.
pub struct InstallControllerImpl {
    channel: ChannelService,
    ota: OtaService,
}

impl InstallControllerImpl {
    pub fn new(channel: ChannelService, ota: OtaService) -> Self {
        Self { channel, ota }
    }

    /// Runs `command` on the device, as the `admin` ssh role.
    async fn run(
        &self,
        request: &InstallRequest,
        command: String,
        input: Option<ExecInput>,
        echo: bool,
        ctx: &OperationContext,
    ) -> Result<ExecOutcome> {
        self.channel
            .exec(
                ExecRequest {
                    over: request.over.clone(),
                    device: request.device.clone(),
                    role: SshRole::Admin,
                    login: request.login.clone(),
                    command,
                    input,
                    echo,
                    binary: request.ssh.clone(),
                    proxy_command: request.proxy_command.clone(),
                },
                ctx,
            )
            .await
            .map_err(|report| {
                if matches!(report.current_context(), ChannelError::Cancelled) {
                    report.change_context(Error::Cancelled)
                } else {
                    report.change_context(Error::Channel(request.device.clone()))
                }
            })
    }

    /// [`Self::run`], which must succeed: a failed command is `error`,
    /// with what it said.
    async fn must(
        &self,
        request: &InstallRequest,
        command: String,
        ctx: &OperationContext,
        error: impl Fn() -> Error,
    ) -> Result<ExecOutcome> {
        let outcome = self.run(request, command, None, false, ctx).await?;
        if outcome.code != 0 {
            return Err(failed(error(), &outcome));
        }
        Ok(outcome)
    }

    /// The bundle as a local file, with its size and sha256: as given, or a
    /// release's, downloaded (or found already downloaded) into its cache.
    async fn local_bundle(
        &self,
        request: &InstallRequest,
        ctx: &OperationContext,
    ) -> Result<(std::path::PathBuf, u64, String)> {
        let (release, file_name, cache) = match &request.bundle {
            Bundle::File(path) => {
                ctx.sink.phase("hashing");
                let (size, sha256) = hash(path, ctx).await?;
                return Ok((path.clone(), size, sha256));
            }
            Bundle::Release {
                release,
                file_name,
                cache,
            } => (release, file_name, cache),
        };
        ctx.sink.phase("downloading");
        let artifact = self
            .ota
            .get_release(request.over.as_ref(), release)
            .await
            .change_context(Error::Download)?
            .artifacts
            .into_iter()
            .find(|artifact| &artifact.file_name == file_name)
            .ok_or_else(|| {
                Report::new(Error::Download)
                    .attach(format!("release {release:?} has no artifact {file_name}"))
            })?;
        tokio::fs::create_dir_all(cache)
            .await
            .change_context_lazy(|| Error::Bundle(cache.clone()))?;
        let path = cache.join(file_name);
        // Already downloaded, and still the release's: reused.
        if tokio::fs::try_exists(&path).await.unwrap_or(false) {
            ctx.sink
                .log(format!("found {} in the cache", path.display()));
            let (size, sha256) = hash(&path, ctx).await?;
            if size == artifact.content_length
                && (artifact.checksum_sha256.is_empty()
                    || sha256.eq_ignore_ascii_case(&artifact.checksum_sha256))
            {
                return Ok((path, size, sha256));
            }
        }
        self.ota
            .download_file(
                request.over.as_ref(),
                DownloadRequest {
                    release: release.clone(),
                    file_name: file_name.clone(),
                    path: path.clone(),
                    overwrite: true,
                },
                ctx,
            )
            .await
            .change_context(Error::Download)?;
        let sha256 = if artifact.checksum_sha256.is_empty() {
            hash(&path, ctx).await?.1
        } else {
            artifact.checksum_sha256.to_ascii_lowercase()
        };
        Ok((path, artifact.content_length, sha256))
    }

    /// Uploads the bundle to `paths.part`, from what the device already
    /// has; a dropped channel is picked up again where the device got to.
    /// Returns what this run sent, and where it started.
    async fn upload(
        &self,
        request: &InstallRequest,
        paths: &RemotePaths,
        local: &Path,
        size: u64,
        mut on_device: u64,
        ctx: &OperationContext,
    ) -> Result<(u64, u64)> {
        let device = || Error::Upload(request.device.clone());
        if on_device > size {
            // Longer than the bundle: not a part of it.
            self.must(request, remote::remove(&paths.part), ctx, device)
                .await?;
            on_device = 0;
        }
        let started = on_device;
        if started > 0 {
            ctx.sink
                .log(format!("resuming at {} of {}", bytes(started), bytes(size)));
        }
        let mut stalled = 0;
        while on_device < size {
            let outcome = self
                .run(
                    request,
                    remote::append(&paths.part),
                    Some(ExecInput {
                        path: local.to_path_buf(),
                        offset: on_device,
                    }),
                    false,
                    ctx,
                )
                .await?;
            // What the device really has, whatever ssh says it sent.
            let now = loop {
                let check = self
                    .run(request, remote::size(&paths.part), None, false, ctx)
                    .await?;
                if check.code == 0 {
                    break remote::fields(&check.stdout)
                        .get("size")
                        .and_then(|size| size.parse().ok())
                        .unwrap_or(0);
                }
                stalled += 1;
                if stalled >= UPLOAD_ATTEMPTS {
                    return Err(failed(device(), &check));
                }
                tokio::time::sleep(Duration::from_secs(stalled as u64)).await;
            };
            if now > on_device {
                stalled = 0;
            } else {
                stalled += 1;
                if stalled >= UPLOAD_ATTEMPTS {
                    return Err(failed(device(), &outcome));
                }
            }
            if now < size {
                ctx.sink
                    .log(format!("channel dropped at {}: resuming", bytes(now)));
            }
            on_device = now;
        }
        Ok((size - started, started))
    }

    /// Waits for the device to come back on another slot than `from`, or at
    /// least from another boot than `boot` (then on whatever slot it
    /// booted), and says that slot.
    async fn wait_for_reboot(
        &self,
        request: &InstallRequest,
        boot: Option<&str>,
        from: Option<&str>,
        ctx: &OperationContext,
    ) -> Result<Option<String>> {
        let started = Instant::now();
        loop {
            if ctx.cancel.is_cancelled() {
                return Err(Report::new(Error::Cancelled));
            }
            if started.elapsed() > REBOOT_TIMEOUT {
                return Err(Report::new(Error::RebootTimeout(request.device.clone())));
            }
            tokio::select! {
                _ = tokio::time::sleep(REBOOT_POLL) => {}
                _ = ctx.cancel.cancelled() => return Err(Report::new(Error::Cancelled)),
            }
            let quiet =
                OperationContext::new(remora_progress::ProgressSink::noop(), ctx.cancel.clone());
            let outcome = self
                .run(request, remote::state(), None, false, &quiet)
                .await?;
            let waited = started.elapsed().as_secs();
            if outcome.code != 0 {
                ctx.sink.log(format!(
                    "waiting for {} to come back ({waited}s)",
                    request.device
                ));
                continue;
            }
            let fields = remote::fields(&outcome.stdout);
            let slot = fields.get("slot").map(String::as_str);
            let now = fields.get("boot").map(String::as_str);
            if (slot.is_some() && from.is_some() && slot != from) || (now.is_some() && now != boot)
            {
                return Ok(slot.map(str::to_owned));
            }
            ctx.sink.log(format!(
                "waiting for {} to go down ({waited}s)",
                request.device
            ));
        }
    }
}

#[async_trait::async_trait]
impl InstallServiceInterface for InstallControllerImpl {
    async fn install(
        &self,
        request: InstallRequest,
        ctx: &OperationContext,
    ) -> Result<InstallOutcome> {
        let (local, size, sha256) = self.local_bundle(&request, ctx).await?;
        let paths = RemotePaths::new(&request.remote_dir, &sha256);
        let mut outcome = InstallOutcome {
            bundle: local
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            size,
            ..Default::default()
        };

        ctx.sink.phase("checking");
        ctx.sink.log(format!("connecting to {}", request.device));
        let check = self
            .run(&request, remote::check(&paths), None, false, ctx)
            .await?;
        match check.code {
            0 => {}
            remote::NO_INSTALLER => {
                return Err(Report::new(Error::NoInstaller(request.device.clone())))
            }
            _ => return Err(failed(Error::Channel(request.device.clone()), &check)),
        }
        let state = remote::fields(&check.stdout);
        outcome.from_slot = state.get("slot").cloned();
        let boot = state.get("boot").cloned();
        let number = |key: &str| state.get(key).and_then(|v| v.parse::<u64>().ok());

        // Already whole from an earlier run: only verified again.
        let whole = number("bundle") == Some(size);
        if !whole {
            ctx.sink.phase("uploading");
            let (uploaded, resumed_from) = self
                .upload(
                    &request,
                    &paths,
                    &local,
                    size,
                    number("part").unwrap_or(0),
                    ctx,
                )
                .await?;
            outcome.uploaded = uploaded;
            outcome.resumed_from = resumed_from;
        }

        ctx.sink.phase("verifying");
        let on_device = if whole { &paths.bundle } else { &paths.part };
        ctx.sink.log(format!("sha256 of {on_device}"));
        let sum = self
            .must(&request, remote::sha256(on_device), ctx, || {
                Error::Upload(request.device.clone())
            })
            .await?;
        if !sum.stdout.trim().eq_ignore_ascii_case(&sha256) {
            let _ = self
                .run(&request, remote::remove(on_device), None, false, ctx)
                .await;
            return Err(Report::new(Error::Checksum {
                device: request.device.clone(),
            })
            .attach(format!("expected {sha256}, got {}", sum.stdout.trim())));
        }
        if !whole {
            self.must(
                &request,
                remote::rename(&paths.part, &paths.bundle),
                ctx,
                || Error::Upload(request.device.clone()),
            )
            .await?;
        }

        ctx.sink.phase("installing");
        let installed = self
            .run_otactl(&request, remote::install(&paths.bundle), ctx)
            .await?;
        if installed.code != 0 {
            return Err(failed(Error::Install(request.device.clone()), &installed));
        }
        // Installed: the bundle has nothing left to do on the device.
        let _ = self
            .run(&request, remote::remove(&paths.bundle), None, false, ctx)
            .await;

        if !request.reboot {
            return Ok(outcome);
        }
        ctx.sink.phase("rebooting");
        self.must(&request, remote::reboot(), ctx, || {
            Error::Install(request.device.clone())
        })
        .await?;
        outcome.rebooted = true;
        let slot = self
            .wait_for_reboot(&request, boot.as_deref(), outcome.from_slot.as_deref(), ctx)
            .await?;
        if let (Some(from), Some(now)) = (&outcome.from_slot, &slot) {
            if from == now {
                return Err(Report::new(Error::OldSlot {
                    device: request.device.clone(),
                    slot: now.clone(),
                }));
            }
        }
        outcome.to_slot = slot;

        if request.validate {
            ctx.sink.phase("validating");
            let validated = self
                .run(&request, remote::validate(), None, true, ctx)
                .await?;
            if validated.code != 0 {
                return Err(failed(Error::Validate(request.device.clone()), &validated));
            }
            outcome.validated = true;
        }
        Ok(outcome)
    }
}

impl InstallControllerImpl {
    /// A `remora-otactl` command, its output relayed: RAUC's percentage as
    /// the phase's progress, its steps and messages (not its noise, see
    /// [`remote::otactl_line`]) as log lines.
    async fn run_otactl(
        &self,
        request: &InstallRequest,
        command: String,
        ctx: &OperationContext,
    ) -> Result<ExecOutcome> {
        let (sink, mut events) = remora_progress::channel();
        let inner = OperationContext::new(sink, ctx.cancel.clone());
        let outer = ctx.sink.clone();
        let relay = tokio::spawn(async move {
            let mut last_step = String::new();
            while let Some(event) = events.next().await {
                let OperationEvent::Log(line) = event else {
                    continue;
                };
                match remote::rauc_progress(&line) {
                    Some((percent, step)) => {
                        outer.progress(percent, 100);
                        let step = step.strip_suffix(" done.").unwrap_or(&step).to_owned();
                        if !step.is_empty() && step != last_step {
                            outer.log(step.clone());
                            last_step = step;
                        }
                    }
                    None => {
                        if let Some(line) = remote::otactl_line(&line) {
                            if line != last_step {
                                outer.log(line.clone());
                                last_step = line;
                            }
                        }
                    }
                }
            }
        });
        let outcome = self.run(request, command, None, true, &inner).await;
        drop(inner);
        let _ = relay.await;
        outcome
    }
}

/// `error`, with what the device said.
fn failed(error: Error, outcome: &ExecOutcome) -> Report<Error> {
    let said: Vec<&str> = outcome
        .stderr
        .lines()
        .chain(outcome.stdout.lines())
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = said[said.len().saturating_sub(5)..].join("\n");
    let report = Report::new(error).attach(format!("exit code {}", outcome.code));
    if tail.is_empty() {
        report
    } else {
        report.attach(tail)
    }
}

/// `path`'s size and sha256, reporting how far it got through `ctx`.
async fn hash(path: &Path, ctx: &OperationContext) -> Result<(u64, String)> {
    let error = || Error::Bundle(path.to_path_buf());
    let mut file = tokio::fs::File::open(path)
        .await
        .change_context_lazy(error)?;
    let size = file.metadata().await.change_context_lazy(error)?.len();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    let mut done = 0u64;
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(Report::new(Error::Cancelled));
        }
        let read = file.read(&mut buffer).await.change_context_lazy(error)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        done += read as u64;
        ctx.sink.progress_bytes(done, size);
    }
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok((size, sha256))
}
