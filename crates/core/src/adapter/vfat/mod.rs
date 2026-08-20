//! Thin wrapper around the `fatfs` crate, narrowed to exactly what the
//! Phase 5 identity/config routines need on a vfat *shared* partition: the
//! filesystem is already formatted (at Yocto build time) — this only ever
//! creates one directory or one file inside an already-existing parent, the
//! same narrow-surface rule `adapter::ext4` follows for ext4. No mkfs
//! here; vfat volumes are never built from scratch by remora-etcher.

pub mod error;
pub mod port;
pub mod service;

pub use error::Error;
pub use port::{Vfat, VfatAdapter};
pub use service::{create_dir, exists, read_file, write_file};
