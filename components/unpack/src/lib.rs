//! Turns an image file into its raw bytes: a `.bmaptar` bundle (an
//! uncompressed tar of the image and its `.bmap`, as meta-remora builds it)
//! is read in place, without extracting it; the image itself, bundled or
//! not, is decompressed on the fly when it's bzip2 (on every core), gzip or
//! zstd. And back to a raw file: [`write_sparse`] skips the zero blocks.
//!
//! Plain functions rather than a DI-injected port, like `remora-scratch`:
//! each vertical reading images already has its own port in front of this
//! (flash's `ImageSourceAdapter`, convert's `ContainerFormatAdapter`), which
//! is what its tests substitute; this is only the decoding they share.

mod error;
mod model;
mod parallel_bzip2;
mod service;
mod sparse;

pub use error::{Error, Result};
pub use model::{is_tar, Compression, Image, Unpacked};
pub use parallel_bzip2::ParallelBzDecoder;
pub use service::{open, open_path};
pub use sparse::{write_sparse, BLOCK_SIZE};
