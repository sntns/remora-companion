use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use remora_factory::model::DeviceSerial;

use super::claim::HardwareInfo;

/// How the operator confirms the active device's label.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConfirmMode {
    /// Scan the label stuck on the device: it must read the device's
    /// serial. The production mode.
    #[default]
    Scan,
    /// Enter alone validates, unchecked: for bench work only.
    Key,
}

/// A station's settings, whatever they were read from.
#[derive(Debug, Clone)]
pub struct StationConfig {
    /// Where every transition is appended, and reloaded from at start.
    pub journal: PathBuf,
    /// Holds the `<event>.d/` hook directories.
    pub hooks: PathBuf,
    /// How many identities this run may issue at most.
    pub max_claims: Option<u32>,
    pub confirm: ConfirmMode,
    /// The active device is put back in the queue after this long without
    /// polling (unplugged, crashed), and a queued one isn't activated
    /// until it polls again.
    pub presence_timeout: Duration,
    /// How long each hook script may run.
    pub hook_timeout: Duration,
    /// The boards accepted, and how each one's devices are named. Any
    /// other board is refused.
    pub boards: BTreeMap<String, BoardPolicy>,
}

impl StationConfig {
    pub const DEFAULT_PRESENCE_TIMEOUT: Duration = Duration::from_secs(10);
    pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(60);
}

/// What `start` reports once the station is ready to serve.
#[derive(Debug, Clone)]
pub struct StationSummary {
    /// The context identities are issued as.
    pub context: String,
    /// Claims reloaded from the journal.
    pub restored: usize,
    /// Of those, the ones waiting for their label again.
    pub awaiting_label: usize,
    /// How often `StationServiceInterface::tick` must run: often enough
    /// to notice a silent active device well within `presence-timeout`.
    pub tick: Duration,
}

/// How a board's devices get their serial on the platform -- the two
/// shapes `DeviceSerial` takes, never `force`: re-issuing an existing
/// device is after-sales work, not a station's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoardPolicy {
    /// The platform allocates a fresh serial from this policy.
    SerialNumberPolicy(String),
    /// The serial is the device's own hardware, through a template.
    DeviceName(DeviceNameTemplate),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoardPolicyError {
    #[error("give either serial-number-policy or device-name, not both")]
    Both,
    #[error("give serial-number-policy or device-name")]
    Neither,
    #[error(transparent)]
    Template(#[from] TemplateError),
}

impl BoardPolicy {
    /// Exactly one of the two, like `DeviceSerial::from_parts`; a template
    /// is checked here, so a bad one stops the station before it serves.
    pub fn from_parts(
        serial_number_policy: Option<String>,
        device_name: Option<String>,
    ) -> Result<Self, BoardPolicyError> {
        match (serial_number_policy, device_name) {
            (Some(_), Some(_)) => Err(BoardPolicyError::Both),
            (None, None) => Err(BoardPolicyError::Neither),
            (Some(policy), None) => Ok(Self::SerialNumberPolicy(policy)),
            (None, Some(template)) => Ok(Self::DeviceName(DeviceNameTemplate::parse(&template)?)),
        }
    }

    /// The serial to ask the platform for, for this device.
    pub fn serial_for(&self, hardware: &HardwareInfo) -> Result<DeviceSerial, TemplateError> {
        Ok(match self {
            Self::SerialNumberPolicy(policy) => DeviceSerial::FromPolicy(policy.clone()),
            Self::DeviceName(template) => DeviceSerial::Explicit {
                device_name: template.render(hardware)?,
                force: false,
            },
        })
    }
}

/// A claim's hardware field, as a `device-name` template names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardwareField {
    BspSerial,
    EthMac,
    TempHostname,
    Board,
}

impl HardwareField {
    const ALL: [Self; 4] = [
        Self::BspSerial,
        Self::EthMac,
        Self::TempHostname,
        Self::Board,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::BspSerial => "bsp_serial",
            Self::EthMac => "eth_mac",
            Self::TempHostname => "temp_hostname",
            Self::Board => "board",
        }
    }

    fn value(self, hardware: &HardwareInfo) -> Option<&str> {
        match self {
            Self::BspSerial => hardware.bsp_serial.as_deref(),
            Self::EthMac => hardware.eth_mac.as_deref(),
            Self::TempHostname => Some(&hardware.temp_hostname),
            Self::Board => Some(&hardware.board),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Field(HardwareField),
}

/// A `device-name` such as `"{bsp_serial}"` or `"hub-{eth_mac}"`: text,
/// with `{field}` replaced by the claim's hardware field of that name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceNameTemplate {
    source: String,
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("empty device-name template")]
    Empty,
    #[error("unknown field {{{0}}} in device-name template (known: {{bsp_serial}}, {{eth_mac}}, {{temp_hostname}}, {{board}})")]
    UnknownField(String),
    #[error("unbalanced brace in device-name template {0:?}")]
    Unbalanced(String),
    #[error("the device did not report {{{0}}}, which its board's device-name needs")]
    MissingField(&'static str),
}

