//! Thin wrapper around the `bmap-parser` crate (Collabora's Rust rewrite of
//! `bmaptool`). It already provides a ready sparse-copy-with-checksum
//! primitive (`bmap_parser::copy`), so this module does not reimplement the
//! `.bmap` format or the copy loop — it only adapts types/errors.

use std::{fmt, io::Read, io::Write};

use bmap_parser::Bmap;

#[derive(Debug)]
pub enum Error {
    // bmap-parser's XML error type is not nameable from outside the crate
    // (it's defined in a private submodule), so it's boxed here instead.
    ParseBmap(Box<dyn std::error::Error>),
    Copy(bmap_parser::CopyError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::ParseBmap(e) => write!(f, "failed to parse .bmap file: {e}"),
            Error::Copy(e) => write!(f, "failed to copy image: {e}"),
        }
    }
}

impl std::error::Error for Error {}

/// Parsed `.bmap` file plus the handful of fields callers need to report
/// progress (mapped size vs. full image size).
pub struct BlockMap {
    inner: Bmap,
}

impl BlockMap {
    pub fn parse(xml: &str) -> Result<Self, Error> {
        Bmap::from_xml(xml)
            .map(|inner| Self { inner })
            .map_err(|e| Error::ParseBmap(Box::new(e)))
    }

    pub fn image_size(&self) -> u64 {
        self.inner.image_size()
    }

    pub fn total_mapped_size(&self) -> u64 {
        self.inner.total_mapped_size()
    }
}

/// Copy `input` to `output`, writing only the block ranges `map` marks as
/// mapped (skipping the rest, like `bmaptool copy` does). Both sides must
/// support seeking, which any `std::fs::File` — including one opened on a
/// Linux block device special file — already does.
pub fn copy_with_bmap<I, O>(input: &mut I, output: &mut O, map: &BlockMap) -> Result<(), Error>
where
    I: Read + bmap_parser::SeekForward,
    O: Write + bmap_parser::SeekForward,
{
    bmap_parser::copy(input, output, &map.inner).map_err(Error::Copy)
}

/// Copy `input` to `output` verbatim (no `.bmap` available).
pub fn copy_without_bmap<I, O>(input: &mut I, output: &mut O) -> Result<(), Error>
where
    I: Read,
    O: Write,
{
    bmap_parser::copy_nobmap(input, output).map_err(Error::Copy)
}
