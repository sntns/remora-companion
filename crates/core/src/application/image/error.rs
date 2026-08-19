use std::{fmt, io, path::PathBuf};

use crate::{
    adapter::{ext4_fsext4, partition_table},
    model::partition_table::SelectionError,
};

#[derive(Debug)]
pub enum Error {
    ReadPartitionTable(partition_table::Error),
    SelectPartition(SelectionError),
    ReadSource { path: PathBuf, source: io::Error },
    Write(ext4_fsext4::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ReadPartitionTable(e) => write!(f, "{e}"),
            Error::SelectPartition(e) => write!(f, "{e}"),
            Error::ReadSource { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Error::Write(e) => write!(f, "{e}"),
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

impl From<ext4_fsext4::Error> for Error {
    fn from(e: ext4_fsext4::Error) -> Self {
        Error::Write(e)
    }
}
