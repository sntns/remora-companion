use std::path::PathBuf;

use remora_context::model::ContextOverride;
use remora_ota::{
    application::OtaService,
    model::{Release, UploadRequest},
};
use remora_progress::{OperationContext, OperationEvent};
use remora_tui as tui;
use serde_json::json;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{
    error::{ota_error, prompt_error, Result},
    shared::{labels_line, parse_labels, print_json, Format},
};

#[derive(clap::Subcommand)]
pub enum Command {
    /// List releases.
    #[command(visible_alias = "ls")]
    List {
        /// Only releases carrying this label (key=value); repeatable.
        #[arg(long = "label", value_name = "KEY=VALUE")]
        labels: Vec<String>,
        #[arg(long, default_value = "table")]
        format: Format,
        /// Only print release names.
        #[arg(short, long)]
        quiet: bool,
    },

    /// Create a release, optionally uploading its artifacts in the same go.
    Create {
        name: String,
        /// The version devices will report once updated.
        #[arg(long)]
        version: String,
        /// A label to put on the release (key=value); repeatable.
        #[arg(long = "label", value_name = "KEY=VALUE")]
        labels: Vec<String>,
        /// A file to upload as an artifact (e.g. a RAUC bundle); repeatable.
        #[arg(long = "artifact", value_name = "PATH")]
        #[arg(value_hint = clap::ValueHint::FilePath)]
        artifacts: Vec<PathBuf>,
        /// Which devices the --artifact files are for, as a boolean
        /// expression over device tags, e.g. "type:rauc && board:rp5".
        #[arg(long)]
        tag_condition: Option<String>,
    },

    /// Show a release and its artifacts.
    Show {
        #[arg(add = remora_completion::values(remora_completion::Kind::Release))]
        name: String,
        #[arg(long, default_value = "table")]
        format: Format,
    },

    /// Upload a file as an artifact of a release. Resumes on its own after
    /// a dropped connection; resume an interrupted run with --resume.
    Upload {
        #[arg(add = remora_completion::values(remora_completion::Kind::Release))]
        release: String,
        #[arg(value_hint = clap::ValueHint::FilePath)]
        path: PathBuf,
        /// The name update clients see (default: the file's name).
        #[arg(long)]
        file_name: Option<String>,
        /// Default: from the extension, else application/octet-stream.
        #[arg(long)]
        content_type: Option<String>,
        /// Which devices this artifact is for, e.g. "board:rp5".
        #[arg(long, default_value = "")]
        tag_condition: String,
        /// The resume token an interrupted upload printed.
        #[arg(long, value_name = "TOKEN")]
        resume: Option<String>,
    },

    /// Set or remove a release's labels.
    Label {
        #[arg(add = remora_completion::values(remora_completion::Kind::Release))]
        name: String,
        /// key=value to set; repeatable.
        #[arg(value_name = "KEY=VALUE")]
        set: Vec<String>,
        /// A label key to remove; repeatable.
        #[arg(long = "unset", value_name = "KEY")]
        unset: Vec<String>,
    },

    /// Delete releases.
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        #[arg(add = remora_completion::values(remora_completion::Kind::Release))]
        names: Vec<String>,
        /// Don't ask for confirmation.
        #[arg(short, long)]
        force: bool,
    },
}

