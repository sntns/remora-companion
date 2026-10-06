use std::collections::BTreeMap;

use remora_factory::adapter::provisioning::ProvisionedIdentity;

/// Names a claim: the lowercase hex SHA-256 of its CSR's DER
/// `SubjectPublicKeyInfo` (see `remora_factory::model::VerifiedCsr`). A
/// device retrying with the key it persisted lands on the same claim, so a
/// retry never allocates a second serial.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClaimId(String);

impl ClaimId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ClaimId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a device knows about itself before it has an identity: enough to
/// pick its board's policy, fill a `device-name` template, and trace the
/// serial it gets back to the hardware in the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HardwareInfo {
    /// The machine type (`hub-v2`): which of the station's boards applies.
    pub board: String,
    /// The hostname the device runs under until it reboots with its
    /// serial (its Ethernet MAC, else its BSP serial).
    pub temp_hostname: String,
    pub eth_mac: Option<String>,
    pub bsp_serial: Option<String>,
    pub machine_id: Option<String>,
    /// Every other interface's MAC, by interface name.
    pub macs: BTreeMap<String, String>,
}

/// The image the device booted, for the journal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageInfo {
    pub version: Option<String>,
    pub compatible: Option<String>,
}

/// The longest a field a device reports about itself may be: room for any
/// MAC, serial, machine-id or version, and no more.
pub const FIELD_MAX: usize = 64;
/// How many interfaces' MACs a device may report.
pub const MACS_MAX: usize = 32;
/// The longest interface name (`wlan0`, `bat0`, `usb0`...).
pub const INTERFACE_MAX: usize = 16;
/// How much of a `failed` ack's reason is kept.
pub const REASON_MAX: usize = 512;

/// Why a device's report about itself is refused. Whatever a device says
/// reaches the operator's terminal, the hooks' environment, the journal
/// and `device-name` templates, from anyone on the workshop network: only
/// short, plain identifiers get that far. An empty value is no value (a
/// hub without Ethernet, an image whose version it can't read), and is
/// fine anywhere but `board`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FieldError {
    #[error("{0} is empty")]
    Empty(String),
    #[error("{0} is longer than {1} characters")]
    TooLong(String, usize),
    #[error("{0} holds characters other than {1}")]
    Charset(String, &'static str),
    #[error("more than {MACS_MAX} MACs")]
    TooManyMacs,
}

const VALUE_CHARSET: &str = "letters, digits, ':', '.', '_', '+' and '-'";
const INTERFACE_CHARSET: &str = "letters, digits, '.', '_' and '-'";

/// `value`, as field `name`: up to `max` characters, each of `charset`
/// (described as `described`).
fn check(
    name: &str,
    value: &str,
    max: usize,
    charset: fn(char) -> bool,
    described: &'static str,
) -> Result<(), FieldError> {
    if value.chars().count() > max {
        return Err(FieldError::TooLong(name.to_string(), max));
    }
    if !value.chars().all(charset) {
        return Err(FieldError::Charset(name.to_string(), described));
    }
    Ok(())
}

/// Up to FIELD_MAX of `[A-Za-z0-9:._+-]` (a version may read `1.4.0+git`).
fn check_value(name: &str, value: &str) -> Result<(), FieldError> {
    check(
        name,
        value,
        FIELD_MAX,
        |c| c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '+' | '-'),
        VALUE_CHARSET,
    )
}

fn check_optional(name: &str, value: Option<&str>) -> Result<(), FieldError> {
    value.map_or(Ok(()), |value| check_value(name, value))
}

impl HardwareInfo {
    /// Every field a plain identifier (see `FieldError`), `board` present.
    pub fn validate(&self) -> Result<(), FieldError> {
        if self.board.is_empty() {
            return Err(FieldError::Empty("hardware.board".into()));
        }
        check_value("hardware.board", &self.board)?;
        check_value("hardware.temp_hostname", &self.temp_hostname)?;
        check_optional("hardware.eth_mac", self.eth_mac.as_deref())?;
        check_optional("hardware.bsp_serial", self.bsp_serial.as_deref())?;
        check_optional("hardware.machine_id", self.machine_id.as_deref())?;
        if self.macs.len() > MACS_MAX {
            return Err(FieldError::TooManyMacs);
        }
        for (interface, mac) in &self.macs {
            if interface.is_empty() {
                return Err(FieldError::Empty("hardware.macs".into()));
            }
            check(
                "hardware.macs",
                interface,
                INTERFACE_MAX,
                |c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'),
                INTERFACE_CHARSET,
            )?;
            check_value(&format!("hardware.macs.{interface}"), mac)?;
        }
        Ok(())
    }
}

impl ImageInfo {
    pub fn validate(&self) -> Result<(), FieldError> {
        check_optional("image.version", self.version.as_deref())?;
        check_optional("image.compatible", self.compatible.as_deref())
    }
}

/// A device asking for an identity.
#[derive(Debug, Clone)]
pub struct ClaimRequest {
    /// DER PKCS#10, for the P-256 key the device generated and keeps.
    pub csr_der: Vec<u8>,
    pub hardware: HardwareInfo,
    pub image: ImageInfo,
}

impl ClaimRequest {
    /// What the device says about itself, checked before anyone sees it.
    pub fn validate(&self) -> Result<(), FieldError> {
        self.hardware.validate()?;
        self.image.validate()
    }
}

/// Where a claim stands once the platform has issued its identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimState {
    Issued,
    /// The device wrote its identity (its `installed` ack).
    Installed,
    /// The device rejected what it got (its `failed` ack).
    Failed,
}

