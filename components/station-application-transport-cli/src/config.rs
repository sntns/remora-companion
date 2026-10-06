//! `station.yaml`, and the `serve` options that override it key for key.

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use error_stack::{Report, ResultExt};
use remora_station::model::{BoardPolicy, ConfirmMode, StationConfig};
use serde::Deserialize;

use crate::error::{Error, Result};

pub(crate) const DEFAULT_LISTEN: &str = "0.0.0.0:8484";
const DEFAULT_JOURNAL: &str = "station.jsonl";
const DEFAULT_HOOKS: &str = "station.d";

/// The file, as written: every key optional, unknown keys refused (a typo
/// must not silently fall back to a default -- `force` included, which a
/// station never sends).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct StationFile {
    listen: Option<SocketAddr>,
    journal: Option<PathBuf>,
    hooks: Option<PathBuf>,
    max_claims: Option<u32>,
    confirm: Option<Confirm>,
    presence_timeout: Option<String>,
    hook_timeout: Option<String>,
    #[serde(default)]
    boards: BTreeMap<String, BoardFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct BoardFile {
    create_factory_device: CreateFactoryDevice,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct CreateFactoryDevice {
    serial_number_policy: Option<String>,
    device_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Confirm {
    /// Scan the label stuck on the hub: it must read its serial.
    Scan,
    /// Enter alone validates (bench work only).
    Key,
}

/// What the command line overrides.
#[derive(Debug, Default)]
pub(crate) struct Overrides {
    pub listen: Option<SocketAddr>,
    pub journal: Option<PathBuf>,
    pub hooks: Option<PathBuf>,
    pub max_claims: Option<u32>,
    pub confirm: Option<Confirm>,
    pub presence_timeout: Option<Duration>,
    pub hook_timeout: Option<Duration>,
    /// `BOARD=POLICY`, each replacing that board's settings.
    pub serial_policies: Vec<String>,
    /// `BOARD=TEMPLATE`, likewise.
    pub device_names: Vec<String>,
}

pub(crate) fn read(path: &Path) -> Result<StationFile> {
    let text =
        std::fs::read_to_string(path).change_context_lazy(|| Error::ReadConfig(path.into()))?;
    yaml_serde::from_str(&text).change_context_lazy(|| Error::ReadConfig(path.into()))
}

fn invalid(message: impl Into<String>) -> Report<Error> {
    Report::new(Error::Config(message.into()))
}

fn duration(key: &str, text: Option<String>, default: Duration) -> Result<Duration> {
    match text {
        None => Ok(default),
        Some(text) => humantime::parse_duration(&text)
            .map_err(|e| invalid(format!("{key}: {text:?} is not a duration ({e})"))),
    }
}

fn board_value<'a>(option: &str, value: &'a str) -> Result<(&'a str, &'a str)> {
    value
        .split_once('=')
        .filter(|(board, rest)| !board.is_empty() && !rest.is_empty())
        .ok_or_else(|| invalid(format!("--{option} {value:?}: expected BOARD=VALUE")))
}

/// The address to listen on, and the station's settings: the file's (its
/// relative paths taken from its own directory), each overridden by the
/// command line (relative to the working directory), else the defaults.
pub(crate) fn resolve(
    file: StationFile,
    base: &Path,
    overrides: Overrides,
) -> Result<(SocketAddr, StationConfig)> {
    let in_file = |path: PathBuf| {
        if path.is_relative() {
            base.join(path)
        } else {
            path
        }
    };
    let listen = overrides
        .listen
        .or(file.listen)
        .unwrap_or_else(|| DEFAULT_LISTEN.parse().expect("the default address parses"));

    let mut boards = BTreeMap::new();
    for (board, settings) in file.boards {
        let CreateFactoryDevice {
            serial_number_policy,
            device_name,
        } = settings.create_factory_device;
        let policy = BoardPolicy::from_parts(serial_number_policy, device_name)
            .map_err(|e| invalid(format!("boards.{board}.create-factory-device: {e}")))?;
        boards.insert(board, policy);
    }
    for value in &overrides.serial_policies {
        let (board, policy) = board_value("serial-policy", value)?;
        boards.insert(
            board.to_string(),
            BoardPolicy::SerialNumberPolicy(policy.to_string()),
        );
    }
    for value in &overrides.device_names {
        let (board, template) = board_value("device-name", value)?;
        let policy = BoardPolicy::from_parts(None, Some(template.to_string()))
            .map_err(|e| invalid(format!("--device-name {value:?}: {e}")))?;
        boards.insert(board.to_string(), policy);
    }

    let config = StationConfig {
        journal: overrides
            .journal
            .or_else(|| file.journal.map(in_file))
            .unwrap_or_else(|| base.join(DEFAULT_JOURNAL)),
        hooks: overrides
            .hooks
            .or_else(|| file.hooks.map(in_file))
            .unwrap_or_else(|| base.join(DEFAULT_HOOKS)),
        max_claims: overrides.max_claims.or(file.max_claims),
        confirm: match overrides.confirm.or(file.confirm) {
            Some(Confirm::Key) => ConfirmMode::Key,
            Some(Confirm::Scan) | None => ConfirmMode::Scan,
        },
        presence_timeout: match overrides.presence_timeout {
            Some(timeout) => timeout,
            None => duration(
                "presence-timeout",
                file.presence_timeout,
                StationConfig::DEFAULT_PRESENCE_TIMEOUT,
            )?,
        },
        hook_timeout: match overrides.hook_timeout {
            Some(timeout) => timeout,
            None => duration(
                "hook-timeout",
                file.hook_timeout,
                StationConfig::DEFAULT_HOOK_TIMEOUT,
            )?,
        },
        boards,
    };
    if config.presence_timeout.is_zero() || config.hook_timeout.is_zero() {
        return Err(invalid("timeouts must be longer than zero"));
    }
    Ok((listen, config))
}

#[cfg(test)]
mod tests {
    use remora_station::model::DeviceNameTemplate;

    use super::*;

    const SPEC_EXAMPLE: &str = r#"
listen: 0.0.0.0:8484
journal: ./station.jsonl          # rechargé au démarrage
hooks: ./station.d
max-claims: 50
confirm: scan
presence-timeout: 10s

boards:
  hub-v2:
    create-factory-device:
      serial-number-policy: hubs-v2
  hub-v1:
    create-factory-device:
      device-name: "{bsp_serial}"
"#;

    #[test]
    fn reads_the_documented_example() {
        let file: StationFile = yaml_serde::from_str(SPEC_EXAMPLE).unwrap();
        let (listen, config) =
            resolve(file, Path::new("/srv/station"), Overrides::default()).unwrap();
        assert_eq!(listen.to_string(), "0.0.0.0:8484");
        assert_eq!(config.journal, Path::new("/srv/station/./station.jsonl"));
        assert_eq!(config.hooks, Path::new("/srv/station/./station.d"));
        assert_eq!(config.max_claims, Some(50));
        assert_eq!(config.confirm, ConfirmMode::Scan);
        assert_eq!(config.presence_timeout, Duration::from_secs(10));
        assert_eq!(config.hook_timeout, Duration::from_secs(60));
        assert_eq!(
            config.boards["hub-v2"],
            BoardPolicy::SerialNumberPolicy("hubs-v2".into())
        );
        assert_eq!(
            config.boards["hub-v1"],
            BoardPolicy::DeviceName(DeviceNameTemplate::parse("{bsp_serial}").unwrap())
        );
    }

    #[test]
    fn the_command_line_overrides_the_file() {
        let file: StationFile = yaml_serde::from_str(SPEC_EXAMPLE).unwrap();
        let overrides = Overrides {
            listen: Some("127.0.0.1:9000".parse().unwrap()),
            journal: Some("other.jsonl".into()),
            max_claims: Some(3),
            confirm: Some(Confirm::Key),
            presence_timeout: Some(Duration::from_secs(2)),
            serial_policies: vec!["hub-v1=hubs-v1".into()],
            device_names: vec!["hub-virtual={temp_hostname}".into()],
            ..Overrides::default()
        };
        let (listen, config) = resolve(file, Path::new("/srv"), overrides).unwrap();
        assert_eq!(listen.to_string(), "127.0.0.1:9000");
        assert_eq!(config.journal, Path::new("other.jsonl"));
        assert_eq!(config.max_claims, Some(3));
        assert_eq!(config.confirm, ConfirmMode::Key);
        assert_eq!(config.presence_timeout, Duration::from_secs(2));
        assert_eq!(
            config.boards["hub-v1"],
            BoardPolicy::SerialNumberPolicy("hubs-v1".into())
        );
        assert_eq!(config.boards.len(), 3);
    }

    #[test]
    fn refuses_what_it_cannot_honour() {
        let both = "boards:\n  hub-v2:\n    create-factory-device:\n      serial-number-policy: a\n      device-name: b\n";
        let file: StationFile = yaml_serde::from_str(both).unwrap();
        assert!(resolve(file, Path::new("."), Overrides::default()).is_err());

        let template =
            "boards:\n  hub-v2:\n    create-factory-device:\n      device-name: \"{serial}\"\n";
        let file: StationFile = yaml_serde::from_str(template).unwrap();
        assert!(resolve(file, Path::new("."), Overrides::default()).is_err());

        let force = "boards:\n  hub-v2:\n    create-factory-device:\n      device-name: x\n      force: true\n";
        assert!(yaml_serde::from_str::<StationFile>(force).is_err());
        assert!(yaml_serde::from_str::<StationFile>("listn: 1.2.3.4:5\n").is_err());

        let file: StationFile = yaml_serde::from_str("presence-timeout: soon\n").unwrap();
        assert!(resolve(file, Path::new("."), Overrides::default()).is_err());
    }
}
