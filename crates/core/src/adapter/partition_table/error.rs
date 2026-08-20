use std::{fmt, path::PathBuf};

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
    UnrecognizedFilesystem {
        path: PathBuf,
        offset: u64,
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
            Error::UnrecognizedFilesystem { path, offset } => write!(
                f,
                "partition at offset {offset} of {} is neither ext4 nor vfat \
                 (unrecognized superblock/boot-sector signature)",
                path.display()
            ),
        }
    }
}

impl std::error::Error for Error {}
