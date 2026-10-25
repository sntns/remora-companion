use std::path::Path;

use remora_disk::{
    adapter::{DiskAdapter, DiskHold, DiskTarget, Result},
    model::DiskInfo,
};

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use remora_disk::adapter::Error;

#[cfg(target_os = "linux")]
use crate::linux as native;
#[cfg(target_os = "macos")]
use crate::macos as native;
#[cfg(windows)]
use crate::windows as native;

#[derive(Debug, Default, Clone, Copy)]
pub struct DiskAdapterImpl;

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
impl DiskAdapter for DiskAdapterImpl {
    fn enumerate(&self) -> Result<Vec<DiskInfo>> {
        native::enumerate()
    }

    fn info(&self, path: &Path) -> Result<DiskInfo> {
        native::info(path)
    }

    fn open(&self, disk: &DiskInfo) -> Result<DiskTarget> {
        native::open(disk)
    }

    fn check_writable(&self, disk: &DiskInfo) -> Result<()> {
        native::check_writable(disk)
    }

    fn hold(&self, path: &Path) -> Result<Option<DiskHold>> {
        native::hold(path)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
impl DiskAdapter for DiskAdapterImpl {
    fn enumerate(&self) -> Result<Vec<DiskInfo>> {
        Err(error_stack::Report::new(Error::Unsupported))
    }

    fn info(&self, _path: &Path) -> Result<DiskInfo> {
        Err(error_stack::Report::new(Error::Unsupported))
    }

    fn open(&self, _disk: &DiskInfo) -> Result<DiskTarget> {
        Err(error_stack::Report::new(Error::Unsupported))
    }

    /// No disk is ever written here but an image file: nothing to hold.
    fn hold(&self, _path: &Path) -> Result<Option<DiskHold>> {
        Ok(None)
    }
}

/// [`DiskAdapterImpl`], and a disk only an administrator may write opened
/// by an elevated helper -- this program run again with `helper_args` (a
/// hidden command running [`serve_helper`]), with administrator rights
/// asked for the platform's way. What wires it provides that command.
#[derive(Debug, Clone)]
pub struct ElevatingDiskAdapterImpl {
    #[cfg(any(unix, windows))]
    helper: crate::elevated::Helper,
}

impl ElevatingDiskAdapterImpl {
    pub fn new(helper_args: Vec<std::ffi::OsString>) -> Self {
        #[cfg(not(any(unix, windows)))]
        let _ = helper_args;
        Self {
            #[cfg(any(unix, windows))]
            helper: crate::elevated::Helper { args: helper_args },
        }
    }
}

impl DiskAdapter for ElevatingDiskAdapterImpl {
    fn enumerate(&self) -> Result<Vec<DiskInfo>> {
        DiskAdapterImpl.enumerate()
    }

    fn info(&self, path: &Path) -> Result<DiskInfo> {
        DiskAdapterImpl.info(path)
    }

    fn open(&self, disk: &DiskInfo) -> Result<DiskTarget> {
        DiskAdapterImpl.open(disk)
    }

    fn check_writable(&self, disk: &DiskInfo) -> Result<()> {
        DiskAdapterImpl.check_writable(disk)
    }

    #[cfg(any(unix, windows))]
    fn open_elevated(&self, disk: &DiskInfo) -> Result<DiskTarget> {
        self.helper.open(disk)
    }

    fn hold(&self, path: &Path) -> Result<Option<DiskHold>> {
        DiskAdapterImpl.hold(path)
    }
}

/// The elevated helper's whole job: connect to `channel`, open the disk at
/// `disk` -- never the system disk, whatever asks -- and write it as told
/// until hung up on.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub fn serve_helper(channel: &str, disk: &Path) -> std::io::Result<()> {
    crate::elevated::serve(channel, disk, |path| {
        let info = native::info(path)?;
        if info.is_system_disk {
            return Err(error_stack::Report::new(
                remora_disk::adapter::Error::SystemDisk(info.path),
            ));
        }
        native::open(&info)
    })
}

/// No disk is ever written here.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub fn serve_helper(_channel: &str, _disk: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "disk access is not yet implemented on this OS",
    ))
}
