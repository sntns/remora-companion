use std::{fmt, path::PathBuf};

/// How an image's bytes are compressed, as its magic says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Bzip2,
    Gzip,
    Zstd,
}

impl fmt::Display for Compression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Bzip2 => "bzip2",
            Self::Gzip => "gzip",
            Self::Zstd => "zstd",
        })
    }
}

/// What a flash is about to write, read from the image and its `.bmap`
/// without copying anything: for the operator to see before it starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSummary {
    /// The image came in a `.bmaptar` bundle.
    pub bundle: bool,
    /// `None`: a raw image.
    pub compression: Option<Compression>,
    /// The full (decompressed) image's size: its `.bmap`'s, else a raw
    /// image's own; `None` for a compressed image without a `.bmap`.
    pub image_size: Option<u64>,
    /// `None`: the whole image is copied verbatim, unverified.
    pub bmap: Option<BmapSummary>,
}

impl ImageSummary {
    /// What the copy will actually write: the mapped ranges with a `.bmap`,
    /// else the whole image (if its size is known).
    pub fn bytes_to_write(&self) -> Option<u64> {
        match &self.bmap {
            Some(bmap) => Some(bmap.mapped_size),
            None => self.image_size,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BmapSummary {
    pub origin: BmapOrigin,
    /// Bytes in the mapped ranges, the only ones written.
    pub mapped_size: u64,
    /// How many mapped ranges, each checked against its own checksum.
    pub ranges: usize,
    pub checksum: &'static str,
}

/// Where the `.bmap` came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BmapOrigin {
    /// Carried by the `.bmaptar` bundle.
    Bundled,
    /// A file of its own: given, or found next to the image.
    File(PathBuf),
}
