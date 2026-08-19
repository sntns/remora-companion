//! Reads whichever partition table (`mbrman` for MBR, `gptman` for GPT) is
//! actually on disk, auto-detected: GPT is tried first (it starts with a
//! protective MBR, so a plain MBR reader would otherwise misread it as one
//! giant 0xEE partition), falling back to plain MBR.

use std::{
    fmt,
    fs::File,
    io::{Seek, SeekFrom},
    path::{Path, PathBuf},
};

mod gpt_gptman;
mod mbr_mbrman;

use crate::model::partition_table::PartitionTable;

#[derive(Debug)]
pub enum Error {
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    Seek(std::io::Error),
    NoValidTable {
        gpt: Box<dyn std::error::Error>,
        mbr: Box<dyn std::error::Error>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Open { path, source } => {
                write!(f, "failed to open {}: {source}", path.display())
            }
            Error::Seek(e) => write!(f, "failed to seek: {e}"),
            Error::NoValidTable { gpt, mbr } => write!(
                f,
                "no valid partition table found (as GPT: {gpt}; as MBR: {mbr})"
            ),
        }
    }
}

impl std::error::Error for Error {}

pub fn read(path: &Path) -> Result<PartitionTable, Error> {
    let mut file = File::open(path).map_err(|source| Error::Open {
        path: path.to_path_buf(),
        source,
    })?;

    let gpt_err = match gpt_gptman::read(&mut file) {
        Ok(table) => return Ok(table),
        Err(e) => e,
    };

    file.seek(SeekFrom::Start(0)).map_err(Error::Seek)?;

    match mbr_mbrman::read(&mut file) {
        Ok(table) => Ok(table),
        Err(mbr_err) => Err(Error::NoValidTable {
            gpt: gpt_err,
            mbr: mbr_err,
        }),
    }
}
