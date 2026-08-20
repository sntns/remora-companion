//! `ext4` and `vfat` are the two backends that can occupy a "shared"/slot
//! partition — this port lets `fs_dispatch` resolve one from the partition's
//! own on-disk signature (`adapter::partition_table::detect_fs_kind`) behind
//! a single interface, instead of matching on `FsKind` at every call site.

use std::{fmt, path::Path};

use crate::adapter::{ext4, vfat};

#[derive(Debug)]
pub enum PartitionFsError {
    Ext4(ext4::Error),
    Vfat(vfat::Error),
}

impl fmt::Display for PartitionFsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PartitionFsError::Ext4(e) => write!(f, "{e}"),
            PartitionFsError::Vfat(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PartitionFsError {}

impl From<ext4::Error> for PartitionFsError {
    fn from(e: ext4::Error) -> Self {
        PartitionFsError::Ext4(e)
    }
}

impl From<vfat::Error> for PartitionFsError {
    fn from(e: vfat::Error) -> Self {
        PartitionFsError::Vfat(e)
    }
}

pub trait PartitionFilesystem {
    fn write_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<(), PartitionFsError>;

    fn read_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
    ) -> Result<Vec<u8>, PartitionFsError>;

    fn exists(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
    ) -> Result<bool, PartitionFsError>;

    fn create_dir(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        mode: u16,
    ) -> Result<(), PartitionFsError>;
}

impl PartitionFilesystem for ext4::Ext4 {
    fn write_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<(), PartitionFsError> {
        Ok(ext4::write_file(
            image, offset, size, dest_path, contents, mode,
        )?)
    }

    fn read_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
    ) -> Result<Vec<u8>, PartitionFsError> {
        Ok(ext4::read_file(image, offset, size, dest_path)?)
    }

    fn exists(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
    ) -> Result<bool, PartitionFsError> {
        Ok(ext4::exists(image, offset, size, dest_path)?)
    }

    fn create_dir(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        mode: u16,
    ) -> Result<(), PartitionFsError> {
        Ok(ext4::create_dir(image, offset, size, dest_path, mode)?)
    }
}

impl PartitionFilesystem for vfat::Vfat {
    fn write_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<(), PartitionFsError> {
        Ok(vfat::write_file(
            image, offset, size, dest_path, contents, mode,
        )?)
    }

    fn read_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
    ) -> Result<Vec<u8>, PartitionFsError> {
        Ok(vfat::read_file(image, offset, size, dest_path)?)
    }

    fn exists(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
    ) -> Result<bool, PartitionFsError> {
        Ok(vfat::exists(image, offset, size, dest_path)?)
    }

    fn create_dir(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        mode: u16,
    ) -> Result<(), PartitionFsError> {
        Ok(vfat::create_dir(image, offset, size, dest_path, mode)?)
    }
}
