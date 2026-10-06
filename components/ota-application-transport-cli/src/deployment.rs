use std::time::Duration;

use error_stack::Report;
use remora_context::model::ContextOverride;
use remora_ota::{
    application::OtaService,
    model::{
        DeployRequest, Deployment, DeploymentFilter, DeploymentProgress, LogEntry, Planned,
        PlannedOutcome, Targets,
    },
};
use remora_progress::{OperationContext, ProgressSink};
use remora_tui::{self as tui, Tone, WatchLine};
use serde_json::json;

use crate::{
    error::{ota_error, prompt_error, Error, Result},
    shared::{labels_line, parse_labels, print_json, reason, status_tone, Format},
};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Deploy a release to devices (same as `rmra deploy`).
    Create(DeployArgs),

    /// List deployments.
    #[command(visible_alias = "ls")]
    List {
        /// Only deployments to this device.
        #[arg(long)]
        #[arg(add = remora_completion::values(remora_completion::Kind::Device))]
        device: Option<String>,
        /// Only deployments of this release.
        #[arg(long)]
        #[arg(add = remora_completion::values(remora_completion::Kind::Release))]
        release: Option<String>,
        /// Only deployments carrying this label (key=value); repeatable.
        #[arg(long = "label", value_name = "KEY=VALUE")]
        labels: Vec<String>,
        #[arg(long, default_value = "table")]
        format: Format,
        /// Only print deployment names.
        #[arg(short, long)]
        quiet: bool,
    },

    /// Show a deployment: its status and the device's latest reports.
    Show {
        #[arg(add = remora_completion::values(remora_completion::Kind::Deployment))]
        name: String,
        #[arg(long, default_value = "table")]
        format: Format,
    },

    /// Start draft deployments, making them visible to their devices.
    Start {
        #[arg(required = true)]
        #[arg(add = remora_completion::values(remora_completion::Kind::Deployment))]
        names: Vec<String>,
        /// Follow them until they finish.
        #[arg(short, long)]
        watch: bool,
        /// Seconds between polls while following.
        #[arg(long, default_value_t = 2, requires = "watch")]
        interval: u64,
    },

    /// Cancel deployments that are still pending or running.
    Cancel {
        #[arg(required = true)]
        #[arg(add = remora_completion::values(remora_completion::Kind::Deployment))]
        names: Vec<String>,
    },

    /// Delete deployments.
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        #[arg(add = remora_completion::values(remora_completion::Kind::Deployment))]
        names: Vec<String>,
        /// Don't ask for confirmation.
        #[arg(short, long)]
        force: bool,
    },

    /// Print what the device reported while updating.
    Logs {
        #[arg(add = remora_completion::values(remora_completion::Kind::Deployment))]
        name: String,
        /// Keep printing new reports until the deployment finishes.
        #[arg(short, long)]
        follow: bool,
        /// Seconds between polls while following.
        #[arg(long, default_value_t = 2, requires = "follow")]
        interval: u64,
        /// `json`: an array, or one object per line with --follow.
        #[arg(long, default_value = "table")]
        format: Format,
    },

    /// Follow deployments live until they finish; fails if any doesn't
    /// succeed. Ctrl-C stops watching, not the deployments.
    Watch {
        #[arg(required = true)]
        #[arg(add = remora_completion::values(remora_completion::Kind::Deployment))]
        names: Vec<String>,
        /// Seconds between polls.
        #[arg(long, default_value_t = 2)]
        interval: u64,
    },
}

#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("targets").required(true).args(["devices", "selector"])))]
pub struct DeployArgs {
    /// The release to install.
    #[arg(add = remora_completion::values(remora_completion::Kind::Release))]
    release: String,
    /// A device (serial) to update; repeatable.
    #[arg(long = "device", value_name = "SERIAL")]
    #[arg(add = remora_completion::values(remora_completion::Kind::Device))]
    devices: Vec<String>,
    /// Every device carrying this label (key=value); repeatable, all must match.
    #[arg(long, value_name = "KEY=VALUE")]
    selector: Vec<String>,
    /// The deployment's name, for a single device (default: <release>-<serial>).
    #[arg(long)]
    name: Option<String>,
    /// A label to put on each deployment (key=value); repeatable.
    #[arg(long = "label", value_name = "KEY=VALUE")]
    labels: Vec<String>,
    /// Create drafts, to start later with `rmra deployment start`.
    #[arg(long)]
    draft: bool,
    /// Follow the deployments until they finish.
    #[arg(short, long)]
    watch: bool,
    /// Seconds between polls while following.
    #[arg(long, default_value_t = 2, requires = "watch")]
    interval: u64,
    /// Don't ask before deploying to devices picked by --selector.
    #[arg(short, long)]
    yes: bool,
    /// How to print what was created, for each device.
    #[arg(long, default_value = "table")]
    format: Format,
}

