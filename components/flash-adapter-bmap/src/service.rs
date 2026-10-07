use std::fs::File;

use error_stack::ResultExt;
use remora_flash::adapter::{
    bmap::{BlockMap, BmapAdapter, Error, Result},
    source::ImageStream,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct BmapAdapterImpl;

impl BmapAdapter for BmapAdapterImpl {
    fn parse(&self, xml: &str) -> Result<BlockMap> {
        bmap_parser::Bmap::from_xml(xml)
            .map(BlockMap::from_parsed)
            .map_err(|e| error_stack::Report::new(Error::ParseBmap).attach(e.to_string()))
    }

    fn copy_with_bmap(
        &self,
        input: &mut ImageStream,
        output: &mut File,
        map: &BlockMap,
    ) -> Result<()> {
        bmap_parser::copy(input, output, map.as_parsed()).change_context(Error::Copy)
    }

    fn copy_without_bmap(&self, input: &mut ImageStream, output: &mut File) -> Result<()> {
        bmap_parser::copy_nobmap(input, output).change_context(Error::Copy)
    }
}
