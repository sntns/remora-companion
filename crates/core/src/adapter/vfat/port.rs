use std::path::Path;

use super::error::Error;
use super::service;

/// DI seam over the vfat backend: implemented here for the real `fatfs`
/// wrapper, and by `application::image::partition_fs::PartitionFilesystem`
/// for the boot-mode-agnostic ext4/vfat dispatch used by `fs_dispatch`.
pub trait VfatAdapter {
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
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Vfat;

impl VfatAdapter for Vfat {
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
}
