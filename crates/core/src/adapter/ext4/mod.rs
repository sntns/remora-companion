//! Thin wrapper around `am-fs-ext4` (the `fs_ext4` crate — validated in the
//! phase-3 spike, see the "spike: validate am-fs-ext4 for phase 3" commit),
//! narrowed to exactly the operation `remora-etcher image partition cp`
//! needs: create one file inside an already-existing directory of an ext4
//! filesystem that lives at a byte-range window inside a larger disk image
//! or device, and write its content.
//!
//! Deliberately not journaled beyond what `apply_create`/`apply_pwrite`
//! themselves do — this is a one-shot CLI operation on a device that isn't
//! concurrently mounted elsewhere, the same class of risk `wic cp`/`debugfs`
//! already carry.

pub mod error;
pub mod port;
pub mod service;

pub use error::Error;
pub use port::{Ext4, Ext4Adapter};
pub use service::{create_dir, exists, format, read_file, write_file};
