//! Shared by `inject`/`mkdir`: pick the `ext4` or `vfat` adapter
//! for a resolved partition based on its own on-disk filesystem signature
//! (`adapter::partition_table::detect_fs_kind`) — never assumed from a
//! boot-mode label. See the Phase 5 plan notes on why shared/slotA/slotB are
//! vfat on EFI/UBOOT/RPI images and ext4 only on BIOS.

use std::path::Path;

use crate::{
    adapter::{
        ext4::{self, Ext4},
        partition_table,
        vfat::Vfat,
    },
    model::partition_table::FsKind,
};

use super::error::Error;
use super::partition_fs::{PartitionFilesystem, PartitionFsError};

fn backend(kind: FsKind) -> &'static dyn PartitionFilesystem {
    match kind {
        FsKind::Ext4 => &Ext4,
        FsKind::Vfat => &Vfat,
    }
}

pub(crate) fn write_file(
    image: &Path,
    offset: u64,
    size: u64,
    dest_path: &str,
    contents: &[u8],
    mode: u16,
) -> Result<(), Error> {
    let kind = partition_table::detect_fs_kind(image, offset)?;
    backend(kind).write_file(image, offset, size, dest_path, contents, mode)?;
    Ok(())
}

pub(crate) fn create_dir(
    image: &Path,
    offset: u64,
    size: u64,
    dest_path: &str,
    mode: u16,
) -> Result<(), Error> {
    let kind = partition_table::detect_fs_kind(image, offset)?;
    backend(kind).create_dir(image, offset, size, dest_path, mode)?;
    Ok(())
}

/// Like `create_dir`, but treats "already exists" as success — `vfat`'s
/// `create_dir` is already idempotent this way, `ext4`'s isn't (it
/// returns `Error::AlreadyExists`, mirroring `apply_mkdir`'s own semantics).
/// Used by `application::config::upload` to ensure `/remora`/`/remora/<slot>`
/// exist before writing `config` into them, without erroring on repeat runs.
pub(crate) fn ensure_dir(
    image: &Path,
    offset: u64,
    size: u64,
    dest_path: &str,
    mode: u16,
) -> Result<(), Error> {
    match create_dir(image, offset, size, dest_path, mode) {
        Ok(()) => Ok(()),
        Err(Error::Write(PartitionFsError::Ext4(ext4::Error::Ext4(
            fs_ext4::Error::AlreadyExists,
        )))) => Ok(()),
        Err(e) => Err(e),
    }
}

pub(crate) fn read_file(
    image: &Path,
    offset: u64,
    size: u64,
    dest_path: &str,
) -> Result<Vec<u8>, Error> {
    let kind = partition_table::detect_fs_kind(image, offset)?;
    Ok(backend(kind).read_file(image, offset, size, dest_path)?)
}

pub(crate) fn exists(image: &Path, offset: u64, size: u64, dest_path: &str) -> Result<bool, Error> {
    let kind = partition_table::detect_fs_kind(image, offset)?;
    Ok(backend(kind).exists(image, offset, size, dest_path)?)
}
