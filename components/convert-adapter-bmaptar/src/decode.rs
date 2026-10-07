use std::{fs::File, path::Path};

use bmap_parser::{Bmap, CopyError, Discarder};
use error_stack::{Report, ResultExt};
use remora_convert::adapter::{Error, Result};
use remora_unpack::{Image, Unpacked};

/// `input`'s image, still to read, and its `.bmap`.
pub(crate) fn open(input: &Path) -> Result<(Unpacked<File>, Bmap)> {
    let unpacked = remora_unpack::open_path(input)
        .change_context_lazy(|| Error::InvalidHeader(input.to_path_buf()))?;
    let xml = unpacked
        .bundled_bmap
        .as_deref()
        .ok_or_else(|| Report::new(Error::InvalidHeader(input.to_path_buf())))?;
    let bmap =
        Bmap::from_xml(xml).change_context_lazy(|| Error::InvalidHeader(input.to_path_buf()))?;
    Ok((unpacked, bmap))
}

/// Decode `input` (a `.bmaptar`) into a raw image at `output_raw`, sized to
/// its `.bmap`'s image size: only the mapped ranges are written, each
/// checked against its checksum as it goes, the rest left as holes -- the
/// image decompressed in place, never extracted.
pub(crate) fn decode(input: &Path, output_raw: &Path) -> Result<()> {
    let (unpacked, bmap) = open(input)?;
    let mut outfile = File::create(output_raw)
        .change_context_lazy(|| Error::WriteFile(output_raw.to_path_buf()))?;
    let mut image: Discarder<Image<File>> = Discarder::new(unpacked.image);
    bmap_parser::copy(&mut image, &mut outfile, &bmap).map_err(|e| {
        let context = match e {
            CopyError::ChecksumError => Error::Checksum(input.to_path_buf()),
            CopyError::WriteError(_) => Error::WriteFile(output_raw.to_path_buf()),
            CopyError::ReadError(_) | CopyError::UnexpectedEof => {
                Error::ReadFile(input.to_path_buf())
            }
        };
        Report::new(e).change_context(context)
    })?;
    // The holes after the last mapped range.
    outfile
        .set_len(bmap.image_size())
        .change_context_lazy(|| Error::WriteFile(output_raw.to_path_buf()))
}
