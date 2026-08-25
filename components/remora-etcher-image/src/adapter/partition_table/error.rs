use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to open {0}")]
    Open(PathBuf),
    #[error("failed to seek")]
    Seek,
    #[error("no valid partition table found (as GPT: {gpt}; as MBR: {mbr})")]
    NoValidTable {
        gpt: Box<dyn std::error::Error + Send + Sync>,
        mbr: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error(
        "partition at offset {offset} of {} is neither ext4 nor vfat \
         (unrecognized superblock/boot-sector signature)",
        path.display()
    )]
    UnrecognizedFilesystem { path: PathBuf, offset: u64 },
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
