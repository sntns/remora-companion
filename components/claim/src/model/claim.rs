use std::{path::PathBuf, time::Duration};

use remora_station::model::{ClaimId, HardwareInfo, ImageInfo};

/// A device claiming its identity: from which station, as what hardware,
/// and where its `remora-factory.yaml` goes.
#[derive(Debug, Clone)]
pub struct ClaimPlan {
    /// The station's base URL, e.g. `http://10.42.0.1:8484`.
    pub station: String,
    pub hardware: HardwareInfo,
    pub image: ImageInfo,
    pub output: PathBuf,
    /// How often it polls while queued, like a hub (1 s).
    pub poll_interval: Duration,
}

/// What a claim ended with: the device's identity written, and its ack
/// sent (at best).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimOutcome {
    pub claim_id: ClaimId,
    /// The serial issued: the label's, and the device's hostname to be.
    pub serial: String,
    /// The station's environment, as its hello said.
    pub environment: String,
    /// Whether the station got the `installed` ack (a lost one changes
    /// nothing for the device).
    pub acknowledged: bool,
}
