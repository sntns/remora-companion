use std::{
    io::{self, Read, Seek},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use bmap_parser::SeekForward;
use tokio_util::sync::CancellationToken;

use super::error::Result;
use crate::model::Compression;

/// How far a skip goes between two cancel checks: skipping a compressed
/// image's unmapped gigabytes means decompressing them, which takes a
/// while.
const SKIP_STEP: u64 = 16 * 1024 * 1024;

/// An image's raw bytes, decompressed, as the copy reads them: forward-only,
/// so a compressed image, or one inside a bundle, streams straight to the
/// disk without being extracted first. Tracks how far it has been read,
/// which after a full copy is what was written, and how much of that was
/// read rather than skipped, i.e. copied.
pub struct ImageStream {
    inner: Box<dyn Stream>,
    position: Arc<AtomicU64>,
    copied: Arc<AtomicU64>,
    cancel: CancellationToken,
}

trait Stream: Read + SeekForward + Send {}

impl<T: Read + SeekForward + Send> Stream for T {}

impl ImageStream {
    pub fn new<T: Read + SeekForward + Send + 'static>(inner: T) -> Self {
        Self {
            inner: Box::new(inner),
            position: Arc::new(AtomicU64::new(0)),
            copied: Arc::new(AtomicU64::new(0)),
            cancel: CancellationToken::new(),
        }
    }

    pub fn position(&self) -> u64 {
        self.position.load(Ordering::Relaxed)
    }

    /// Track how many bytes the stream has had read (not skipped) in
    /// `copied`, for whoever follows the copy from another thread, and fail
    /// every read and skip once `cancel` fires, so a copy under way stops
    /// promptly.
    pub fn follow(&mut self, copied: Arc<AtomicU64>, cancel: CancellationToken) {
        copied.store(self.copied.load(Ordering::Relaxed), Ordering::Relaxed);
        self.copied = copied;
        self.cancel = cancel;
    }

    fn check_cancelled(&self) -> io::Result<()> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        Ok(())
    }
}

impl Read for ImageStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.check_cancelled()?;
        let n = self.inner.read(buf)?;
        self.position.fetch_add(n as u64, Ordering::Relaxed);
        self.copied.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

impl SeekForward for ImageStream {
    fn seek_forward(&mut self, offset: u64) -> io::Result<()> {
        let mut left = offset;
        while left > 0 {
            self.check_cancelled()?;
            let step = left.min(SKIP_STEP);
            self.inner.seek_forward(step)?;
            self.position.fetch_add(step, Ordering::Relaxed);
            left -= step;
        }
        Ok(())
    }
}

/// What `ImageSourceAdapter::open` found at a path: the image to copy, and
/// the `.bmap` that came bundled with it, if any.
pub struct SourceImage {
    pub stream: ImageStream,
    /// The `.bmap` (XML) a `.bmaptar` bundle carries; `None` for a plain
    /// image, whose bmap, if any, is a file of its own.
    pub bundled_bmap: Option<String>,
    /// The image came in a `.bmaptar` bundle.
    pub bundle: bool,
    /// `None`: a raw image.
    pub compression: Option<Compression>,
    /// The image's size, when known without decompressing it all: a raw
    /// image's.
    pub size: Option<u64>,
}

/// Bytes an image is read from like a file: a local one, or a download
/// that reconnects wherever a seek lands.
pub trait ImageFile: Read + Seek + Send {}

impl<T: Read + Seek + Send> ImageFile for T {}

/// DI seam for `remora-flash-application`: how an image file turns into the
/// bytes to flash.
pub trait ImageSourceAdapter: Send + Sync {
    /// Open `path`: a `.bmaptar` bundle (a tar holding the image and its
    /// `.bmap`) is read in place, anything else is the image itself; the
    /// image, either way, raw or compressed (bzip2, gzip, zstd).
    fn open(&self, path: &Path) -> Result<SourceImage>;
    /// [`Self::open`], from `file`, `len` bytes long.
    fn open_file(&self, file: Box<dyn ImageFile>, len: u64) -> Result<SourceImage>;
}

/// Injectable handle to whatever `ImageSourceAdapter` was wired at startup.
#[derive(Clone)]
pub struct ImageSourceAdapterService(busybody::Service<Box<dyn ImageSourceAdapter>>);

impl ImageSourceAdapterService {
    pub fn new<T: ImageSourceAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for ImageSourceAdapterService {
    type Target = busybody::Service<Box<dyn ImageSourceAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