impl DeviceNameTemplate {
    pub fn parse(source: &str) -> Result<Self, TemplateError> {
        let unbalanced = || TemplateError::Unbalanced(source.to_string());
        let mut parts = Vec::new();
        let mut rest = source;
        while !rest.is_empty() {
            match rest.find(['{', '}']) {
                Some(at) if rest[at..].starts_with('}') => return Err(unbalanced()),
                Some(at) => {
                    if at > 0 {
                        parts.push(Part::Literal(rest[..at].to_string()));
                    }
                    let close = rest[at..].find('}').ok_or_else(unbalanced)? + at;
                    let name = &rest[at + 1..close];
                    if name.contains('{') {
                        return Err(unbalanced());
                    }
                    let field = HardwareField::ALL
                        .into_iter()
                        .find(|field| field.name() == name)
                        .ok_or_else(|| TemplateError::UnknownField(name.to_string()))?;
                    parts.push(Part::Field(field));
                    rest = &rest[close + 1..];
                }
                None => {
                    parts.push(Part::Literal(rest.to_string()));
                    rest = "";
                }
            }
        }
        if parts.is_empty() {
            return Err(TemplateError::Empty);
        }
        Ok(Self {
            source: source.to_string(),
            parts,
        })
    }

    /// The template as written.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// This device's name; a field it didn't report (or left empty) is an
    /// error rather than a name with a hole in it.
    pub fn render(&self, hardware: &HardwareInfo) -> Result<String, TemplateError> {
        let mut name = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(text) => name.push_str(text),
                Part::Field(field) => name.push_str(
                    field
                        .value(hardware)
                        .filter(|value| !value.is_empty())
                        .ok_or(TemplateError::MissingField(field.name()))?,
                ),
            }
        }
        Ok(name)
    }
}

impl std::fmt::Display for DeviceNameTemplate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hardware() -> HardwareInfo {
        HardwareInfo {
            board: "hub-v1".into(),
            temp_hostname: "e2b4a1c09f13".into(),
            eth_mac: Some("e2:b4:a1:c0:9f:13".into()),
            bsp_serial: Some("c3d2".into()),
            ..Default::default()
        }
    }

    #[test]
    fn templates_fill_in_hardware_fields() {
        let template = DeviceNameTemplate::parse("{board}-{bsp_serial}").unwrap();
        assert_eq!(template.render(&hardware()).unwrap(), "hub-v1-c3d2");
        let template = DeviceNameTemplate::parse("{temp_hostname}").unwrap();
        assert_eq!(template.render(&hardware()).unwrap(), "e2b4a1c09f13");
        let template = DeviceNameTemplate::parse("fixed").unwrap();
        assert_eq!(template.render(&hardware()).unwrap(), "fixed");
        assert_eq!(template.source(), "fixed");
    }

    #[test]
    fn templates_refuse_what_they_cannot_fill() {
        assert_eq!(
            DeviceNameTemplate::parse("{serial}"),
            Err(TemplateError::UnknownField("serial".into()))
        );
        assert!(matches!(
            DeviceNameTemplate::parse("{bsp_serial"),
            Err(TemplateError::Unbalanced(_))
        ));
        assert!(matches!(
            DeviceNameTemplate::parse("a}b"),
            Err(TemplateError::Unbalanced(_))
        ));
        assert_eq!(DeviceNameTemplate::parse(""), Err(TemplateError::Empty));

        let template = DeviceNameTemplate::parse("{eth_mac}").unwrap();
        let mut bare = hardware();
        bare.eth_mac = Some(String::new());
        assert_eq!(
            template.render(&bare),
            Err(TemplateError::MissingField("eth_mac"))
        );
    }

    #[test]
    fn a_board_names_its_serial_one_way_never_forced() {
        assert_eq!(
            BoardPolicy::from_parts(Some("hubs".into()), Some("{board}".into())),
            Err(BoardPolicyError::Both)
        );
        assert_eq!(
            BoardPolicy::from_parts(None, None),
            Err(BoardPolicyError::Neither)
        );
        let policy = BoardPolicy::from_parts(Some("hubs-v2".into()), None).unwrap();
        assert_eq!(
            policy.serial_for(&hardware()).unwrap(),
            DeviceSerial::FromPolicy("hubs-v2".into())
        );
        let named = BoardPolicy::from_parts(None, Some("{bsp_serial}".into())).unwrap();
        assert_eq!(
            named.serial_for(&hardware()).unwrap(),
            DeviceSerial::Explicit {
                device_name: "c3d2".into(),
                force: false
            }
        );
    }
}