pub async fn run(
    command: Command,
    service: &OtaService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    match command {
        Command::Create(args) => run_deploy(args, service, over).await,
        Command::List {
            device,
            release,
            labels,
            format,
            quiet,
        } => {
            let filter = DeploymentFilter {
                device,
                release,
                labels: parse_labels(&labels)?,
            };
            let deployments = service
                .list_deployments(over, &filter)
                .await
                .map_err(ota_error)?;
            if quiet {
                deployments.iter().for_each(|d| println!("{}", d.name));
                return Ok(());
            }
            match format {
                Format::Json => print_json(&json!(deployments
                    .iter()
                    .map(|d| json!({"name": d.name, "status": d.status.as_str()}))
                    .collect::<Vec<_>>())),
                Format::Table if deployments.is_empty() => tui::info("No deployment."),
                Format::Table => {
                    let mut table = tui::Table::new(["name", "status"]);
                    for deployment in &deployments {
                        table.row_toned([
                            (deployment.name.clone(), None),
                            (
                                deployment.status.to_string(),
                                Some(status_tone(&deployment.status)),
                            ),
                        ]);
                    }
                    table.print();
                }
            }
            Ok(())
        }
        Command::Show { name, format } => {
            let deployment = service
                .get_deployment(over, &name)
                .await
                .map_err(ota_error)?;
            let logs = service
                .deployment_logs(over, &name)
                .await
                .map_err(ota_error)?;
            match format {
                Format::Json => print_json(&json!({
                    "name": deployment.name,
                    "release": deployment.release,
                    "device": deployment.target,
                    "status": deployment.status.as_str(),
                    "statusDetails": deployment.details,
                    "updatedAt": deployment.updated_at.map(tui::timestamp),
                    "labels": deployment.labels,
                    "logs": logs.iter().map(log_json).collect::<Vec<_>>(),
                })),
                Format::Table => print_deployment(&deployment, &logs),
            }
            Ok(())
        }
        Command::Start {
            names,
            watch,
            interval,
        } => {
            for name in &names {
                service
                    .start_deployment(over, name)
                    .await
                    .map_err(ota_error)?;
                tui::success(format!("Started {}", tui::accent(name)));
            }
            if watch {
                follow(service, over, &names, seconds(interval)).await?;
            }
            Ok(())
        }
        Command::Cancel { names } => {
            for name in names {
                service
                    .cancel_deployment(over, &name)
                    .await
                    .map_err(ota_error)?;
                tui::success(format!("Cancellation of {} requested", tui::accent(&name)));
            }
            Ok(())
        }
        Command::Remove { names, force } => {
            for name in names {
                if !force && tui::interactive() {
                    let question = format!("Delete deployment {}?", tui::accent(&name));
                    if !tui::confirm(question)
                        .initial_value(false)
                        .interact()
                        .map_err(prompt_error)?
                    {
                        tui::info(format!("Kept {}", tui::accent(&name)));
                        continue;
                    }
                }
                service
                    .delete_deployment(over, &name)
                    .await
                    .map_err(ota_error)?;
                tui::success(format!("Deleted deployment {}", tui::accent(&name)));
            }
            Ok(())
        }
        Command::Logs {
            name,
            follow,
            interval,
            format,
        } => {
            let interval = follow.then(|| seconds(interval));
            logs(service, over, &name, interval, format).await
        }
        Command::Watch { names, interval } => {
            self::follow(service, over, &names, seconds(interval)).await
        }
    }
}

/// A polling interval from `--interval`: at least a second, so that a
/// typo doesn't hammer the platform.
fn seconds(interval: u64) -> Duration {
    Duration::from_secs(interval.max(1))
}

/// Watching and following stop on Ctrl-C: the deployments carry on.
fn interruptible() -> OperationContext {
    OperationContext::new(ProgressSink::noop(), remora_progress::cancelled_by_ctrl_c())
}

