//! I/O on a file, or on a disk device given in its place. A device's
//! metadata length is 0; Linux tells its capacity by seeking to its end,
//! but macOS and Windows don't: there, the disk driver is asked. A Windows
//! disk (`\\.\PhysicalDriveN`, `\\.\E:`) takes whole sectors only, as does
//! a macOS raw node: [`SectorIo`] makes any read or write of them one.

mod block_file;
mod len;
#[cfg(target_os = "macos")]
mod macos;
mod sector;
mod sync;
#[cfg(windows)]
mod windows;

pub use block_file::BlockFile;
pub use len::{len, len_of};
pub use sector::SectorIo;
pub use sync::sync;
