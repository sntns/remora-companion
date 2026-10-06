use crate::{
    adapter::hooks::{HookEvent, HookOutcome},
    model::ClaimId,
};

/// What the operator types (or scans: a barcode scanner reads as a
/// keyboard, a line then Enter), for the active device. Quitting is the
/// console's own business, not the station's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorInput {
    /// A scanned label (or anything typed that isn't a command).
    Scan(String),
    /// Enter alone: validates in `confirm: key` mode.
    Enter,
    /// `r`: print the active device's label again.
    Reprint,
    /// `s`: send the active device to the end of the queue.
    Skip,
    /// `f`: allow validating despite a failed label hook (the scan is
    /// still required).
    Force,
}

/// A device, as the operator sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubSummary {
    pub claim_id: ClaimId,
    /// Its serial, once issued.
    pub serial: Option<String>,
    pub board: String,
    pub temp_hostname: String,
    pub eth_mac: Option<String>,
}

/// The dashboard's counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    /// Claims being issued right now.
    pub searching: usize,
    pub issued: usize,
    /// Waiting for their label, the active one included.
    pub queued: usize,
    /// Labelled, not yet acknowledged installed.
    pub labelled: usize,
    pub installed: usize,
    pub failed: usize,
}

/// Why the active device's label can't be validated yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelBlock {
    /// `label.d` is still running.
    Printing,
    /// `label.d` failed: reprint (`r`) or force (`f`).
    PrintFailed,
}

/// What the station tells its operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorEvent {
    /// A new identity: its serial is the station's output.
    Issued {
        hub: HubSummary,
    },
    /// The device to label now: its LED is the steady one.
    Active {
        hub: HubSummary,
        attempt: u32,
        counters: Counters,
    },
    /// No device to label.
    Idle {
        counters: Counters,
    },
    /// The scan matched: its LED goes off.
    Labelled {
        hub: HubSummary,
        counters: Counters,
    },
    /// A scan that isn't the active device's serial: nothing validated.
    Mismatch {
        expected: String,
        scanned: String,
    },
    /// A scan (or Enter) while no device is active.
    NothingActive,
    /// Enter alone in `confirm: scan` mode: a scan is required.
    ScanRequired,
    /// The label matched, but can't be validated yet.
    Blocked {
        hub: HubSummary,
        reason: LabelBlock,
    },
    Forced {
        hub: HubSummary,
    },
    Skipped {
        hub: HubSummary,
    },
    /// The active device stopped polling.
    Lost {
        hub: HubSummary,
    },
    /// One hook script finished.
    Hook {
        event: HookEvent,
        hub: HubSummary,
        outcome: HookOutcome,
    },
    Installed {
        hub: HubSummary,
    },
    /// A device rejected its identity, or the platform refused it one.
    Failed {
        hub: HubSummary,
        reason: String,
    },
    /// A claim refused before anything was issued.
    Rejected {
        hub: HubSummary,
        reason: String,
    },
    /// Something the operator should know about but that doesn't stop the
    /// station (a hook couldn't be read, one was stopped at shutdown).
    Warning(String),
    /// Something the operator must act on: devices are being turned away
    /// (the journal can't be written, the context's credentials no longer
    /// work).
    Alert(String),
}

/// DI seam for `remora-station-application`: the labelling console's
/// display -- what happens, and which device to label. Infallible: a
/// console that can't draw must not stop the station. What the operator
/// types comes the other way, through `StationServiceInterface::handle`.
pub trait OperatorAdapter: Send + Sync {
    fn show(&self, event: &OperatorEvent);
}

/// Injectable handle to whatever `OperatorAdapter` was wired at startup.
#[derive(Clone)]
pub struct OperatorAdapterService(busybody::Service<Box<dyn OperatorAdapter>>);

impl OperatorAdapterService {
    pub fn new<T: OperatorAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for OperatorAdapterService {
    type Target = busybody::Service<Box<dyn OperatorAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