/// `rmra deploy`: one deployment per device, started unless `--draft`.
pub async fn run_deploy(
    args: DeployArgs,
    service: &OtaService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let labels = parse_labels(&args.labels)?;
    let targets = if !args.devices.is_empty() {
        Targets::Devices(args.devices)
    } else if args.yes {
        Targets::Selector(parse_labels(&args.selector)?)
    } else {
        // A selector can reach the whole fleet: say how much before doing
        // it, then deploy to exactly the devices confirmed -- not to one
        // labelled in the meantime, which nobody was asked about.
        let selector = parse_labels(&args.selector)?;
        if !tui::interactive() {
            return Err(Report::new(Error::NeedsYes));
        }
        let devices = service
            .resolve_targets(over, &Targets::Selector(selector.clone()))
            .await
            .map_err(ota_error)?;
        tui::note("Targets", devices.join("\n"));
        let question = format!(
            "Deploy {} to {} device{} matching {}?",
            tui::accent(&args.release),
            devices.len(),
            if devices.len() == 1 { "" } else { "s" },
            labels_line(&selector)
        );
        if !tui::confirm(question)
            .initial_value(false)
            .interact()
            .map_err(prompt_error)?
        {
            return Err(Report::new(Error::Cancelled));
        }
        Targets::Devices(devices)
    };

    let spinner = tui::Spinner::start(format!("Deploying {}", tui::accent(&args.release)));
    let planned = match service
        .deploy(
            over,
            DeployRequest {
                release: args.release.clone(),
                targets,
                name: args.name,
                labels,
                start: !args.draft,
            },
        )
        .await
    {
        Ok(planned) => planned,
        Err(report) => {
            spinner.fail(format!("Could not deploy {}", tui::accent(&args.release)));
            return Err(ota_error(report));
        }
    };
    let total = planned.len();
    let failed = planned
        .iter()
        .filter(|p| p.outcome.failure().is_some())
        .count();
    let summary = format!(
        "{} of {} deployment{} created{}",
        total - failed,
        total,
        if total == 1 { "" } else { "s" },
        if args.draft {
            " as drafts"
        } else {
            " and started"
        }
    );
    if failed == 0 {
        spinner.done(summary);
    } else {
        spinner.fail(summary);
    }
    match args.format {
        Format::Json => print_json(&json!(planned.iter().map(planned_json).collect::<Vec<_>>())),
        Format::Table => print_planned(&planned),
    }

    let started: Vec<_> = planned
        .iter()
        .filter(|p| matches!(p.outcome, PlannedOutcome::Started))
        .map(|p| p.deployment.clone())
        .collect();
    if args.watch && !started.is_empty() {
        follow(service, over, &started, seconds(args.interval)).await?;
    } else if !started.is_empty() {
        tui::step(format!(
            "Follow with {}",
            tui::accent(format!("rmra deployment watch {}", started.join(" ")))
        ));
    }
    // Each failure's whole chain, under one headline.
    let failures: Option<Report<[remora_ota::application::Error]>> = planned
        .into_iter()
        .filter_map(|plan| match plan.outcome {
            PlannedOutcome::NotCreated(report) | PlannedOutcome::NotStarted(report) => Some(report),
            PlannedOutcome::Started | PlannedOutcome::Draft => None,
        })
        .collect();
    match failures {
        Some(failures) => Err(failures.change_context(Error::NotCreated(failed, total))),
        None => Ok(()),
    }
}

fn planned_result(plan: &Planned) -> (String, Tone) {
    match &plan.outcome {
        PlannedOutcome::Started => ("started".to_owned(), Tone::Active),
        PlannedOutcome::Draft => ("draft".to_owned(), Tone::Idle),
        PlannedOutcome::NotCreated(report) => (reason(report), Tone::Bad),
        PlannedOutcome::NotStarted(report) => (
            format!("created, but not started: {}", reason(report)),
            Tone::Bad,
        ),
    }
}

fn print_planned(planned: &[Planned]) {
    let mut table = tui::Table::new(["device", "deployment", "result"]);
    for plan in planned {
        let (result, tone) = planned_result(plan);
        table.row_toned([
            (plan.device.clone(), None),
            (plan.deployment.clone(), None),
            (result, Some(tone)),
        ]);
    }
    table.print();
}

fn planned_json(plan: &Planned) -> serde_json::Value {
    let (result, error) = match &plan.outcome {
        PlannedOutcome::Started => ("started", None),
        PlannedOutcome::Draft => ("draft", None),
        PlannedOutcome::NotCreated(report) => ("not-created", Some(reason(report))),
        PlannedOutcome::NotStarted(report) => ("not-started", Some(reason(report))),
    };
    json!({
        "device": plan.device,
        "deployment": plan.deployment,
        "result": result,
        "error": error,
    })
}

/// Follows deployments until each is settled, one live line each; fails
/// if any doesn't succeed.
async fn follow(
    service: &OtaService,
    over: Option<&ContextOverride>,
    names: &[String],
    interval: Duration,
) -> Result<()> {
    let total = names.len();
    let mut watch = tui::Watch::new(format!(
        "Following {total} deployment{}",
        if total == 1 { "" } else { "s" }
    ));
    let outcome = service
        .watch(over, names, interval, &interruptible(), &mut |progress| {
            watch.update(&progress.name, watch_line(&progress))
        })
        .await;
    watch.finish();
    let outcome = outcome.map_err(ota_error)?;
    if !outcome.pending.is_empty() {
        tui::info(format!(
            "Stopped watching; {} deployment{} carry on",
            outcome.pending.len(),
            if outcome.pending.len() == 1 { "" } else { "s" }
        ));
        return Ok(());
    }
    if !outcome.unknown.is_empty() {
        tui::warning(format!(
            "{} reached a status this rmra doesn't know: see {}, or get a newer rmra with {}",
            outcome.unknown.join(", "),
            tui::accent("rmra deployment show <name>"),
            tui::accent("rmra update")
        ));
    }
    let unsuccessful = outcome.failed.len() + outcome.unknown.len();
    if unsuccessful > 0 {
        return Err(Report::new(Error::Unsuccessful(unsuccessful, total)));
    }
    tui::success(format!(
        "{} deployment{} succeeded",
        if total == 1 {
            "The".to_owned()
        } else {
            format!("All {total}")
        },
        if total == 1 { "" } else { "s" }
    ));
    Ok(())
}

