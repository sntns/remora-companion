use std::path::{Path, PathBuf};

use super::error::Result;

#[derive(Debug)]
pub struct BuildSummary {
    pub entry_count: usize,
    pub bytes_written: u64,
}

/// The squashfs vertical's application-facing port: what every transport
/// (CLI today, anything else later) calls into.
pub trait SquashfsServiceInterface: Send + Sync {
    /// Build a squashfs image from `inputs` (files and/or directories) into
    /// `output`. A directory input has its *contents* merged into the
    /// squashfs root (like `mksquashfs <dir> out.squashfs`); a file input is
    /// placed at the root under its own basename. Real uid/gid/mode from the
    /// host filesystem are preserved (no `-all-root` equivalent).
    fn build(
        &self,
        inputs: &[PathBuf],
        output: &Path,
        options: &crate::model::BuildOptions,
    ) -> Result<BuildSummary>;

    /// List the entries of an existing squashfs image.
    fn inspect(&self, image: &Path) -> Result<Vec<crate::adapter::InspectedEntry>>;
}

/// Injectable handle to whatever `SquashfsServiceInterface` implementation
/// was wired at startup (normally `remora-etcher-squashfs-application`'s
/// `SquashfsControllerImpl`).
#[derive(Clone)]
pub struct SquashfsService(busybody::Service<Box<dyn SquashfsServiceInterface>>);

impl SquashfsService {
    pub fn new<T: SquashfsServiceInterface + 'static>(service: T) -> Self {
        Self(busybody::Service::new(Box::new(service)))
    }
}

impl std::ops::Deref for SquashfsService {
    type Target = busybody::Service<Box<dyn SquashfsServiceInterface>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
