use std::path::PathBuf;

use super::partition_table::{BootMode, PartitionSelector};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InjectRequest {
    pub image: PathBuf,
    pub source: PathBuf,
    /// Absolute path inside the partition's filesystem, e.g.
    /// `/play/tplst-app-config/config.json`. Its parent directory must
    /// already exist.
    pub dest_path: String,
    pub partition: PartitionSelector,
    pub boot_mode: Option<BootMode>,
    /// File mode (permission bits only), e.g. `0o644`.
    pub mode: u16,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MkdirRequest {
    pub image: PathBuf,
    /// Absolute path inside the partition's filesystem, e.g.
    /// `/play/tplst-app-config`. Its parent directory must already exist —
    /// not recursive, same narrow-surface rule as `InjectRequest`.
    pub dest_path: String,
    pub partition: PartitionSelector,
    pub boot_mode: Option<BootMode>,
    /// Directory mode (permission bits only), e.g. `0o755`.
    pub mode: u16,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CpDirRequest {
    pub image: PathBuf,
    /// Host directory to copy in, recursively.
    pub source_dir: PathBuf,
    /// Directory inside the partition's filesystem that `source_dir`'s
    /// content is copied under, e.g. `/` or `/dump`. Must already exist —
    /// not created by this request (see `mkdir`).
    pub dest_path: String,
    pub partition: PartitionSelector,
    pub boot_mode: Option<BootMode>,
}
