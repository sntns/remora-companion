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
    #[error("no partition #{index} in {}", path.display())]
    NoSuchPartition { path: PathBuf, index: u32 },
    #[error(
        "partition #{index} is not the last one of {}: only the last partition \
         can be resized (another one starts after it)",
        path.display()
    )]
    NotLastPartition { path: PathBuf, index: u32 },
    #[error("{size} bytes is not a whole number of {sector_size}-byte sectors")]
    UnalignedSize { size: u64, sector_size: u64 },
    #[error("{size} bytes is more than {}'s MBR can address", path.display())]
    TooBig { path: PathBuf, size: u64 },
    #[error("failed to write the partition table of {0}")]
    Write(PathBuf),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;
