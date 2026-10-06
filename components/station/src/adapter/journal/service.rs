use std::path::Path;

use remora_factory::adapter::provisioning::ProvisionedIdentity;

use super::error::Result;
use crate::model::{BoardPolicy, ClaimId, HardwareInfo, ImageInfo};

/// One transition of one claim. The journal is the station's memory across
/// restarts (what makes a retry return the same identity without asking the
/// platform again) and the production register: serial, MACs, BSP serial,
/// machine-id, date, context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub claim_id: ClaimId,
    pub event: JournalEvent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalEvent {
    /// A device asked, before the platform is: what it said it is.
    Received {
        hardware: HardwareInfo,
        image: ImageInfo,
    },
    /// The platform issued its identity -- all of it, certificates
    /// included, so that a retry gets it back from here.
    Issued {
        identity: ProvisionedIdentity,
        context: String,
        policy: BoardPolicy,
    },
    /// One hook script ran.
    Hook {
        /// `<event>.d/<script>`.
        hook: String,
        exit: Option<i32>,
        timed_out: bool,
        attempt: u32,
        stdout: String,
        stderr: String,
    },
    /// It became the device being labelled, for the `attempt`th time.
    LabelActive {
        attempt: u32,
    },
    /// The operator asked for its label again.
    Reprint {
        attempt: u32,
    },
    /// A scan that wasn't its serial.
    LabelMismatch {
        scanned: String,
    },
    /// The operator validated it despite a failed label hook.
    LabelForced,
    /// The operator sent it back to the end of the queue.
    LabelSkipped,
    /// It stopped polling while active: back to the end of the queue.
    LabelLost,
    /// Its label was validated: by a scan, or by Enter alone (`None`).
    Labelled {
        scanned: Option<String>,
    },
    Installed,
    /// The device rejected its identity, or the platform refused it one.
    Failed {
        reason: String,
    },
}

/// DI seam for `remora-station-application`: where the journal lives and in
/// what format. Named by path on each call, like the factory's credential
/// writer: which file is a station's configuration, known only once it is
/// read.
#[async_trait::async_trait]
pub trait JournalAdapter: Send + Sync {
    /// Every entry of `path`, oldest first; none when it doesn't exist
    /// yet. A last line cut short by a crash is dropped, not an error.
    async fn load(&self, path: &Path) -> Result<Vec<JournalEntry>>;
    /// Appends `entry`, durably: on disk when this returns.
    async fn append(&self, path: &Path, entry: &JournalEntry) -> Result<()>;
}

/// Injectable handle to whatever `JournalAdapter` was wired at startup.
#[derive(Clone)]
pub struct JournalAdapterService(busybody::Service<Box<dyn JournalAdapter>>);

impl JournalAdapterService {
    pub fn new<T: JournalAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for JournalAdapterService {
    type Target = busybody::Service<Box<dyn JournalAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
