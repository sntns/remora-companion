use std::{fmt, io, path::PathBuf};

use crate::{
    adapter::partition_table,
    application::{image, squashfs},
    model::partition_table::SelectionError,
};

#[derive(Debug)]
pub enum Error {
    CreateScratchDir { path: PathBuf, source: io::Error },
    WriteScratchFile { path: PathBuf, source: io::Error },
    Squashfs(squashfs::Error),
    ReadPartitionTable(partition_table::Error),
    SelectPartition(SelectionError),
    ReadBuiltImage { path: PathBuf, source: io::Error },
    WriteToShared(image::Error),
    GenerateSshHostKey(ssh_key::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CreateScratchDir { path, source } => {
                write!(f, "failed to create {}: {source}", path.display())
            }
            Error::WriteScratchFile { path, source } => {
                write!(f, "failed to write {}: {source}", path.display())
            }
            Error::Squashfs(e) => write!(f, "failed to build identity.squashfs: {e}"),
            Error::ReadPartitionTable(e) => write!(f, "{e}"),
            Error::SelectPartition(e) => write!(f, "{e}"),
            Error::ReadBuiltImage { path, source } => {
                write!(f, "failed to read {}: {source}", path.display())
            }
            Error::WriteToShared(e) => {
                write!(
                    f,
                    "failed to write identity.squashfs to the shared partition: {e}"
                )
            }
            Error::GenerateSshHostKey(e) => write!(f, "failed to generate ssh host key: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<squashfs::Error> for Error {
    fn from(e: squashfs::Error) -> Self {
        Error::Squashfs(e)
    }
}

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
        Error::WriteToShared(e)
    }
}

impl From<ssh_key::Error> for Error {
    fn from(e: ssh_key::Error) -> Self {
        Error::GenerateSshHostKey(e)
    }
}
