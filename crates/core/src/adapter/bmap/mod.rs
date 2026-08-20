//! Thin wrapper around the `bmap-parser` crate (Collabora's Rust rewrite of
//! `bmaptool`). It already provides a ready sparse-copy-with-checksum
//! primitive (`bmap_parser::copy`), so this module does not reimplement the
//! `.bmap` format or the copy loop — it only adapts types/errors.

pub mod error;
pub mod port;
pub mod service;

pub use error::Error;
pub use port::{Bmap, BmapAdapter};
pub use service::{copy_with_bmap, copy_without_bmap, BlockMap};
