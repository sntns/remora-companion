use std::path::PathBuf;

/// What we know about a candidate flash target, gathered purely for the
/// safety guard in `application::flash::service` — this is the information a
/// human (or a future GUI) needs to see before agreeing to overwrite a disk.
#[derive(Debug, Clone)]
pub struct DiskInfo {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub model: Option<String>,
    pub is_removable: bool,
    /// Best-effort: true if this disk (or one of its partitions) backs the
    /// current root filesystem. A false negative is possible in exotic setups
    /// (e.g. network root); a false positive should not happen.
    pub is_system_disk: bool,
}
