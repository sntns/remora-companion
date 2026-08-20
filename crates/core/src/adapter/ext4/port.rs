use std::path::Path;

use super::error::Error;
use super::service;

/// DI seam over the ext4 backend: implemented here for the real `am-fs-ext4`
/// wrapper, and by `application::image::partition_fs::PartitionFilesystem`
/// for the boot-mode-agnostic ext4/vfat dispatch used by `fs_dispatch`.
pub trait Ext4Adapter {
    fn write_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<(), Error>;

    fn read_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
    ) -> Result<Vec<u8>, Error>;

    fn exists(&self, image: &Path, offset: u64, size: u64, dest_path: &str) -> Result<bool, Error>;

    fn create_dir(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        mode: u16,
    ) -> Result<(), Error>;

    fn format(
        &self,
        image: &Path,
        size_bytes: u64,
        block_size: u32,
        label: Option<&str>,
    ) -> Result<(), Error>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Ext4;

impl Ext4Adapter for Ext4 {
    fn write_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<(), Error> {
        service::write_file(image, offset, size, dest_path, contents, mode)
    }

    fn read_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
    ) -> Result<Vec<u8>, Error> {
        service::read_file(image, offset, size, dest_path)
    }

    fn exists(&self, image: &Path, offset: u64, size: u64, dest_path: &str) -> Result<bool, Error> {
        service::exists(image, offset, size, dest_path)
    }

    fn create_dir(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        mode: u16,
    ) -> Result<(), Error> {
        service::create_dir(image, offset, size, dest_path, mode)
    }

    fn format(
        &self,
        image: &Path,
        size_bytes: u64,
        block_size: u32,
        label: Option<&str>,
    ) -> Result<(), Error> {
        service::format(image, size_bytes, block_size, label)
    }
}
