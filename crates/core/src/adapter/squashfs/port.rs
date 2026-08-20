use std::fs::File;

use crate::model::squashfs::{BuildOptions, Entry};

use super::error::Error;
use super::service::{self, InspectedEntry};

/// DI seam for `application::squashfs`: swap the real `backhand` writer/
/// reader for a test double without touching the build/inspect use cases.
pub trait SquashfsAdapter {
    fn write(
        &self,
        entries: &[Entry],
        options: &BuildOptions,
        image_mtime: u32,
        root_owner: (u32, u32),
        out: File,
    ) -> Result<u64, Error>;

    fn inspect(&self, input: File) -> Result<Vec<InspectedEntry>, Error>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Squashfs;

impl SquashfsAdapter for Squashfs {
    fn write(
        &self,
        entries: &[Entry],
        options: &BuildOptions,
        image_mtime: u32,
        root_owner: (u32, u32),
        out: File,
    ) -> Result<u64, Error> {
        service::write(entries, options, image_mtime, root_owner, out)
    }

    fn inspect(&self, input: File) -> Result<Vec<InspectedEntry>, Error> {
        service::inspect(input)
    }
}
