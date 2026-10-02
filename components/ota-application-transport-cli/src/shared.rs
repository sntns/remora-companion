use error_stack::Report;
use remora_ota::model::{DeploymentStatus, Labels};
use remora_tui::Tone;

use crate::error::{Error, Result};

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    Table,
    Json,
}

/// `key=value` pairs, as `--label`/`--selector` take them.
pub fn parse_labels(pairs: &[String]) -> Result<Labels> {
    pairs
        .iter()
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
            _ => Err(Report::new(Error::Label(pair.clone()))),
        })
        .collect()
}

pub fn status_tone(status: &DeploymentStatus) -> Tone {
    match status {
        DeploymentStatus::Succeeded => Tone::Good,
        DeploymentStatus::Failed | DeploymentStatus::Rejected => Tone::Bad,
        DeploymentStatus::Running => Tone::Active,
        DeploymentStatus::Canceling | DeploymentStatus::Canceled => Tone::Warn,
        DeploymentStatus::Pending | DeploymentStatus::Other(_) => Tone::Idle,
    }
}

pub fn print_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("json values serialize")
    );
}

pub fn labels_line(labels: &Labels) -> String {
    labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}
