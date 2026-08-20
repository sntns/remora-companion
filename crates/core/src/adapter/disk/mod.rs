//! Disk enumeration: list candidate flash targets and answer "is this the
//! system disk?" without shelling out to any external tool. Linux is
//! implemented via `/sys/block` + `/proc/self/mountinfo`; other OSes land in
//! a later phase (see the project plan).

#[cfg(target_os = "linux")]
mod linux;

pub mod error;
pub mod port;
pub mod service;

pub use error::Error;
pub use port::{Disk, DiskAdapter};
pub use service::{enumerate, info};
