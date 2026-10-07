use std::path::PathBuf;

use error_stack::ResultExt;
use remora_context::{application::ContextService, model::ContextOverride};
use remora_format::human_size;
use remora_install::{
    application::InstallService,
    model::{Bundle, InstallOutcome, InstallRequest, DEFAULT_REMOTE_DIR},
};
use remora_ota::application::OtaService;
use remora_tui as tui;

use super::error::{Error, Result};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// The device to install on, by serial.
    #[arg(add = remora_completion::values(remora_completion::Kind::Device))]
    device: String,

    /// A local RAUC bundle (.raucb) to install.
    #[arg(
        value_hint = clap::ValueHint::FilePath,
        required_unless_present = "release",
        conflicts_with = "release"
    )]
    bundle: Option<PathBuf>,

    /// Install a bundle of this release instead: one of its artifacts tagged
    /// `type:rauc`, picked by --board or --artifact, or asked for when
    /// there are several. Downloaded first, into a local cache a later run
    /// reuses.
    #[arg(long)]
    #[arg(add = remora_completion::values(remora_completion::Kind::Release))]
    release: Option<String>,

    /// With --release: the board to install the bundle of (its artifact's
    /// `board:` tag).
    #[arg(long, requires = "release")]
    board: Option<String>,

    /// With --release: the artifact to install, by file name.
    #[arg(long, requires = "release")]
    artifact: Option<String>,

    /// Where on the device the bundle is uploaded before it's installed.
    #[arg(long, default_value = DEFAULT_REMOTE_DIR)]
    remote_dir: String,

    /// Install, but don't reboot into the new slot (nor validate it).
    #[arg(long)]
    no_reboot: bool,

    /// Reboot into the new slot, but don't mark it good: validate it
    /// yourself (`remora-otactl validate`), or it's rolled back.
    #[arg(long, conflicts_with = "no_reboot")]
    no_validate: bool,

    /// The device account to log into, instead of the admin role's
    /// default.
    #[arg(long)]
    user: Option<String>,

    /// The ssh client to run.
    #[arg(long, default_value = "ssh", value_hint = clap::ValueHint::CommandName)]
    ssh: PathBuf,
}

pub async fn run(
    args: Args,
    install: &InstallService,
    ota: &OtaService,
    contexts: &ContextService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let Args {
        device,
        bundle,
        release,
        board,
        artifact,
        remote_dir,
        no_reboot,
        no_validate,
        user,
        ssh,
    } = args;

    let mut lines = vec![tui::accent(&device)];
    let label = tui::dim;
    let bundle = match (bundle, release) {
        (Some(path), _) => {
            lines.push(format!("{} {}", label("bundle:"), path.display()));
            Bundle::File(path)
        }
        (None, Some(release)) => {
            let found = ota.get_release(over, &release).await.map_err(|report| {
                let message = report.current_context().to_string();
                report.change_context(Error::Install(message))
            })?;
            let picked = remora_ota_application_transport_cli::choose_artifact(
                &release,
                found.artifacts,
                artifact.as_deref(),
                board.as_deref(),
                Some("rauc"),
            )
            .change_context(Error::Choose)?;
            let mut from = format!(
                "{} {} {} from release {}",
                label("bundle:"),
                picked.file_name,
                human_size(picked.content_length),
                tui::accent(&release)
            );
            if let Ok(context) = contexts.resolve(over).await {
                from.push_str(&format!(
                    ", {}",
                    tui::dim(format!("context {}", context.context.name))
                ));
            }
            lines.push(from);
            if !picked.tag_condition.is_empty() {
                lines.push(format!("{} {}", label("tags:"), picked.tag_condition));
            }
            Bundle::Release {
                release,
                file_name: picked.file_name,
                cache: cache_dir(),
            }
        }
        (None, None) => unreachable!("clap requires a bundle or --release"),
    };
    lines.push(format!("{} {remote_dir}", label("upload to:")));
    lines.push(format!(
        "{} {}",
        label("then:"),
        match (no_reboot, no_validate) {
            (true, _) => "install, no reboot",
            (false, true) => "install, reboot into the new slot, don't validate it",
            (false, false) => "install, reboot into the new slot, validate it",
        }
    ));
    tui::note("Install", lines.join("\n"));

    let request = InstallRequest {
        over: over.cloned(),
        device: device.clone(),
        bundle,
        login: user,
        remote_dir,
        reboot: !no_reboot,
        validate: !no_reboot && !no_validate,
        ssh,
        proxy_command: remora_channel_application_transport_cli::proxy_command()
            .change_context(Error::Ssh)?,
    };
    let (sink, stream) = remora_progress::channel();
    let follow = tui::follow(stream);
    let ctx = remora_progress::OperationContext::new(sink, remora_progress::cancelled_by_ctrl_c());
    let outcome = install.install(request, &ctx).await;
    drop(ctx);
    follow.finish(&outcome).await;
    let outcome = outcome.map_err(|report| {
        let message = report.current_context().to_string();
        report.change_context(Error::Install(message))
    })?;
    tui::success(summary(&device, &outcome));
    Ok(())
}

fn summary(device: &str, outcome: &InstallOutcome) -> String {
    let mut detail = Vec::new();
    if outcome.uploaded > 0 {
        detail.push(format!("{} uploaded", human_size(outcome.uploaded)));
    }
    if outcome.resumed_from > 0 {
        detail.push(format!("resumed at {}", human_size(outcome.resumed_from)));
    }
    let slots = match (&outcome.from_slot, &outcome.to_slot) {
        (Some(from), Some(to)) => format!(", slot {from} → {to}"),
        _ => String::new(),
    };
    let state = match (outcome.rebooted, outcome.validated) {
        (true, true) => "running it, validated",
        (true, false) => "running it, not validated",
        (false, _) => "reboot to run it",
    };
    format!(
        "Installed {} on {}{slots}: {state} {}",
        tui::accent(&outcome.bundle),
        tui::accent(device),
        tui::dim(format!("({})", detail.join(", ")))
    )
    .replace(" ()", "")
}

/// Where a release's bundles are downloaded to, and found by later runs.
fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("rmra").join("bundles")
}
