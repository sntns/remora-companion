use std::path::PathBuf;

use remora_channel::model::ProxyCommandBuilder;
use remora_context::model::ContextOverride;

/// Where a device keeps an uploaded bundle until it's installed: its
/// persistent data partition, not a tmpfs that would eat its RAM.
pub const DEFAULT_REMOTE_DIR: &str = "/data/cache";

/// The bundle to install.
#[derive(Debug, Clone)]
pub enum Bundle {
    /// A local `.raucb`.
    File(PathBuf),
    /// An artifact of a release, downloaded first (into `cache`, picked up
    /// there by a later run).
    Release {
        release: String,
        file_name: String,
        cache: PathBuf,
    },
}

/// Installing a bundle on a device, as the selected context, over the
/// device's ssh channel (as the `admin` ssh role: installing is root's).
pub struct InstallRequest {
    pub over: Option<ContextOverride>,
    pub device: String,
    pub bundle: Bundle,
    /// The device account, instead of the role's default.
    pub login: Option<String>,
    /// Where on the device the bundle is uploaded.
    pub remote_dir: String,
    /// Reboot into the new slot and wait for the device to come back.
    pub reboot: bool,
    /// Once back on the new slot, mark it good (`remora-otactl validate`).
    pub validate: bool,
    /// The ssh client to run.
    pub ssh: PathBuf,
    pub proxy_command: ProxyCommandBuilder,
}

/// How an install went.
#[derive(Debug, Clone, Default)]
pub struct InstallOutcome {
    pub bundle: String,
    pub size: u64,
    /// Bytes uploaded by this run, and where it started from (what an
    /// interrupted run left on the device).
    pub uploaded: u64,
    pub resumed_from: u64,
    /// The slot the device ran before, and the one it came back on.
    pub from_slot: Option<String>,
    pub to_slot: Option<String>,
    pub rebooted: bool,
    pub validated: bool,
}
