use std::path::Path;

use remora_convert::adapter::{ContainerFormatAdapter, Result};

use crate::{decode, encode};

#[derive(Debug, Default, Clone, Copy)]
pub struct BmaptarAdapterImpl;

impl ContainerFormatAdapter for BmaptarAdapterImpl {
    fn recognizes(&self, header: &[u8]) -> bool {
        remora_unpack::is_tar(header)
    }

    fn decoded_size(&self, input: &Path) -> Result<Option<u64>> {
        decode::open(input).map(|(_, bmap)| Some(bmap.image_size()))
    }

    fn decode_to_raw(&self, input: &Path, output_raw: &Path) -> Result<()> {
        decode::decode(input, output_raw)
    }

    fn encode_from_raw(&self, input_raw: &Path, output: &Path) -> Result<()> {
        encode::encode(input_raw, output)
    }
}
