use std::{collections::HashMap, time::SystemTime};

use error_stack::{Report, ResultExt};
use remora_context::model::ContextOverride;
use remora_device::{
    application::DeviceService,
    model::{parse_labels, Device, FactoryDevice, Labels},
};
use remora_tui as tui;
use serde_json::json;

use crate::error::{device_error, prompt_error, Error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// List the account's devices and their labels.
    #[command(visible_alias = "ls")]
    List {
        /// Only devices carrying this label (key=value); repeatable, all
        /// must match.
        #[arg(long = "label", value_name = "KEY=VALUE")]
        labels: Vec<String>,
        /// Also list the devices the account manufactured that have no
        /// device yet (not enrolled): their factory record only. --label
        /// picks those by their factory labels. Adds when each device was
        /// manufactured.
        #[arg(long)]
        with_factory_devices: bool,
        /// `table` for humans, `json` for scripts.
        #[arg(long, default_value = "table")]
        format: Format,
        /// Only print device names (one call, however many devices).
        #[arg(short, long)]
        quiet: bool,
    },

    /// Delete devices from the platform: their operational record only --
    /// the factory one stays, so a device can enroll again.
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        #[arg(add = remora_completion::values(remora_completion::Kind::Device))]
        names: Vec<String>,
        /// Don't ask for confirmation.
        #[arg(short, long)]
        #[arg(short_alias = 'f', alias = "force")]
        yes: bool,
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
            with_factory_devices,
            format,
            quiet,
        } => {
            let filter = parse_labels(&labels)
                .map_err(Report::new)
                .change_context(Error::Labels)?;
            let spinner = (!quiet).then(|| tui::Spinner::start("Listing devices"));
            // With factory devices: those not enrolled, and every record,
            // for when the enrolled ones were manufactured.
            let everything = Labels::new();
            let (devices, waiting, made) = if with_factory_devices {
                let (devices, waiting, made) = futures_util::join!(
                    service.list(over, &filter, !quiet),
                    service.list_factory(over, &filter, true),
                    service.list_factory(over, &everything, false)
                );
                (devices, Some(waiting), Some(made))
            } else {
                (service.list(over, &filter, !quiet).await, None, None)
            };
            let devices = devices.map_err(device_error)?;
            let waiting = waiting.transpose().map_err(device_error)?;
            let made = made.transpose().map_err(device_error)?;
            drop(spinner);
            let manufactured: HashMap<_, _> = made
                .unwrap_or_default()
                .into_iter()
                .filter_map(|device| Some((device.name, device.created_at?)))
                .collect();
            let rows = rows(devices, waiting.unwrap_or_default(), &manufactured);
            if quiet {
                rows.iter().for_each(|row| println!("{}", row.name()));
                return Ok(());
            }
            match format {
                Format::Json => print_json(&json!(rows
                    .iter()
                    .map(|row| row.json(with_factory_devices))
                    .collect::<Vec<_>>())),
                Format::Table if rows.is_empty() => {
                    tui::info(match (filter.is_empty(), with_factory_devices) {
                        (true, false) => "No device in this account.",
                        (true, true) => {
                            "No device in this account, and none made that waits for one."
                        }
                        (false, _) => "No device matches these labels.",
                    })
                }
                Format::Table => {
                    let mut headers = vec!["name", "labels"];
                    if with_factory_devices {
                        headers.extend(["state", "created"]);
                    }
                    headers.push("enrolled");
                    let mut table = tui::Table::new(headers);
                    for row in &rows {
                        // Toned cells go through the table, not as escape
                        // codes: those would count in the column widths.
                        let mut cells = vec![(row.name().to_owned(), None)];
                        match row {
                            Row::Device { device, created_at } => {
                                cells.push((labels_line(device.labels.iter().flatten()), None));
                                if with_factory_devices {
                                    cells.push(("enrolled".to_owned(), Some(tui::Tone::Good)));
                                    cells.push((time_cell(*created_at), None));
                                }
                                cells.push((time_cell(device.enrolled_at), None));
                            }
                            Row::Factory(device) => {
                                let (state, tone) = factory_state(device);
                                cells.push((String::new(), None));
                                cells.push((state.to_owned(), Some(tone)));
                                cells.push((time_cell(device.created_at), None));
                                cells.push((String::new(), None));
                            }
                        }
                        table.row_toned(cells);
                    }
                    table.print();
                }
            }
            Ok(())
        }
        Command::Remove { names, yes } => {
            for name in names {
                if !yes && tui::interactive() {
                    let question = format!(
                        "Delete device {} from the platform? It loses its access until it \
                         enrolls again.",
                        tui::accent(&name)
                    );
                    if !tui::confirm(question)
                        .initial_value(false)
                        .interact()
                        .map_err(prompt_error)?
                    {
                        tui::info(format!("Kept {}", tui::accent(&name)));
                        continue;
                    }
                }
                service.delete(over, &name).await.map_err(device_error)?;
                tui::success(format!("Deleted device {}", tui::accent(&name)));
            }
            Ok(())
        }
    }
}

