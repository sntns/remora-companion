use std::path::{Path, PathBuf};

use error_stack::ResultExt;
use remora_channel::{
    application::ChannelService,
    model::{LocalCertificateRequest, LocalSshCertificate},
};
use remora_context::model::ContextOverride;
use remora_tui as tui;
use serde_json::json;

use crate::{
    error::{channel_error, Error, Result},
    service::{Format, Role},
};

#[derive(clap::Args)]
#[command(after_help = "Example:\n  \
    rmra channel local-certificate --key ~/.ssh/id_ed25519 525400C0FFEE 525400C0FFEF\n  \
    ssh -i ~/.ssh/id_ed25519 -o UserKnownHostsFile=~/.ssh/id_ed25519-known_hosts \\\n      \
    -o HostKeyAlias=525400c0ffee.devices.sentiens root@192.168.1.20")]
pub struct LocalCertificateArgs {
    /// The devices' names (their serials), 1 to 64.
    #[arg(required = true, num_args = 1..=64)]
    #[arg(add = remora_completion::values(remora_completion::Kind::Device))]
    devices: Vec<String>,
    /// The private key to certify: its public half is read from KEY.pub,
    /// the certificate written to KEY-cert.pub, where ssh finds it.
    #[arg(long, value_name = "KEY")]
    #[arg(value_hint = clap::ValueHint::FilePath)]
    key: PathBuf,
    /// Where to write the devices' host authority, for ssh's
    /// UserKnownHostsFile. Default: KEY-known_hosts.
    #[arg(long, value_name = "PATH")]
    #[arg(value_hint = clap::ValueHint::FilePath)]
    known_hosts: Option<PathBuf>,
    /// The role to be on the devices; each is its own IAM action.
    #[arg(long, value_enum, default_value = "user")]
    role: Role,
    /// How long the certificate is valid for. Default: the account's
    /// (8 h unless changed); more than the account allows is refused.
    #[arg(long, value_name = "HOURS", value_parser = clap::value_parser!(u32).range(1..))]
    validity_hours: Option<u32>,
    /// `table` for humans, `json` for scripts.
    #[arg(long, default_value = "table")]
    format: Format,
}

/// `rmra channel local-certificate`: certify the operator's own key for
/// direct ssh to devices on their local network, and write what plain ssh
/// needs next to it.
pub async fn run(
    args: LocalCertificateArgs,
    service: &ChannelService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let public_path = suffixed(&args.key, ".pub");
    let public_key = std::fs::read_to_string(&public_path)
        .change_context_lazy(|| Error::Read(public_path.clone()))?;
    let role = args.role.into();

    let spinner = tui::Spinner::start(format!(
        "Certifying {} for {} {}",
        tui::accent(public_path.display()),
        tui::accent(args.devices.join(", ")),
        tui::dim(format!("as local-{}", args.role.as_str()))
    ));
    let issued = service
        .certify_local(LocalCertificateRequest {
            over: over.cloned(),
            devices: args.devices,
            public_key: public_key.trim().to_owned(),
            role,
            validity_hours: args.validity_hours,
        })
        .await;
    let issued = match issued {
        Ok(issued) => issued,
        Err(report) => {
            spinner.fail("Not certified");
            return Err(channel_error(report));
        }
    };

    let certificate_path = suffixed(&args.key, "-cert.pub");
    let known_hosts_path = args
        .known_hosts
        .unwrap_or_else(|| suffixed(&args.key, "-known_hosts"));
    write(&certificate_path, &issued.certificate)?;
    write(&known_hosts_path, &issued.known_hosts)?;
    spinner.done(match issued.valid_before {
        Some(until) => format!("Certified until {}", tui::timestamp(until)),
        None => "Certified".to_owned(),
    });

    print(
        &issued,
        &args.key,
        &certificate_path,
        &known_hosts_path,
        args.format,
    );
    Ok(())
}

fn print(
    issued: &LocalSshCertificate,
    key: &Path,
    certificate_path: &Path,
    known_hosts_path: &Path,
    format: Format,
) {
    match format {
        Format::Json => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "certificate": certificate_path,
                "known-hosts": known_hosts_path,
                "valid-before": issued.valid_before.map(tui::timestamp),
                "devices": issued.devices.iter().map(|device| json!({
                    "name": device.device,
                    "user": device.user,
                    "host-key-alias": device.host_key_alias,
                })).collect::<Vec<_>>(),
            }))
            .expect("json values serialize")
        ),
        Format::Table => {
            let mut table = tui::Table::new(["device", "user", "host key alias"]);
            for device in &issued.devices {
                table.row(
                    [
                        device.device.clone(),
                        device.user.clone(),
                        device.host_key_alias.clone(),
                    ],
                    false,
                );
            }
            table.print();
            if let Some(first) = issued.devices.first() {
                // The host certificate names the alias, not the address the
                // device is reached at: ssh must be told which to check.
                tui::note(
                    "Log in with",
                    format!(
                        "ssh -i {} -o UserKnownHostsFile={} -o HostKeyAlias={} {}@<device address>",
                        key.display(),
                        known_hosts_path.display(),
                        first.host_key_alias,
                        first.user
                    ),
                );
            }
        }
    }
}

/// `path` with `suffix` appended to its file name: `id.pub`, `id-cert.pub`.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn write(path: &Path, line: &str) -> Result<()> {
    std::fs::write(path, format!("{}\n", line.trim_end()))
        .change_context_lazy(|| Error::Write(path.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_files_after_the_key() {
        let key = Path::new("/home/ada/.ssh/id_ed25519");
        assert_eq!(
            suffixed(key, "-cert.pub"),
            Path::new("/home/ada/.ssh/id_ed25519-cert.pub")
        );
        assert_eq!(
            suffixed(key, ".pub"),
            Path::new("/home/ada/.ssh/id_ed25519.pub")
        );
    }
}
