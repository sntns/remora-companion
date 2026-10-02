mod build_options;

pub use build_options::{
    BuildOptions, Compression, InvalidBuildOptions, DEFAULT_BLOCK_SIZE, MAX_BLOCK_SIZE,
    MIN_BLOCK_SIZE,
};
// Same shape fs-walk already provides, reused as-is (no conversion step)
// rather than duplicated as a parallel squashfs-owned type.
pub use remora_fs_walk::{
    WalkEntry as Entry, WalkEntryKind as EntryKind, WalkEntryMetadata as EntryMetadata,
};