/// A line of `rmra device ls`: a device (and when it was manufactured,
/// when its factory record says), or a factory record with no device.
enum Row {
    Device {
        device: Device,
        created_at: Option<SystemTime>,
    },
    Factory(FactoryDevice),
}

impl Row {
    fn name(&self) -> &str {
        match self {
            Row::Device { device, .. } => &device.name,
            Row::Factory(device) => &device.name,
        }
    }

    /// When it was manufactured, when known.
    fn created_at(&self) -> Option<SystemTime> {
        match self {
            Row::Device { created_at, .. } => *created_at,
            Row::Factory(device) => device.created_at,
        }
    }

    /// `enrolled`, `createdAt` and `owner` only with --with-factory-devices.
    fn json(&self, with_factory_devices: bool) -> serde_json::Value {
        match (self, with_factory_devices) {
            (Row::Device { device: d, .. }, false) => json!({
                "name": d.name,
                "labels": d.labels,
                "enrolledAt": d.enrolled_at.map(tui::timestamp_utc),
            }),
            (
                Row::Device {
                    device: d,
                    created_at,
                },
                true,
            ) => json!({
                "name": d.name,
                "labels": d.labels,
                "enrolled": true,
                "createdAt": created_at.map(tui::timestamp_utc),
                "enrolledAt": d.enrolled_at.map(tui::timestamp_utc),
            }),
            (Row::Factory(d), _) => json!({
                "name": d.name,
                "labels": null,
                "enrolled": false,
                "owner": d.owner,
                "createdAt": d.created_at.map(tui::timestamp_utc),
                "enrolledAt": null,
            }),
        }
    }
}

/// Both listings merged, oldest manufactured first; `manufactured` dates
/// the devices. Undated ones come first (made before the platform kept the
/// date), then by name -- all of them, without --with-factory-devices.
fn rows(
    devices: Vec<Device>,
    factory: Vec<FactoryDevice>,
    manufactured: &HashMap<String, SystemTime>,
) -> Vec<Row> {
    let mut rows: Vec<_> = devices
        .into_iter()
        .map(|device| Row::Device {
            created_at: manufactured.get(&device.name).copied(),
            device,
        })
        .chain(factory.into_iter().map(Row::Factory))
        .collect();
    rows.sort_by(|a, b| (a.created_at(), a.name()).cmp(&(b.created_at(), b.name())));
    rows
}

/// A date in a table: empty where the platform has none (not applicable,
/// or older than the platform keeping it).
pub(crate) fn time_cell(time: Option<SystemTime>) -> String {
    time.map(tui::timestamp).unwrap_or_default()
}

/// Where a device without an operational record stands.
pub(crate) fn factory_state(device: &FactoryDevice) -> (&'static str, tui::Tone) {
    match device.owner {
        Some(_) => ("not enrolled", tui::Tone::Warn),
        None => ("in stock", tui::Tone::Idle),
    }
}

pub(crate) fn labels_line<'a>(
    labels: impl IntoIterator<Item = (&'a String, &'a String)>,
) -> String {
    labels
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn print_json(value: &serde_json::Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("json values serialize")
    );
}
