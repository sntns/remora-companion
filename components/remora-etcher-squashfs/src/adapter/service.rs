use std::{fs::File, path::PathBuf};

use crate::model::{BuildOptions, Entry};

use super::error::Result;

/// Summary of one entry in an existing squashfs image, for `squashfs inspect`.
pub struct InspectedEntry {
    pub path: PathBuf,
    pub kind: &'static str,
    pub permissions: u16,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u32,
}

/// DI seam for `remora-etcher-squashfs-application`: swap the real `backhand`
/// writer/reader for a test double without touching the build/inspect use
/// cases.
pub trait SquashfsAdapter: Send + Sync {
    fn write(
        &self,
        entries: &[Entry],
        options: &BuildOptions,
        image_mtime: u32,
        root_owner: (u32, u32),
        out: File,
    ) -> Result<u64>;

    fn inspect(&self, input: File) -> Result<Vec<InspectedEntry>>;
}

/// Injectable handle to whatever `SquashfsAdapter` was wired at startup.
#[derive(Clone)]
pub struct SquashfsAdapterService(busybody::Service<Box<dyn SquashfsAdapter>>);

impl SquashfsAdapterService {
    pub fn new<T: SquashfsAdapter + 'static>(adapter: T) -> Self {
        Self(busybody::Service::new(Box::new(adapter)))
    }
}

impl std::ops::Deref for SquashfsAdapterService {
    type Target = busybody::Service<Box<dyn SquashfsAdapter>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
