use std::path::Path;

use super::error::Result;

/// DI seam over the vfat backend: implemented by
/// `remora-etcher-image-adapter-vfat` for the real `fatfs` wrapper, and by
/// that same adapter for the `PartitionFilesystem` composite port. No
/// `*Service` DI wrapper yet — see `ext4::Ext4Adapter`'s doc comment.
pub trait VfatAdapter: Send + Sync {
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
}
