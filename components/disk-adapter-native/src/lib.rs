//! Disk enumeration: list candidate flash targets and answer "is this the
//! system disk?" without shelling out to any external tool. Linux is
//! implemented via `/sys/block` + `/proc/self/mountinfo`; other OSes land in
//! a later phase (see the project plan).

#[cfg(target_os = "linux")]
mod linux;

mod service;

pub use service::DiskAdapterImpl;
