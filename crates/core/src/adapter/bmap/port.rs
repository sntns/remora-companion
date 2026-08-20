use std::fs::File;

use super::error::Error;
use super::service::{self, BlockMap};

/// DI seam for `application::flash`: swap the real `bmap-parser` copy for a
/// test double without touching the flash use case.
pub trait BmapAdapter {
    fn parse(&self, xml: &str) -> Result<BlockMap, Error>;
    fn copy_with_bmap(
        &self,
        input: &mut File,
        output: &mut File,
        map: &BlockMap,
    ) -> Result<(), Error>;
    fn copy_without_bmap(&self, input: &mut File, output: &mut File) -> Result<(), Error>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Bmap;

impl BmapAdapter for Bmap {
    fn parse(&self, xml: &str) -> Result<BlockMap, Error> {
        BlockMap::parse(xml)
    }

    fn copy_with_bmap(
        &self,
        input: &mut File,
        output: &mut File,
        map: &BlockMap,
    ) -> Result<(), Error> {
        service::copy_with_bmap(input, output, map)
    }

    fn copy_without_bmap(&self, input: &mut File, output: &mut File) -> Result<(), Error> {
        service::copy_without_bmap(input, output)
    }
}
