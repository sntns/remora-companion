use std::{fmt, io, path::PathBuf};

use crate::{adapter::partition_table, model::partition_table::SelectionError};

use super::partition_fs::PartitionFsError;

#[derive(Debug)]
pub enum Error {
    ReadPartitionTable(partition_table::Error),
    SelectPartition(SelectionError),
    ReadSource { path: PathBuf, source: io::Error },
    Write(PartitionFsError),
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

impl From<PartitionFsError> for Error {
    fn from(e: PartitionFsError) -> Self {
        Error::Write(e)
    }
}