pub async fn run(
    command: Command,
    service: &OtaService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    match command {
        Command::List {
            labels,
            format,
            quiet,
        } => {
            let releases = service
                .list_releases(over, &parse_labels(&labels)?)
                .await
                .map_err(ota_error)?;
            if quiet {
                releases.iter().for_each(|r| println!("{}", r.name));
                return Ok(());
            }
            match format {
                Format::Json => print_json(&json!(releases
                    .iter()
                    .map(|r| json!({"name": r.name, "version": r.version}))
                    .collect::<Vec<_>>())),
                Format::Table if releases.is_empty() => tui::info(format!(
                    "No release yet. Publish one with {}",
                    tui::accent("rmra release create <name> --version <v> --artifact <bundle>")
                )),
                Format::Table => {
                    let mut table = tui::Table::new(["name", "version"]);
                    for release in &releases {
                        table.row([release.name.clone(), release.version.clone()], false);
                    }
                    table.print();
                }
            }
            Ok(())
        }
        Command::Create {
            name,
            version,
            labels,
            artifacts,
            tag_condition,
        } => {
            let labels = parse_labels(&labels)?;
            let spinner = tui::Spinner::start(format!("Creating release {}", tui::accent(&name)));
            if let Err(report) = service.create_release(over, &name, &version, &labels).await {
                spinner.fail(format!("Could not create {}", tui::accent(&name)));
                return Err(ota_error(report));
            }
            spinner.done(format!(
                "Created release {} {}",
                tui::accent(&name),
                tui::dim(format!("version {version}"))
            ));
            let cancel = remora_progress::cancelled_by_ctrl_c();
            for path in artifacts {
                upload(
                    service,
                    over,
                    UploadRequest {
                        release: name.clone(),
                        path,
                        file_name: None,
                        content_type: None,
                        tag_condition: tag_condition.clone().unwrap_or_default(),
                        resume: None,
                    },
                    &cancel,
                )
                .await?;
            }
            tui::step(format!(
                "Deploy it with {}",
                tui::accent(format!("rmra deploy {name} --device <serial>"))
            ));
            Ok(())
        }
        Command::Show { name, format } => {
            let release = service.get_release(over, &name).await.map_err(ota_error)?;
            match format {
                Format::Json => print_json(&release_json(&release)),
                Format::Table => print_release(&release),
            }
            Ok(())
        }
        Command::Upload {
            release,
            path,
            file_name,
            content_type,
            tag_condition,
            resume,
        } => {
            upload(
                service,
                over,
                UploadRequest {
                    release,
                    path,
                    file_name,
                    content_type,
                    tag_condition,
                    resume,
                },
                &remora_progress::cancelled_by_ctrl_c(),
            )
            .await
        }
        Command::Label { name, set, unset } => {
            service
                .label_release(over, &name, &parse_labels(&set)?, &unset)
                .await
                .map_err(ota_error)?;
            let release = service.get_release(over, &name).await.map_err(ota_error)?;
            tui::success(format!(
                "Labels of {}: {}",
                tui::accent(&name),
                if release.labels.is_empty() {
                    tui::dim("none")
                } else {
                    labels_line(&release.labels)
                }
            ));
            Ok(())
        }
        Command::Remove { names, force } => {
            for name in names {
                if !force && tui::interactive() {
                    let question =
                        format!("Delete release {} and its artifacts?", tui::accent(&name));
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
                    .delete_release(over, &name)
                    .await
                    .map_err(ota_error)?;
                tui::success(format!("Deleted release {}", tui::accent(&name)));
            }
            Ok(())
        }
    }
}

/// Uploads one artifact with a live byte bar; Ctrl-C (`cancel`) stops it
/// cleanly, printing how to resume.
async fn upload(
    service: &OtaService,
    over: Option<&ContextOverride>,
    request: UploadRequest,
    cancel: &CancellationToken,
) -> Result<()> {
    let shown = request
        .file_name
        .clone()
        .or_else(|| {
            request
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let title = format!(
        "Uploading {} to {}",
        tui::accent(&shown),
        tui::accent(&request.release)
    );
    // Drawn once the upload knows the file's size (its first progress).
    let mut bar: Option<tui::Progress> = None;

    let (sink, mut events) = remora_progress::channel();
    let ctx = OperationContext::new(sink, cancel.clone());
    let resume_with = request.clone();
    let upload = service.upload(over, request, &ctx);
    tokio::pin!(upload);
    let result = loop {
        tokio::select! {
            result = &mut upload => break result,
            Some(event) = events.next() => match event {
                OperationEvent::Progress { done, total } => bar
                    .get_or_insert_with(|| tui::Progress::bytes(total, &title))
                    .set(done),
                OperationEvent::Log(line) => match &bar {
                    Some(bar) => bar.note(line),
                    None => tui::step(line),
                },
                OperationEvent::Phase(_) => {}
            },
        }
    };
    match result {
        Ok(outcome) => {
            let mut resumed = String::new();
            if outcome.resumed_from > 0 {
                resumed.push_str(&format!(
                    ", continued from {}",
                    tui::bytes(outcome.resumed_from)
                ));
            }
            if outcome.resumes > 0 {
                resumed.push_str(&format!(
                    ", resumed {} time{} after a dropped connection",
                    outcome.resumes,
                    if outcome.resumes == 1 { "" } else { "s" }
                ));
            }
            let message = format!(
                "Uploaded {} to {} {}",
                tui::accent(&outcome.file_name),
                tui::accent(&resume_with.release),
                tui::dim(format!("{}{resumed}", tui::bytes(outcome.bytes)))
            );
            match bar {
                Some(bar) => bar.done(message),
                None => tui::success(message),
            }
            Ok(())
        }
        Err(report) => {
            // Without a bar, it failed before sending anything: the error
            // says it all.
            if let Some(bar) = bar {
                bar.fail(format!("Upload of {} stopped", tui::accent(&shown)));
            }
            if let Some(token) = report.current_context().resume_token() {
                tui::step(format!(
                    "Resume it with {}",
                    tui::accent(resume_command(&resume_with, token))
                ));
            }
            Err(ota_error(report))
        }
    }
}

/// The command that continues an interrupted upload where it stopped: the
/// same file, release and artifact settings, and the token.
fn resume_command(request: &UploadRequest, token: &str) -> String {
    let quote = |value: &str| {
        if !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./:=+,@".contains(c))
        {
            value.to_owned()
        } else {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
    };
    let mut command = format!(
        "rmra release upload {} {}",
        quote(&request.release),
        quote(&request.path.to_string_lossy())
    );
    if let Some(name) = &request.file_name {
        command.push_str(&format!(" --file-name {}", quote(name)));
    }
    if let Some(content_type) = &request.content_type {
        command.push_str(&format!(" --content-type {}", quote(content_type)));
    }
    if !request.tag_condition.is_empty() {
        command.push_str(&format!(
            " --tag-condition {}",
            quote(&request.tag_condition)
        ));
    }
    command.push_str(&format!(" --resume {token}"));
    command
}

fn print_release(release: &Release) {
    let mut body = format!("version   {}", release.version);
    if !release.labels.is_empty() {
        body.push_str(&format!("\nlabels    {}", labels_line(&release.labels)));
    }
    tui::note(format!("Release {}", tui::accent(&release.name)), body);
    if release.artifacts.is_empty() {
        tui::warning(format!(
            "No artifact yet: add one with {}",
            tui::accent(format!("rmra release upload {} <file>", release.name))
        ));
        return;
    }
    let mut table = tui::Table::new(["artifact", "size", "for devices", "sha256"]);
    for artifact in &release.artifacts {
        table.row(
            [
                artifact.file_name.clone(),
                tui::bytes(artifact.content_length),
                if artifact.tag_condition.is_empty() {
                    "all".to_owned()
                } else {
                    artifact.tag_condition.clone()
                },
                artifact.checksum_sha256.chars().take(12).collect(),
            ],
            false,
        );
    }
    table.print();
}

fn release_json(release: &Release) -> serde_json::Value {
    json!({
        "name": release.name,
        "version": release.version,
        "labels": release.labels,
        "artifacts": release.artifacts.iter().map(|a| json!({
            "fileName": a.file_name,
            "contentType": a.content_type,
            "contentLength": a.content_length,
            "checksumSha256": a.checksum_sha256,
            "tagCondition": a.tag_condition,
        })).collect::<Vec<_>>(),
    })
}
