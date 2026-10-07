use std::path::PathBuf;

use super::ImageOrigin;

#[derive(Debug, Clone)]
pub struct FlashRequest {
    /// A `.bmaptar` bundle, or a plain image (raw or compressed); local, or
    /// an artifact of a release.
    pub image: ImageOrigin,
    pub bmap: BmapSource,
    pub device: PathBuf,
    /// Bypass the removable-disk check (still refuses a disk identified as
    /// the system disk regardless of this flag).
    pub force: bool,
}

/// Which `.bmap` drives the copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BmapSource {
    /// The one a `.bmaptar` bundle carries; for a plain image, a sibling
    /// `<image>.bmap`, else one named after the image without its
    /// compression extension (`x.wic.bz2` -> `x.wic.bmap`), as `bmaptool`
    /// looks them up; else none.
    Auto,
    /// This file, whatever the image.
    File(PathBuf),
    /// None: the whole image is copied verbatim (no sparse skip, no
    /// checksum verification).
    None,
}

#[derive(Debug)]
pub struct FlashOutcome {
    /// Bytes actually written, i.e. the mapped size when a `.bmap` was used;
    /// the full (decompressed) image size otherwise.
    pub bytes_written: u64,
    pub used_bmap: bool,
}