fn watch_line(progress: &DeploymentProgress) -> WatchLine {
    WatchLine {
        status: progress.status.to_string(),
        tone: status_tone(&progress.status),
        percent: progress.percent,
        detail: progress.detail.clone(),
        finished: progress.status.is_settled(),
    }
}

/// Prints a deployment's reports; with `follow` (an interval), new ones as
/// they come until it is settled.
async fn logs(
    service: &OtaService,
    over: Option<&ContextOverride>,
    name: &str,
    follow: Option<Duration>,
    format: Format,
) -> Result<()> {
    let Some(interval) = follow else {
        let logs = service
            .deployment_logs(over, name)
            .await
            .map_err(ota_error)?;
        match format {
            Format::Json => print_json(&json!(logs.iter().map(log_json).collect::<Vec<_>>())),
            Format::Table => logs
                .iter()
                .for_each(|entry| println!("{}", log_line(entry))),
        }
        return Ok(());
    };
    let status = service
        .follow_logs(over, name, interval, &interruptible(), &mut |entry| {
            match format {
                // One object per line: a stream a script can read as it comes.
                Format::Json => println!("{}", log_json(&entry)),
                Format::Table => println!("{}", log_line(&entry)),
            }
        })
        .await
        .map_err(ota_error)?;
    if let Some(status) = status {
        tui::info(format!(
            "{} is {}",
            tui::accent(name),
            tui::toned(&status, status_tone(&status))
        ));
    }
    Ok(())
}

fn log_line(entry: &LogEntry) -> String {
    let mut line = match entry.recorded_at {
        Some(at) => format!("{} {}", tui::dim(tui::timestamp(at)), entry.execution),
        None => entry.execution.clone(),
    };
    if !entry.result.is_empty() {
        let tone = match entry.result.as_str() {
            "success" => Tone::Good,
            "failure" => Tone::Bad,
            _ => Tone::Idle,
        };
        line.push(' ');
        line.push_str(&tui::toned(&entry.result, tone));
    }
    if entry.code != 0 {
        line.push_str(&format!(" (code {})", entry.code));
    }
    if let Some(progress) = &entry.progress {
        line.push_str(&format!(
            " {}/{} {}",
            progress.current, progress.max, progress.unit
        ));
    }
    for detail in &entry.details {
        line.push_str(&format!(" — {detail}"));
    }
    line
}

fn log_json(entry: &LogEntry) -> serde_json::Value {
    json!({
        "execution": entry.execution,
        "result": entry.result,
        "code": entry.code,
        "details": entry.details,
        "recordedAt": entry.recorded_at.map(tui::timestamp),
        "progress": entry.progress.as_ref().map(|p| json!({
            "current": p.current, "max": p.max, "unit": p.unit, "speed": p.speed,
        })),
    })
}

fn print_deployment(deployment: &Deployment, logs: &[LogEntry]) {
    let mut body = format!(
        "release   {}\ndevice    {}\nstatus    {}",
        deployment.release,
        deployment.target,
        tui::toned(&deployment.status, status_tone(&deployment.status))
    );
    if let Some(at) = deployment.updated_at {
        body.push_str(&format!(
            "\nupdated   {} ({})",
            tui::timestamp(at),
            tui::ago(at)
        ));
    }
    if !deployment.details.is_empty() {
        body.push_str(&format!("\ndetails   {}", labels_line(&deployment.details)));
    }
    if !deployment.labels.is_empty() {
        body.push_str(&format!("\nlabels    {}", labels_line(&deployment.labels)));
    }
    tui::note(
        format!("Deployment {}", tui::accent(&deployment.name)),
        body,
    );
    if logs.is_empty() {
        tui::info("The device has not reported anything yet.");
        return;
    }
    let shown = logs.len().min(5);
    tui::step(format!(
        "Latest {shown} of {} reports {}",
        logs.len(),
        tui::dim(format!("(all: rmra deployment logs {})", deployment.name))
    ));
    for entry in &logs[logs.len() - shown..] {
        println!("{}", log_line(entry));
    }
}
