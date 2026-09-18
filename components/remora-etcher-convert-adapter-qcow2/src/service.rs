use std::path::Path;

use remora_etcher_convert::adapter::{ContainerFormatAdapter, Result};

use crate::{decode, encode};

#[derive(Debug, Default, Clone, Copy)]
pub struct Qcow2AdapterImpl;

impl ContainerFormatAdapter for Qcow2AdapterImpl {
    fn decode_to_raw(&self, input: &Path, output_raw: &Path) -> Result<()> {
        decode::decode(input, output_raw)
    }

    fn encode_from_raw(&self, input_raw: &Path, output: &Path) -> Result<()> {
        encode::encode(input_raw, output)
    }
}
