use std::path::Path;

use super::error::Result;

/// DI seam for `remora-etcher-convert-application`: one concrete adapter per
/// whole-disk-image container format (qcow2, gzip). Unlike most verticals'
/// single adapter port, this trait is implemented by more than one
/// concrete adapter at once -- injected directly as
/// `Arc<dyn ContainerFormatAdapter>` (one per format), the same way the
/// image vertical injects its ext4/vfat backends directly rather than
/// through the busybody container, which keys purely on `TypeId` and can't
/// hold two distinct instances behind the same wrapper type.
pub trait ContainerFormatAdapter: Send + Sync {
    /// Decode `input` (in this adapter's format) into a plain raw disk
    /// image at `output_raw`.
    fn decode_to_raw(&self, input: &Path, output_raw: &Path) -> Result<()>;

    /// Encode `input_raw` (a plain raw disk image) into this adapter's
    /// format at `output`.
    fn encode_from_raw(&self, input_raw: &Path, output: &Path) -> Result<()>;
}
