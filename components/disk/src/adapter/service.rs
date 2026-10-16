use std::{
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    path::Path,
};

use crate::model::DiskInfo;

use super::error::Result;

/// DI seam for `remora-disk-application`: swap the real `/sys/block`
/// reader for a test double without touching the use case.
pub trait DiskAdapter: Send + Sync {
    fn enumerate(&self) -> Result<Vec<DiskInfo>>;
    /// The whole disk `path` opens, wherever `path` points: symlinks
    /// (`/dev/disk/by-id/...`) are followed and the device node is
    /// identified by its device number, not its name, so the returned
    /// `DiskInfo::path` is the canonical node -- the one the flash guard
    /// compares with what it is about to open.
    fn info(&self, path: &Path) -> Result<DiskInfo>;
    /// The disk `disk` describes, opened to be overwritten whole: whatever
    /// the OS needs first (its volumes unmounted, and on macOS kept from
    /// mounting again until the target is dropped) is done here.
    fn open(&self, disk: &DiskInfo) -> Result<DiskTarget>;
    /// When `path` is a disk device (the whole disk or one of its
    /// partitions), that whole disk's volumes unmounted, for something to
    /// write into it from under no filesystem; on macOS, kept from mounting
    /// again until the hold is dropped. `None` for anything else (an image
    /// file): nothing to hold. The system disk is refused.
    fn hold(&self, path: &Path) -> Result<Option<DiskHold>>;
}

/// A disk's volumes held unmounted, until dropped. What it holds is the
/// adapter's (macOS: a mount blocker); nothing on Linux, where nothing
/// mounts a disk on its own once unmounted.
pub struct DiskHold {
    _held: Box<dyn Send>,
}

impl std::fmt::Debug for DiskHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiskHold")
    }
}

impl DiskHold {
    pub fn new<T: Send + 'static>(held: T) -> Self {
        Self {
            _held: Box::new(held),
        }
    }
}

/// What a [`DiskTarget`] writes through: a plain device file on Linux, an
/// aligned writer over the raw node on macOS.
pub trait DiskWrite: Write + Seek + Send {
    /// Everything written is on the disk, not in a buffer of ours or the
    /// kernel's.
    fn sync(&mut self) -> io::Result<()>;

    /// Closes the disk and, where the OS has the notion, ejects it, so it
    /// can be unplugged at once; true if it was ejected. Plain closing
    /// (Linux: nothing of it was mounted by us) by default.
    fn eject(self: Box<Self>) -> io::Result<bool> {
        Ok(false)
    }
}

impl DiskWrite for File {
    fn sync(&mut self) -> io::Result<()> {
        self.sync_all()
    }
}

/// A whole disk opened for writing an image to. Whatever was done to open
/// it (volumes held unmounted) lasts until it's dropped.
pub struct DiskTarget(Box<dyn DiskWrite>);

impl DiskTarget {
    pub fn new<T: DiskWrite + 'static>(inner: T) -> Self {
        Self(Box::new(inner))
    }

    pub fn sync(&mut self) -> io::Result<()> {
        self.0.sync()
    }

    /// See [`DiskWrite::eject`].
    pub fn eject(self) -> io::Result<bool> {
        self.0.eject()
    }
}

impl Write for DiskTarget {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Seek for DiskTarget {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.seek(pos)
    }
}

/// Injectable handle to whatever `DiskAdapter` was wired at startup.
#[derive(Clone)]
pub struct DiskAdapterService(busybody::Service<Box<dyn DiskAdapter>>);

impl DiskAdapterService {
    pub fn new<T: DiskAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for DiskAdapterService {
    type Target = busybody::Service<Box<dyn DiskAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
