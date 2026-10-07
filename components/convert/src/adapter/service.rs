use std::path::Path;

use super::error::Result;

/// How much of a file's start [`ContainerFormatAdapter::recognizes`] is
/// shown: enough for every format's magic, a tar's sitting furthest in.
pub const HEADER_LEN: usize = 512;

/// DI seam for `remora-convert-application`: one concrete adapter per
/// whole-disk-image container format (qcow2, gzip, zstd, bzip2, bmaptar).
/// Unlike most verticals' single adapter port, this trait is implemented by
/// more than one concrete adapter at once -- injected directly as
/// `Arc<dyn ContainerFormatAdapter>` (one per format), the same way the
/// image vertical injects its ext4/vfat backends directly rather than
/// through the busybody container, which keys purely on `TypeId` and can't
/// hold two distinct instances behind the same wrapper type.
pub trait ContainerFormatAdapter: Send + Sync {
    /// Whether `header` (a file's first [`HEADER_LEN`] bytes, fewer for a
    /// shorter file) starts the way this adapter's format does: what tells
    /// a mislabelled or compressed input from a raw image.
    fn recognizes(&self, header: &[u8]) -> bool;

    /// The size of the raw image `input` decodes to, when its format says
    /// so up front (a `.bmaptar`'s `.bmap` does): a progress total. `None`
    /// when it would take decoding it all to know.
    fn decoded_size(&self, input: &Path) -> Result<Option<u64>> {
        let _ = input;
        Ok(None)
    }

    /// Decode `input` (in this adapter's format) into a plain raw disk
    /// image at `output_raw`.
    fn decode_to_raw(&self, input: &Path, output_raw: &Path) -> Result<()>;

    /// Encode `input_raw` (a plain raw disk image) into this adapter's
    /// format at `output`.
    fn encode_from_raw(&self, input_raw: &Path, output: &Path) -> Result<()>;
}
