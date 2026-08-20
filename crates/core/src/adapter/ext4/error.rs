use std::{fmt, io, path::PathBuf};

#[derive(Debug)]
pub enum Error {
    Open {
        path: PathBuf,
        source: io::Error,
    },
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
    NeedsJournalRecovery {
        path: PathBuf,
    },
    Ext4(fs_ext4::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Open { path, source } => {
                write!(f, "failed to open {}: {source}", path.display())
            }
            Error::OutOfWindow {
                path,
                offset,
                len,
                window_size,
            } => write!(
                f,
                "partition window [{offset}, {}) does not fit inside {} ({window_size} bytes)",
                offset + len,
                path.display()
            ),
            Error::NeedsJournalRecovery { path } => write!(
                f,
                "{}: filesystem needs journal recovery (needs_recovery flag set) — \
                 refusing to mount rather than risk am-fs-ext4 corrupting the superblock \
                 on this journal feature combination; run a real `fsck.ext4 -y` on it \
                 (or let the device boot once and shut down cleanly) first",
                path.display()
            ),
            Error::Ext4(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<fs_ext4::Error> for Error {
    fn from(e: fs_ext4::Error) -> Self {
        Error::Ext4(e)
    }
}
