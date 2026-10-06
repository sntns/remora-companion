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

/// A device asking for an identity.
#[derive(Debug, Clone)]
pub struct ClaimRequest {
    /// DER PKCS#10, for the P-256 key the device generated and keeps.
    pub csr_der: Vec<u8>,
    pub hardware: HardwareInfo,
    pub image: ImageInfo,
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

/// What a device discovering the station checks it found one, and for
/// which environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    /// The context the station issues identities as.
    pub environment: String,
}
