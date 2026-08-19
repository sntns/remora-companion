//! Thin wrapper around `am-fs-ext4` (the `fs_ext4` crate — validated in the
//! phase-3 spike, see the "spike: validate am-fs-ext4 for phase 3" commit),
//! narrowed to exactly the operation `remora-etcher image partition cp`
//! needs: create one file inside an already-existing directory of an ext4
//! filesystem that lives at a byte-range window inside a larger disk image
//! or device, and write its content.
//!
//! Deliberately not journaled beyond what `apply_create`/`apply_pwrite`
//! themselves do — this is a one-shot CLI operation on a device that isn't
//! concurrently mounted elsewhere, the same class of risk `wic cp`/`debugfs`
//! already carry.

use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use fs_ext4::block_io::BlockDevice;
use fs_ext4::Filesystem;

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

/// A `BlockDevice` that's really just `[offset, offset + size)` of a larger
/// file — a wic image's `data` partition, typically — so `am-fs-ext4` can
/// mount that range directly without us copying the partition out to its
/// own temporary file first.
struct WindowedFileDevice {
    file: Mutex<File>,
    offset: u64,
    size: u64,
}

impl WindowedFileDevice {
    fn open_rw(path: &std::path::Path, offset: u64, size: u64) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| Error::Open {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            file: Mutex::new(file),
            offset,
            size,
        })
    }

    fn check_bounds(&self, offset: u64, len: usize) -> fs_ext4::Result<()> {
        if offset.saturating_add(len as u64) > self.size {
            return Err(fs_ext4::Error::Corrupt("access outside partition window"));
        }
        Ok(())
    }
}

impl BlockDevice for WindowedFileDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_ext4::Result<()> {
        self.check_bounds(offset, buf.len())?;
        let mut f = self.file.lock().unwrap();
        f.seek(SeekFrom::Start(self.offset + offset))?;
        f.read_exact(buf)?;
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.size
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> fs_ext4::Result<()> {
        self.check_bounds(offset, buf.len())?;
        let mut f = self.file.lock().unwrap();
        f.seek(SeekFrom::Start(self.offset + offset))?;
        f.write_all(buf)?;
        Ok(())
    }

    fn flush(&self) -> fs_ext4::Result<()> {
        let mut f = self.file.lock().unwrap();
        f.flush()?;
        f.sync_data()?;
        Ok(())
    }

    fn is_writable(&self) -> bool {
        true
    }
}

// Standard ext2/3/4 on-disk superblock layout (e2fsprogs' ext2fs.h /
// linux/fs/ext4/ext4.h) — the superblock always starts 1024 bytes into the
// filesystem, regardless of block size.
const EXT4_SUPERBLOCK_OFFSET: u64 = 1024;
const EXT4_MAGIC_OFFSET: usize = 0x38;
const EXT4_FEATURE_INCOMPAT_OFFSET: usize = 0x60;
const EXT4_MAGIC: u16 = 0xEF53;
const EXT4_FEATURE_INCOMPAT_RECOVER: u32 = 0x0004;

/// Read just enough of the superblock to check `needs_recovery`, without
/// going through `am-fs-ext4` at all — see `Error::NeedsJournalRecovery`.
fn refuse_if_needs_journal_recovery(
    image: &std::path::Path,
    partition_offset: u64,
) -> Result<(), Error> {
    let mut file = File::open(image).map_err(|source| Error::Open {
        path: image.to_path_buf(),
        source,
    })?;
    let mut buf = [0u8; 100];
    file.seek(SeekFrom::Start(partition_offset + EXT4_SUPERBLOCK_OFFSET))
        .and_then(|_| file.read_exact(&mut buf))
        .map_err(|source| Error::Open {
            path: image.to_path_buf(),
            source,
        })?;

    let magic = u16::from_le_bytes([buf[EXT4_MAGIC_OFFSET], buf[EXT4_MAGIC_OFFSET + 1]]);
    if magic != EXT4_MAGIC {
        // Not our concern here — let Filesystem::mount() report the real
        // "this isn't ext4" error with its own context.
        return Ok(());
    }

    let feature_incompat = u32::from_le_bytes([
        buf[EXT4_FEATURE_INCOMPAT_OFFSET],
        buf[EXT4_FEATURE_INCOMPAT_OFFSET + 1],
        buf[EXT4_FEATURE_INCOMPAT_OFFSET + 2],
        buf[EXT4_FEATURE_INCOMPAT_OFFSET + 3],
    ]);
    if feature_incompat & EXT4_FEATURE_INCOMPAT_RECOVER != 0 {
        return Err(Error::NeedsJournalRecovery {
            path: image.to_path_buf(),
        });
    }
    Ok(())
}

