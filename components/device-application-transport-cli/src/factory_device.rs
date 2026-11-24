use error_stack::{Report, ResultExt};
use remora_context::model::ContextOverride;
use remora_device::{application::DeviceService, model::parse_labels};
use remora_tui as tui;
use serde_json::json;

use crate::{
    error::{device_error, prompt_error, Error, Result},
    service::{factory_state, labels_line, print_json, time_cell, Format},
};

#[derive(clap::Subcommand)]
pub enum FactoryDeviceCommand {
    /// List the devices the account manufactured, and whom each was
    /// assigned to.
    #[command(visible_alias = "ls")]
    List {
        /// Only factory devices carrying this label (key=value);
        /// repeatable, all must match.
        #[arg(long = "label", value_name = "KEY=VALUE")]
        labels: Vec<String>,
        /// Only those no device of the account has the name of yet: made,
        /// but not enrolled.
        #[arg(long)]
        without_device: bool,
        /// `table` for humans, `json` for scripts.
        #[arg(long, default_value = "table")]
        format: Format,
        /// Only print their names.
        #[arg(short, long)]
        quiet: bool,
    },

    /// Show a manufactured device: its owner, labels and factory
    /// certificate.
    Show {
        /// The device's name (its serial).
        name: String,
        /// `table` for humans, `json` for scripts.
        #[arg(long, default_value = "table")]
        format: Format,
    },

    /// Delete manufactured devices' factory records and revoke the
    /// certificate each was built with -- for hardware that never shipped:
    /// it can no longer enroll. A device already enrolled stays.
    #[command(visible_alias = "rm")]
    Remove {
        /// The devices' names (their serials).
        #[arg(required = true)]
        names: Vec<String>,
        /// Don't ask for confirmation.
        #[arg(short, long)]
        #[arg(short_alias = 'f', alias = "force")]
        yes: bool,
    },
}

pub async fn run_factory_device(
    command: FactoryDeviceCommand,
    service: &DeviceService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    match command {
        FactoryDeviceCommand::List {
            labels,
            without_device,
            format,
            quiet,
        } => {
            let filter = parse_labels(&labels)
                .map_err(Report::new)
                .change_context(Error::Labels)?;
            let spinner = (!quiet).then(|| tui::Spinner::start("Listing factory devices"));
            let mut devices = service
                .list_factory(over, &filter, without_device)
                .await
                .map_err(device_error)?;
            // Oldest manufactured first; undated ones (made before the
            // platform kept the date) before them, then by name.
            devices.sort_by(|a, b| (a.created_at, &a.name).cmp(&(b.created_at, &b.name)));
            drop(spinner);
            if quiet {
                devices
                    .iter()
                    .for_each(|device| println!("{}", device.name));
                return Ok(());
            }
            match format {
                Format::Json => print_json(&json!(devices
                    .iter()
                    .map(|d| {
                        json!({
                            "name": d.name,
                            "owner": d.owner,
                            "createdAt": d.created_at.map(tui::timestamp_utc),
                        })
                    })
                    .collect::<Vec<_>>())),
                Format::Table if devices.is_empty() => {
                    tui::info(match (filter.is_empty(), without_device) {
                        (true, false) => "No device manufactured by this account.",
                        (true, true) => "Every device this account made has enrolled.",
                        (false, _) => "No factory device matches these labels.",
                    })
                }
                Format::Table => {
                    let mut table = tui::Table::new(["name", "owner", "created"]);
                    for device in &devices {
                        // Toned cells go through the table, not as escape
                        // codes: those would count in the column widths.
                        let owner = match &device.owner {
                            Some(owner) => (owner.clone(), None),
                            None => {
                                let (state, tone) = factory_state(device);
                                (state.to_owned(), Some(tone))
                            }
                        };
                        table.row_toned([
                            (device.name.clone(), None),
                            owner,
                            (time_cell(device.created_at), None),
                        ]);
                    }
                    table.print();
                }
            }
            Ok(())
        }
        FactoryDeviceCommand::Show { name, format } => {
            let device = service
                .get_factory(over, &name)
                .await
                .map_err(device_error)?;
            match format {
                Format::Json => print_json(&json!({
                    "name": device.name,
                    "owner": device.owner,
                    "labels": device.labels,
                    "certificate": device.certificate,
                    "createdAt": device.created_at.map(tui::timestamp_utc),
                })),
                Format::Table => {
                    let mut body = format!(
                        "owner         {}",
                        device.owner.clone().unwrap_or_else(|| {
                            let (state, tone) = factory_state(&device);
                            tui::toned(state, tone)
                        })
                    );
                    if let Some(created_at) = device.created_at {
                        body.push_str(&format!("\ncreated       {}", tui::timestamp(created_at)));
                    }
                    if let Some(certificate) = &device.certificate {
                        body.push_str(&format!("\ncertificate   {certificate}"));
                    }
                    let labels = labels_line(device.labels.iter().flatten());
                    if !labels.is_empty() {
                        body.push_str(&format!("\nlabels        {labels}"));
                    }
                    tui::note(
                        format!("Factory device {}", tui::accent(&device.name)),
                        body,
                    );
                }
            }
            Ok(())
        }
        FactoryDeviceCommand::Remove { names, yes } => {
            for name in names {
                if !yes && tui::interactive() {
                    let question = format!(
                        "Delete factory device {} and revoke its factory certificate? It can \
                         no longer enroll.",
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
                let revoked = service
                    .delete_factory(over, &name)
                    .await
                    .map_err(device_error)?;
                tui::success(format!(
                    "Deleted factory device {}{}",
                    tui::accent(&name),
                    match revoked.len() {
                        0 => String::new(),
                        1 => ", its certificate revoked".to_owned(),
                        n => format!(", {n} certificates revoked"),
                    }
                ));
            }
            Ok(())
        }
    }
}
