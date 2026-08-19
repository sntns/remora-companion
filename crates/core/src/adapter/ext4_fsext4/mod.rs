//! Thin wrapper around `am-fs-ext4` (the `fs_ext4` crate — see the phase-3
//! spike at `crates/ext4-spike` for how this was validated), narrowed to
//! exactly the operation `remora-etcher image partition cp` needs: create
//! one file inside an already-existing directory of an ext4 filesystem that
//! lives at a byte-range window inside a larger disk image or device, and
//! write its content.
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

    let dev = Arc::new(WindowedFileDevice::open_rw(image, offset, size)?);
    let fs = Filesystem::mount(dev)?;
    fs.replay_journal_if_dirty()?;
    fs.apply_create(dest_path, mode)?;
    fs.apply_pwrite(dest_path, 0, contents)?;
    Ok(())
}
