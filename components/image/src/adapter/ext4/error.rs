use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error on {0}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error(
        "partition window [{offset}, {}) does not fit inside {} ({window_size} bytes)",
        offset + len, path.display()
    )]
    OutOfWindow {
        path: PathBuf,
        offset: u64,
        len: u64,
        window_size: u64,
    },
    /// The `needs_recovery` (`INCOMPAT_RECOVER`) superblock flag is set —
    /// refused before ever calling `Filesystem::mount()`. Found by hand
    /// against a real device-like image (a qemu-booted, uncleanly-shut-down
    /// wic image): with `journal_64bit`+`journal_checksum_v3`, `mount()`
    /// itself fails *and* leaves the superblock zeroed/unreadable behind —
    /// a real corruption, not just a returned error. Until that's fixed
    /// upstream, this check is the only thing standing between "journal
    /// needs replay" and a wrecked partition.
    #[error(
        "{}: filesystem needs journal recovery (needs_recovery flag set) — \
         refusing to mount rather than risk am-fs-ext4 corrupting the superblock \
         on this journal feature combination; run a real `fsck.ext4 -y` on it \
         (or let the device boot once and shut down cleanly) first",
        path.display()
    )]
    NeedsJournalRecovery { path: PathBuf },
    #[error("{0}")]
    Ext4(#[from] fs_ext4::Error),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
