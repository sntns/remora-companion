use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct FlashRequest {
    pub image: PathBuf,
    pub bmap: Option<PathBuf>,
    pub device: PathBuf,
    /// Bypass the removable-disk check (still refuses a disk identified as
    /// the system disk regardless of this flag).
    pub force: bool,
}

#[derive(Debug)]
pub struct FlashOutcome {
    /// Bytes actually written, i.e. the mapped size when a `.bmap` was used;
    /// the full image size otherwise.
    pub bytes_written: u64,
    pub used_bmap: bool,
}
