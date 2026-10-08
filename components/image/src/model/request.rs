use std::path::PathBuf;

use super::partition_table::PartitionSelector;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InjectRequest {
    pub image: PathBuf,
    pub source: PathBuf,
    /// Absolute path inside the partition's filesystem, e.g.
    /// `/play/tplst-app-config/config.json`. Its parent directory must
    /// already exist.
    pub dest_path: String,
    pub partition: PartitionSelector,
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
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FillRequest {
    pub image: PathBuf,
    /// Host file whose bytes become the partition's content, as is: no
    /// filesystem (e.g. an installer's `.bmaptar` payload).
    pub source: PathBuf,
    /// Must resolve to the image's last partition (GPT, or a primary one on
    /// MBR), which is resized to fit `source`.
    pub partition: PartitionSelector,
}

/// What filling a partition made of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FillOutcome {
    pub index: u32,
    /// The source's length.
    pub content_bytes: u64,
    /// The partition's new size: the content rounded up to
    /// [`FILL_ALIGNMENT`].
    pub partition_bytes: u64,
}

/// A filled partition's size is a multiple of this: wic's own 1 MiB
/// alignment, so a resized partition ends where a freshly built one would.
pub const FILL_ALIGNMENT: u64 = 1024 * 1024;
