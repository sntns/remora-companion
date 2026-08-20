use std::io::{Read, Write};

use bmap_parser::Bmap;

use super::error::Error;

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