/// Where an issued claim stands in the labelling queue. The device writes
/// its identity only once `Labelled`: validating the label is the commit
/// point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelState {
    /// Waiting for its turn.
    Queued,
    /// The one device being labelled: its LED is steady.
    Active,
    /// Its label was scanned and matched.
    Labelled,
}

/// What a device polls for.
#[derive(Debug, Clone)]
pub struct ClaimStatus {
    pub claim_id: ClaimId,
    pub state: ClaimState,
    pub label: LabelState,
    /// 0 for the active device, then 1, 2... behind it; none once
    /// labelled, or out of the queue.
    pub queue_position: Option<usize>,
    /// Everything public about the identity: the device holds the key.
    pub identity: ProvisionedIdentity,
}

/// A device's report once it has (or hasn't) written its identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ack {
    Installed,
    Failed { reason: String },
}

impl Ack {
    /// The same ack, its reason fit for a terminal, a hook's stdin and the
    /// journal: control characters (an escape sequence, a newline forging
    /// a line) turned into spaces, runs of spaces collapsed, and cut at
    /// REASON_MAX characters. A reason is free text, and a device's last
    /// word: it is cleaned rather than refused.
    pub fn sanitized(self) -> Self {
        match self {
            Self::Installed => Self::Installed,
            Self::Failed { reason } => Self::Failed {
                reason: reason
                    .split(|c: char| c.is_control() || c.is_whitespace())
                    .filter(|word| !word.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(REASON_MAX)
                    .collect(),
            },
        }
    }
}

/// What a device discovering the station checks it found one, and for
/// which environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    /// The context the station issues identities as.
    pub environment: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hardware() -> HardwareInfo {
        HardwareInfo {
            board: "hub-v2".into(),
            temp_hostname: "e2b4a1c09f13".into(),
            eth_mac: Some("e2:b4:a1:c0:9f:13".into()),
            bsp_serial: Some("f205fadcba7746a1".into()),
            machine_id: Some("1abf02e5bed34d63a097f3f38f0f6408".into()),
            macs: BTreeMap::from([
                ("wlan0".into(), "aa:bb:cc:dd:ee:ff".into()),
                ("bat0".into(), "aabbccddeeff".into()),
                ("wwan0".into(), String::new()),
            ]),
        }
    }

    #[test]
    fn real_hub_reports_are_valid_empty_values_included() {
        hardware().validate().unwrap();
        for (version, compatible) in [("1.4.0", "v2"), ("local-748efa3", "virtual"), ("", "")] {
            ImageInfo {
                version: Some(version.into()),
                compatible: Some(compatible.into()),
            }
            .validate()
            .unwrap();
        }
        // hub-virtual: no BSP serial, no Ethernet.
        HardwareInfo {
            board: "hub-virtual".into(),
            temp_hostname: "525400123456".into(),
            eth_mac: Some(String::new()),
            bsp_serial: Some(String::new()),
            machine_id: None,
            macs: BTreeMap::new(),
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn refuses_what_could_forge_a_terminal_line_or_a_hook_variable() {
        let refused = |change: fn(&mut HardwareInfo)| {
            let mut hardware = hardware();
            change(&mut hardware);
            hardware.validate().unwrap_err()
        };
        assert_eq!(
            refused(|h| h.temp_hostname = "\x1b[2J1H7Z".into()),
            FieldError::Charset("hardware.temp_hostname".into(), VALUE_CHARSET)
        );
        assert_eq!(
            refused(|h| h.bsp_serial = Some("a\nREMORA_SERIAL=1".into())),
            FieldError::Charset("hardware.bsp_serial".into(), VALUE_CHARSET)
        );
        assert_eq!(
            refused(|h| h.machine_id = Some("a".repeat(FIELD_MAX + 1))),
            FieldError::TooLong("hardware.machine_id".into(), FIELD_MAX)
        );
        assert_eq!(
            refused(|h| h.board = String::new()),
            FieldError::Empty("hardware.board".into())
        );
        assert_eq!(
            refused(|h| h.macs = (0..=MACS_MAX)
                .map(|n| (format!("eth{n}"), "x".into()))
                .collect()),
            FieldError::TooManyMacs
        );
        assert_eq!(
            refused(|h| {
                h.macs.insert("wlan1".into(), "$(reboot)".into());
            }),
            FieldError::Charset("hardware.macs.wlan1".into(), VALUE_CHARSET)
        );
        assert_eq!(
            refused(|h| {
                h.macs.insert("wlan:1".into(), "x".into());
            }),
            FieldError::Charset("hardware.macs".into(), INTERFACE_CHARSET)
        );
        assert_eq!(
            refused(|h| {
                h.macs.insert("a".repeat(INTERFACE_MAX + 1), "x".into());
            }),
            FieldError::TooLong("hardware.macs".into(), INTERFACE_MAX)
        );
    }

    #[test]
    fn a_failure_reason_is_cleaned_not_refused() {
        let ack = Ack::Failed {
            reason: "bad\x1b[2J chain\n\n  of trust ".to_string() + &"x".repeat(1000),
        }
        .sanitized();
        let Ack::Failed { reason } = ack else {
            unreachable!()
        };
        assert!(reason.starts_with("bad [2J chain of trust x"), "{reason}");
        assert_eq!(reason.chars().count(), REASON_MAX);
        assert!(!reason.chars().any(char::is_control));
    }
}
