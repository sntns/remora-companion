use std::path::Path;

use super::error::Result;

/// `ext4` and `vfat` are the two backends that can occupy a "shared"/slot
/// partition — this composite port lets the image application resolve one
/// from the partition's own on-disk signature
/// (`partition_table::PartitionTableAdapter::detect_fs_kind`) behind a
/// single interface, instead of matching on `FsKind` at every call site.
/// Implemented by `remora-image-adapter-ext4` and
/// `remora-image-adapter-vfat` on top of their own narrower
/// `Ext4Adapter`/`VfatAdapter` ports.
pub trait PartitionFilesystem: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn write_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<()>;

    fn read_file(&self, image: &Path, offset: u64, size: u64, dest_path: &str) -> Result<Vec<u8>>;

    fn exists(&self, image: &Path, offset: u64, size: u64, dest_path: &str) -> Result<bool>;

    fn create_dir(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        mode: u16,
    ) -> Result<()>;

    /// Like `create_dir`, but treats "already exists" as success.
    ///
    /// Backends disagree on whether re-creating an existing directory is an
    /// error (ext4's `apply_mkdir` says yes; `fatfs`'s `create_dir` on vfat
    /// says no — see `remora-image-adapter-vfat`'s
    /// `create_dir_is_idempotent` test) and `exists()` itself isn't a safe
    /// probe on every backend (vfat's `exists()` errors, rather than
    /// returning `false`, when given a directory path instead of a file —
    /// see that same crate's `write_file_overwrites_and_read_file_and_exists_round_trip`
    /// neighborhood). So this only consults `exists()` as a *last resort*,
    /// after `create_dir` has already failed — on vfat that fallback path
    /// is simply never reached, since `create_dir` never fails there in the
    /// first place.
    fn ensure_dir(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        mode: u16,
    ) -> Result<()> {
        match self.create_dir(image, offset, size, dest_path, mode) {
            Ok(()) => Ok(()),
            Err(create_err) => {
                if self.exists(image, offset, size, dest_path).unwrap_or(false) {
                    Ok(())
                } else {
                    Err(create_err)
                }
            }
        }
    }
}