fn mount_window(image: &std::path::Path, offset: u64, size: u64) -> Result<Filesystem, Error> {
    let image_len = std::fs::metadata(image)
        .map_err(|source| Error::Open {
            path: image.to_path_buf(),
            source,
        })?
        .len();
    let window_end = offset.checked_add(size).filter(|&end| end <= image_len);
    if window_end.is_none() {
        return Err(Error::OutOfWindow {
            path: image.to_path_buf(),
            offset,
            len: size,
            window_size: image_len,
        });
    }

    refuse_if_needs_journal_recovery(image, offset)?;

    let dev = Arc::new(WindowedFileDevice::open_rw(image, offset, size)?);
    let fs = Filesystem::mount(dev)?;
    fs.replay_journal_if_dirty()?;
    Ok(fs)
}

/// Create `dest_path` (parent directory must already exist) inside the
/// ext4 filesystem occupying `[offset, offset + size)` of `image`, and
/// write `contents` to it.
pub fn write_file(
    image: &std::path::Path,
    offset: u64,
    size: u64,
    dest_path: &str,
    contents: &[u8],
    mode: u16,
) -> Result<(), Error> {
    let fs = mount_window(image, offset, size)?;
    fs.apply_create(dest_path, mode)?;
    fs.apply_pwrite(dest_path, 0, contents)?;
    Ok(())
}

/// Create directory `dest_path` (parent directory must already exist)
/// inside the ext4 filesystem occupying `[offset, offset + size)` of
/// `image`. Not recursive — same narrow-surface rule as `write_file`.
pub fn create_dir(
    image: &std::path::Path,
    offset: u64,
    size: u64,
    dest_path: &str,
    mode: u16,
) -> Result<(), Error> {
    let fs = mount_window(image, offset, size)?;
    fs.apply_mkdir(dest_path, mode)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal buffer with a valid ext4 magic at the real superblock
    /// offset, and `feature_incompat` set from the given flags.
    fn fake_filesystem_bytes(feature_incompat: u32) -> Vec<u8> {
        let mut buf = vec![0u8; 1024 + 128];
        buf[1024 + EXT4_MAGIC_OFFSET..1024 + EXT4_MAGIC_OFFSET + 2]
            .copy_from_slice(&EXT4_MAGIC.to_le_bytes());
        buf[1024 + EXT4_FEATURE_INCOMPAT_OFFSET..1024 + EXT4_FEATURE_INCOMPAT_OFFSET + 4]
            .copy_from_slice(&feature_incompat.to_le_bytes());
        buf
    }

    fn write_temp(bytes: &[u8]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-etcher-ext4-fsext4-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        path
    }

    #[test]
    fn refuses_when_needs_recovery_is_set() {
        let path = write_temp(&fake_filesystem_bytes(
            EXT4_FEATURE_INCOMPAT_RECOVER | 0x0040,
        ));
        let err = refuse_if_needs_journal_recovery(&path, 0).unwrap_err();
        assert!(matches!(err, Error::NeedsJournalRecovery { .. }));
    }

    #[test]
    fn allows_a_clean_journal_through() {
        // extents (0x0040) set, RECOVER not set.
        let path = write_temp(&fake_filesystem_bytes(0x0040));
        assert!(refuse_if_needs_journal_recovery(&path, 0).is_ok());
    }

    #[test]
    fn defers_to_mount_when_magic_does_not_match() {
        let mut bytes = fake_filesystem_bytes(EXT4_FEATURE_INCOMPAT_RECOVER);
        bytes[1024 + EXT4_MAGIC_OFFSET] = 0x00;
        bytes[1024 + EXT4_MAGIC_OFFSET + 1] = 0x00;
        let path = write_temp(&bytes);
        assert!(refuse_if_needs_journal_recovery(&path, 0).is_ok());
    }
}
