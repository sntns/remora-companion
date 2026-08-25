use std::path::PathBuf;

/// One entry found while walking a host directory tree, already resolved to
/// plain data — adapter-agnostic so it doesn't leak `walkdir` types into
/// whichever vertical consumes it (squashfs, and later ext4image/config).
#[derive(Debug, Clone)]
pub struct WalkEntry {
    /// Path relative to the walk's root, e.g. `etc/hostname`.
    pub path: PathBuf,
    pub metadata: WalkEntryMetadata,
    pub kind: WalkEntryKind,
}

#[derive(Debug, Clone, Copy)]
pub struct WalkEntryMetadata {
    pub permissions: u16,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u32,
}

#[derive(Debug, Clone)]
pub enum WalkEntryKind {
    Directory,
    File { source: PathBuf },
    Symlink { target: PathBuf },
}
