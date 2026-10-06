/// Squashfs block size bounds, mirroring squashfs-tools' own constraints
/// (power of two, 4 KiB..=1 MiB).
pub const MIN_BLOCK_SIZE: u32 = 4_096;
pub const MAX_BLOCK_SIZE: u32 = 1_048_576;

/// Default squashfs block size (128 KiB), matching `mksquashfs`'s own default.
pub const DEFAULT_BLOCK_SIZE: u32 = 131_072;

/// Compression algorithm for a squashfs image: the ones `backhand` can
/// write with its default features. mksquashfs's lzma and lzo aren't
/// offered, since every build with them would fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Compression {
    /// Matches `mksquashfs`'s own default when no `-comp` flag is given.
    #[default]
    Gzip,
    Xz,
    Lz4,
    Zstd,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("invalid squashfs block size {0}: must be a power of two between {MIN_BLOCK_SIZE} and {MAX_BLOCK_SIZE}")]
pub struct InvalidBuildOptions(pub u32);

impl BuildOptions {
    pub fn validate(&self) -> Result<(), InvalidBuildOptions> {
        if self.block_size < MIN_BLOCK_SIZE
            || self.block_size > MAX_BLOCK_SIZE
            || !self.block_size.is_power_of_two()
        {
            return Err(InvalidBuildOptions(self.block_size));
        }
        Ok(())
    }
}
