use std::path::PathBuf;

use crate::model::error::Error;

/// Squashfs block size bounds, mirroring squashfs-tools' own constraints
/// (power of two, 4 KiB..=1 MiB).
pub const MIN_BLOCK_SIZE: u32 = 4_096;
pub const MAX_BLOCK_SIZE: u32 = 1_048_576;

/// Default squashfs block size (128 KiB), matching `mksquashfs`'s own default.
pub const DEFAULT_BLOCK_SIZE: u32 = 131_072;

/// Compression algorithm for a squashfs image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compression {
    /// Matches `mksquashfs`'s own default when no `-comp` flag is given.
    #[default]
    Gzip,
    Lzma,
    Lzo,
    Xz,
    Lz4,
    Zstd,
}

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub compression: Compression,
    pub block_size: u32,
    /// Applied to every entry's mtime when set; otherwise each entry keeps its
    /// own real mtime and the image's own SOURCE_DATE_EPOCH-style field falls
    /// back to the newest mtime among inputs (mirrors OE-core's
    /// `SOURCE_DATE_EPOCH = stat -c '%Y' ${IMAGE_ROOTFS}`).
    pub source_date_epoch: Option<u32>,
    /// Mode of the synthetic squashfs root directory.
    pub root_mode: u16,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            compression: Compression::default(),
            block_size: DEFAULT_BLOCK_SIZE,
            source_date_epoch: None,
            root_mode: 0o755,
        }
    }
}

impl BuildOptions {
    pub fn validate(&self) -> Result<(), Error> {
        if self.block_size < MIN_BLOCK_SIZE
            || self.block_size > MAX_BLOCK_SIZE
            || !self.block_size.is_power_of_two()
        {
            return Err(Error::InvalidBlockSize(self.block_size));
        }
        Ok(())
    }
}

/// One entry to place in the built squashfs image, already resolved from the
/// host filesystem — adapter-agnostic so it doesn't leak any backhand types.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Path relative to the squashfs root, e.g. `etc/hostname`.
    pub path: PathBuf,
    pub metadata: EntryMetadata,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Copy)]
pub struct EntryMetadata {
    pub permissions: u16,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u32,
}

#[derive(Debug, Clone)]
pub enum EntryKind {
    Directory,
    File { source: PathBuf },
    Symlink { target: PathBuf },
}
