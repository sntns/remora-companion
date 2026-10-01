use std::path::Path;

use super::error::Result;

/// DI seam over the ext4 backend: implemented by
/// `remora-image-adapter-ext4` for the real `am-fs-ext4` wrapper, and
/// by that same adapter for the boot-mode-agnostic `PartitionFilesystem`
/// composite port used by the image application service. The config
/// vertical also injects this directly (not through `PartitionFilesystem`)
/// to manipulate a standalone `config.ext4` file — that file's content
/// happens to be a whole ext4 filesystem in its own right, addressed at
/// `offset = 0`, not a partition inside a bigger disk image.
pub trait Ext4Adapter: Send + Sync {
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

    fn format(
        &self,
        image: &Path,
        size_bytes: u64,
        block_size: u32,
        label: Option<&str>,
    ) -> Result<()>;
}

/// Injectable handle to whatever `Ext4Adapter` was wired at startup.
#[derive(Clone)]
pub struct Ext4AdapterService(busybody::Service<Box<dyn Ext4Adapter>>);

impl Ext4AdapterService {
    pub fn new<T: Ext4Adapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for Ext4AdapterService {
    type Target = busybody::Service<Box<dyn Ext4Adapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
