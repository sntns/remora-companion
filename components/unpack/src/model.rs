use std::{
    fmt,
    io::{self, BufReader, Read},
};

/// Enough of a file's first bytes to tell every format here apart: a tar's
/// magic sits furthest in.
pub(crate) const HEADER_LEN: usize = 512;

/// Where a tar header's magic sits: "ustar", in POSIX and GNU tars alike.
const TAR_MAGIC_OFFSET: usize = 257;
const TAR_MAGIC: &[u8] = b"ustar";

/// Whether `header` (a file's first bytes) is a tar header's.
pub fn is_tar(header: &[u8]) -> bool {
    header
        .get(TAR_MAGIC_OFFSET..TAR_MAGIC_OFFSET + TAR_MAGIC.len())
        .is_some_and(|magic| magic == TAR_MAGIC)
}

/// How an image's bytes are compressed, as its magic says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    Bzip2,
    Gzip,
    Zstd,
}

impl Compression {
    /// The compression `magic` (a file's first bytes) says, if any.
    pub fn of(magic: &[u8]) -> Option<Self> {
        if magic.starts_with(b"BZh") {
            Some(Self::Bzip2)
        } else if magic.starts_with(&[0x1f, 0x8b]) {
            Some(Self::Gzip)
        } else if magic.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
            Some(Self::Zstd)
        } else {
            None
        }
    }
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

/// What [`crate::open`] found in a file: the image's raw bytes, and the
/// `.bmap` that came bundled with them, if any.
pub struct Unpacked<F> {
    pub image: Image<F>,
    /// The `.bmap` (XML) a `.bmaptar` bundle carries; `None` for a plain
    /// image, which isn't in a bundle.
    pub bundled_bmap: Option<String>,
    /// `None`: a raw image.
    pub compression: Option<Compression>,
    /// The image's size, when known without decompressing it all: a raw
    /// image's.
    pub size: Option<u64>,
}

/// An image's raw bytes.
pub enum Image<F> {
    /// A plain raw image: its file itself, which still seeks (to skip
    /// unmapped ranges without reading them).
    File(BufReader<F>),
    /// Decompressed, or a raw image within a bundle: forward only.
    Stream(Box<dyn Read + Send>),
}

impl<F: Read> Read for Image<F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.read(buf),
            Self::Stream(stream) => stream.read(buf),
        }
    }
}
