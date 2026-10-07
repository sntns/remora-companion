use std::fs::File;

use super::error::Result;
use crate::adapter::source::ImageStream;

/// Parsed `.bmap` file plus the handful of fields callers need to report
/// progress (mapped size vs. full image size). Only ever produced by a real
/// `BmapAdapter::parse` (or a test double standing in for one) — never
/// constructed ad hoc by application code.
pub struct BlockMap {
    inner: bmap_parser::Bmap,
}

impl BlockMap {
    /// Wrap an already-parsed `bmap_parser::Bmap`. Called from
    /// `remora-flash-adapter-bmap`'s real `parse` implementation (or a
    /// test double building a synthetic map); not meant to be called
    /// directly by application code.
    pub fn from_parsed(inner: bmap_parser::Bmap) -> Self {
        Self { inner }
    }

    pub fn image_size(&self) -> u64 {
        self.inner.image_size()
    }

    pub fn total_mapped_size(&self) -> u64 {
        self.inner.total_mapped_size()
    }

    pub fn as_parsed(&self) -> &bmap_parser::Bmap {
        &self.inner
    }
}

/// DI seam for `remora-flash-application`: swap the real
/// `bmap-parser` copy for a test double without touching the flash use case.
pub trait BmapAdapter: Send + Sync {
    fn parse(&self, xml: &str) -> Result<BlockMap>;
    fn copy_with_bmap(
        &self,
        input: &mut ImageStream,
        output: &mut File,
        map: &BlockMap,
    ) -> Result<()>;
    fn copy_without_bmap(&self, input: &mut ImageStream, output: &mut File) -> Result<()>;
}

/// Injectable handle to whatever `BmapAdapter` was wired at startup.
#[derive(Clone)]
pub struct BmapAdapterService(busybody::Service<Box<dyn BmapAdapter>>);

impl BmapAdapterService {
    pub fn new<T: BmapAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for BmapAdapterService {
    type Target = busybody::Service<Box<dyn BmapAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
