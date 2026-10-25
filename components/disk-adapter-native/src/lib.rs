//! Disk enumeration: list candidate flash targets and answer "is this the
//! system disk?" without shelling out to any external tool, and open one to
//! be overwritten. Linux is implemented via `/sys/block` +
//! `/proc/self/mountinfo`, macOS via DiskArbitration and the IOKit
//! registry, Windows via `\\.\PhysicalDriveN` and its volumes' ioctls; on
//! other OSes every call fails with `Error::Unsupported`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(any(unix, windows))]
mod elevated;
mod service;

pub use service::{serve_helper, DiskAdapterImpl, ElevatingDiskAdapterImpl};
