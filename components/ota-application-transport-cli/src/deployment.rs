use std::{collections::BTreeSet, time::Duration};

use error_stack::Report;
use remora_context::model::ContextOverride;
use remora_ota::{
    application::OtaService,
    model::{DeployRequest, Deployment, DeploymentFilter, LogEntry, Targets},
};
use remora_tui::{self as tui, Tone, WatchLine};
use serde_json::json;

use crate::{
    error::{ota_error, prompt_error, Error, Result},
    shared::{labels_line, parse_labels, print_json, status_tone, Format},
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
        device: Option<String>,
        /// Only deployments of this release.
        #[arg(long)]
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
        name: String,
        #[arg(long, default_value = "table")]
        format: Format,
    },

    /// Start draft deployments, making them visible to their devices.
    Start {
        #[arg(required = true)]
        names: Vec<String>,
        /// Follow them until they finish.
        #[arg(short, long)]
        watch: bool,
    },

    /// Cancel deployments that are still pending or running.
    Cancel {
        #[arg(required = true)]
        names: Vec<String>,
    },

    /// Delete deployments.
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        names: Vec<String>,
        /// Don't ask for confirmation.
        #[arg(short, long)]
        force: bool,
    },

    /// Print what the device reported while updating.
    Logs {
        name: String,
        /// Keep printing new reports until the deployment finishes.
        #[arg(short, long)]
        follow: bool,
    },

    /// Follow deployments live until they finish; fails if any doesn't
    /// succeed. Ctrl-C stops watching, not the deployments.
    Watch {
        #[arg(required = true)]
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
    release: String,
    /// A device (serial) to update; repeatable.
    #[arg(long = "device", value_name = "SERIAL")]
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
        Command::Start { names, watch } => {
            for name in &names {
                service
                    .start_deployment(over, name)
                    .await
                    .map_err(ota_error)?;
                tui::success(format!("Started {}", tui::accent(name)));
            }
            if watch {
                follow(service, over, names, Duration::from_secs(2)).await?;
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
        Command::Logs { name, follow } => logs(service, over, &name, follow).await,
        Command::Watch { names, interval } => {
            self::follow(service, over, names, Duration::from_secs(interval.max(1))).await
        }
    }
}

/// `rmra deploy`: one deployment per device, started unless `--draft`.
pub async fn run_deploy(
    args: DeployArgs,
    service: &OtaService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let targets = if args.devices.is_empty() {
        Targets::Selector(parse_labels(&args.selector)?)
    } else {
        Targets::Devices(args.devices)
    };
    // A selector can reach the whole fleet: say how much before doing it.
    if let Targets::Selector(labels) = &targets {
        let devices = service
            .resolve_targets(over, &targets)
            .await
            .map_err(ota_error)?;
        let question = format!(
            "Deploy {} to {} device{} matching {}?",
            tui::accent(&args.release),
            devices.len(),
            if devices.len() == 1 { "" } else { "s" },
            labels_line(labels)
        );
        if !args.yes {
            if !tui::interactive() {
                return Err(Report::new(Error::Ota(
                    "not a terminal: pass --yes to deploy to devices picked by --selector".into(),
                )));
            }
            tui::note("Targets", devices.join("\n"));
            if !tui::confirm(question)
                .initial_value(false)
                .interact()
                .map_err(prompt_error)?
            {
                return Err(Report::new(Error::Cancelled));
            }
        }
    }

    let spinner = tui::Spinner::start(format!("Deploying {}", tui::accent(&args.release)));
    let planned = match service
        .deploy(
            over,
            DeployRequest {
                release: args.release.clone(),
                targets,
                name: args.name,
                labels: parse_labels(&args.labels)?,
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
    let failed = planned.iter().filter(|p| p.outcome.is_err()).count();
    let summary = format!(
        "{} of {} deployment{} created{}",
        planned.len() - failed,
        planned.len(),
        if planned.len() == 1 { "" } else { "s" },
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

    let mut table = tui::Table::new(["device", "deployment", "result"]);
    for plan in &planned {
        let (result, tone) = match &plan.outcome {
            Ok(true) => ("started".to_owned(), Tone::Active),
            Ok(false) => ("draft".to_owned(), Tone::Idle),
            Err(reason) => (reason.clone(), Tone::Bad),
        };
        table.row_toned([
            (plan.device.clone(), None),
            (plan.deployment.clone(), None),
            (result, Some(tone)),
        ]);
    }
    table.print();

    let started: Vec<_> = planned
        .iter()
        .filter(|p| matches!(p.outcome, Ok(true)))
        .map(|p| p.deployment.clone())
        .collect();
    if args.watch && !started.is_empty() {
        follow(
            service,
            over,
            started,
            Duration::from_secs(args.interval.max(1)),
        )
        .await?;
    } else if !started.is_empty() {
        tui::step(format!(
            "Follow with {}",
            tui::accent(format!("rmra deployment watch {}", started.join(" ")))
        ));
    }
    if failed > 0 {
        return Err(Report::new(Error::NotCreated(failed, planned.len())));
    }
    Ok(())
}

/// Polls deployments until each is terminal, drawing one live line each.
async fn follow(
    service: &OtaService,
    over: Option<&ContextOverride>,
    names: Vec<String>,
    interval: Duration,
) -> Result<()> {
    let total = names.len();
    let mut watch = tui::Watch::new(format!(
        "Following {total} deployment{}",
        if total == 1 { "" } else { "s" }
    ));
    let mut pending: BTreeSet<String> = names.into_iter().collect();
    let mut unsuccessful = 0;
    while !pending.is_empty() {
        for name in pending.clone() {
            let deployment = match service.get_deployment(over, &name).await {
                Ok(deployment) => deployment,
                Err(report) => {
                    watch.finish();
                    return Err(ota_error(report));
                }
            };
            let logs = service
                .deployment_logs(over, &name)
                .await
                .unwrap_or_default();
            let line = watch_line(&deployment, &logs);
            watch.update(&name, line);
            if deployment.status.is_terminal() {
                pending.remove(&name);
                if deployment.status.is_failure() {
                    unsuccessful += 1;
                }
            }
        }
        if pending.is_empty() {
            break;
        }
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            _ = tokio::signal::ctrl_c() => {
                watch.finish();
                tui::info(format!(
                    "Stopped watching; {} deployment{} carry on",
                    pending.len(),
                    if pending.len() == 1 { "" } else { "s" }
                ));
                return Ok(());
            }
        }
    }
    watch.finish();
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

/// A deployment's live line: status, how far the latest report says it
/// is, and the latest thing it said (or why it failed).
fn watch_line(deployment: &Deployment, logs: &[LogEntry]) -> WatchLine {
    let percent = logs
        .iter()
        .rev()
        .find_map(|entry| entry.progress.as_ref())
        .filter(|progress| progress.max > 0)
        .map(|progress| (progress.current.clamp(0, progress.max) * 100 / progress.max) as u64);
    let detail = if deployment.status.is_failure() && !deployment.details.is_empty() {
        labels_line(&deployment.details)
    } else {
        logs.iter()
            .rev()
            .find_map(|entry| entry.details.last().cloned())
            .unwrap_or_default()
    };
    WatchLine {
        status: deployment.status.to_string(),
        tone: status_tone(&deployment.status),
        percent: if deployment.status == remora_ota::model::DeploymentStatus::Succeeded {
            Some(100)
        } else {
            percent
        },
        detail,
        finished: deployment.status.is_terminal(),
    }
}

async fn logs(
    service: &OtaService,
    over: Option<&ContextOverride>,
    name: &str,
    follow: bool,
) -> Result<()> {
    let mut printed = 0;
    loop {
        let logs = service
            .deployment_logs(over, name)
            .await
            .map_err(ota_error)?;
        for entry in logs.iter().skip(printed) {
            println!("{}", log_line(entry));
        }
        printed = printed.max(logs.len());
        if !follow {
            return Ok(());
        }
        let deployment = service
            .get_deployment(over, name)
            .await
            .map_err(ota_error)?;
        if deployment.status.is_terminal() {
            tui::info(format!(
                "{} is {}",
                tui::accent(name),
                tui::toned(&deployment.status, status_tone(&deployment.status))
            ));
            return Ok(());
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(2)) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
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
