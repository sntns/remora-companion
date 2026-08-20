use std::{fmt, io, path::PathBuf};

use crate::{
    adapter::{ext4, partition_table},
    application::{ext4image, image},
    model::partition_table::SelectionError,
};

#[derive(Debug)]
pub enum Error {
    ReadPartitionTable(partition_table::Error),
    SelectPartition(SelectionError),
    ReadSource { path: PathBuf, source: io::Error },
    ReadWriteShared(image::Error),
    BuildFreshImage(ext4image::Error),
    ReadTempImage { path: PathBuf, source: io::Error },
    WriteTempImage { path: PathBuf, source: io::Error },
    MountTempImage(ext4::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ReadPartitionTable(e) => write!(f, "{e}"),
            Error::SelectPartition(e) => write!(f, "{e}"),
            Error::ReadSource { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Error::ReadWriteShared(e) => write!(f, "failed to access the shared partition: {e}"),
            Error::BuildFreshImage(e) => write!(f, "failed to build a fresh config.ext4: {e}"),
            Error::ReadTempImage { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Error::WriteTempImage { path, source } => {
                write!(f, "failed to write {}: {source}", path.display())
            }
            Error::MountTempImage(e) => {
                write!(f, "failed to update the local config.ext4 copy: {e}")
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<partition_table::Error> for Error {
    fn from(e: partition_table::Error) -> Self {
        Error::ReadPartitionTable(e)
    }
}

impl From<SelectionError> for Error {
    fn from(e: SelectionError) -> Self {
        Error::SelectPartition(e)
    }
}

impl From<image::Error> for Error {
    fn from(e: image::Error) -> Self {
        Error::ReadWriteShared(e)
    }
}

impl From<ext4image::Error> for Error {
    fn from(e: ext4image::Error) -> Self {
        Error::BuildFreshImage(e)
    }
}

impl From<ext4::Error> for Error {
    fn from(e: ext4::Error) -> Self {
        Error::MountTempImage(e)
    }
}
