//! Thin wrapper around the `bmap-parser` crate (Collabora's Rust rewrite of
//! `bmaptool`). It already provides a ready sparse-copy-with-checksum
//! primitive (`bmap_parser::copy`), so this module does not reimplement the
//! `.bmap` format or the copy loop — it only adapts types/errors.

mod service;

pub use service::BmapAdapterImpl;
