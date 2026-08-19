use std::fmt;

use crate::adapter::partition_table;

#[derive(Debug)]
pub enum Error {
    ReadPartitionTable(partition_table::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ReadPartitionTable(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<partition_table::Error> for Error {
    fn from(e: partition_table::Error) -> Self {
        Error::ReadPartitionTable(e)
    }
}
