use error_stack::Report;
use remora_context::model::ContextOverride;
use remora_device::{application::DeviceService, model::Labels};
use remora_tui as tui;
use serde_json::json;

use crate::error::{device_error, Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// List the account's devices and their labels.
    #[command(visible_alias = "ls")]
    List {
        /// Only devices carrying this label (key=value); repeatable, all
        /// must match.
        #[arg(long = "label", value_name = "KEY=VALUE")]
        labels: Vec<String>,
        /// `table` for humans, `json` for scripts.
        #[arg(long, default_value = "table")]
        format: Format,
        /// Only print device names (one call, however many devices).
        #[arg(short, long)]
        quiet: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    Table,
    Json,
}

pub async fn run(
    command: Command,
    service: &DeviceService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    match command {
        Command::List {
            labels,
            format,
            quiet,
        } => {
            let filter = parse_labels(&labels)?;
            let spinner = (!quiet).then(|| tui::Spinner::start("Listing devices"));
            let devices = match service.list(over, &filter, !quiet).await {
                Ok(devices) => devices,
                Err(report) => return Err(device_error(report)),
            };
            drop(spinner);
            if quiet {
                devices
                    .iter()
                    .for_each(|device| println!("{}", device.name));
                return Ok(());
            }
            match format {
                Format::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&json!(devices
                        .iter()
                        .map(|d| json!({"name": d.name, "labels": d.labels}))
                        .collect::<Vec<_>>()))
                    .expect("json values serialize")
                ),
                Format::Table if devices.is_empty() => tui::info(if filter.is_empty() {
                    "No device in this account.".to_owned()
                } else {
                    "No device matches these labels.".to_owned()
                }),
                Format::Table => {
                    let mut table = tui::Table::new(["name", "labels"]);
                    for device in &devices {
                        let labels = device
                            .labels
                            .iter()
                            .flatten()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        table.row([device.name.clone(), labels], false);
                    }
                    table.print();
                }
            }
            Ok(())
        }
    }
}

fn parse_labels(pairs: &[String]) -> Result<Labels> {
    pairs
        .iter()
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) if !key.is_empty() => Ok((key.to_owned(), value.to_owned())),
            _ => Err(Report::new(Error::Label(pair.clone()))),
        })
        .collect()
}
