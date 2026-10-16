//! A disk's volumes: which they are, and holding them dismounted while
//! the disk is written. Windows refuses a write into a mounted volume's
//! sectors through the disk's own handle unless that volume is locked or
//! dismounted, and remounts a dismounted one at its next access unless it
//! stays locked: the lock is held for as long as the disk is.

use std::{collections::HashSet, fs::File, io, path::Path};

use windows_sys::Win32::{
    Foundation::{ERROR_ACCESS_DENIED, INVALID_HANDLE_VALUE},
    Storage::FileSystem::{
        FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetVolumePathNamesForVolumeNameW,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
    },
    System::Ioctl::{DISK_EXTENT, FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME},
};

use super::ioctl::{self, from_wide, to_wide};

/// The disk numbers (`N` of `\\.\PhysicalDriveN`) a volume lies on: one,
/// unless it's spanned or mirrored.
pub fn disks_of(volume: &File) -> io::Result<HashSet<u32>> {
    // `VOLUME_DISK_EXTENTS`: a u32 count (padded to 8), then the extents;
    // room for 32 of them, as u64s for the struct's alignment.
    let mut buffer = [0u64; 1 + 32 * 3];
    // SAFETY: the u64 buffer viewed as bytes, for the ioctl to fill.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), buffer.len() * 8)
    };
    ioctl::control(volume, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], bytes)?;
    let count = (buffer[0] & 0xffff_ffff) as usize;
    // SAFETY: the extents follow the 8-byte header, `count` of them (at
    // most what the buffer holds), each a plain `DISK_EXTENT`.
    let extents = unsafe {
        std::slice::from_raw_parts(buffer.as_ptr().add(1).cast::<DISK_EXTENT>(), count.min(32))
    };
    Ok(extents.iter().map(|extent| extent.DiskNumber).collect())
}

/// A volume, by the path it's opened with (`\\?\Volume{...}`, without the
/// trailing backslash that would name its root directory instead).
pub struct Volume {
    pub path: String,
    /// Where it's mounted (`E:\`), for an operator to recognize it.
    pub mounts: Vec<String>,
}

/// Every volume on the disk numbered `disk`.
pub fn volumes_on(disk: u32) -> Vec<Volume> {
    all_volumes()
        .into_iter()
        .filter(|name| {
            let path = name.trim_end_matches('\\');
            ioctl::open_for_query(Path::new(path))
                .and_then(|volume| disks_of(&volume))
                .is_ok_and(|disks| disks.contains(&disk))
        })
        .map(|name| Volume {
            mounts: mount_points(&name),
            path: name.trim_end_matches('\\').to_owned(),
        })
        .collect()
}

/// Every volume's GUID name (`\\?\Volume{...}\`).
fn all_volumes() -> Vec<String> {
    let mut names = Vec::new();
    let mut buffer = [0u16; 64];
    // SAFETY: a buffer of the length given; the search handle is closed
    // once the volumes run out.
    unsafe {
        let search = FindFirstVolumeW(buffer.as_mut_ptr(), buffer.len() as u32);
        if search == INVALID_HANDLE_VALUE {
            return names;
        }
        loop {
            names.push(from_wide(&buffer));
            // Out of volumes (`ERROR_NO_MORE_FILES`) or stopped short: what
            // was found is what there is to go by either way.
            if FindNextVolumeW(search, buffer.as_mut_ptr(), buffer.len() as u32) == 0 {
                break;
            }
        }
        FindVolumeClose(search);
    }
    names
}

/// The drive letters and folders `volume` (`\\?\Volume{...}\`) is mounted
/// at.
fn mount_points(volume: &str) -> Vec<String> {
    let name = to_wide(volume);
    let mut buffer = vec![0u16; 1024];
    let mut needed = 0;
    // SAFETY: a NUL-terminated name, and a buffer of the length given.
    let ok = unsafe {
        GetVolumePathNamesForVolumeNameW(
            name.as_ptr(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Vec::new();
    }
    // A list of NUL-terminated strings, ended by an empty one.
    buffer
        .split(|&c| c == 0)
        .take_while(|s| !s.is_empty())
        .map(from_wide)
        .collect()
}

/// The volumes of a disk, each locked and dismounted, until dropped
/// (closing their handles lets Windows mount them again).
pub struct VolumeLocks {
    _volumes: Vec<File>,
}

/// Why a volume couldn't be locked.
#[derive(Debug)]
pub struct LockError {
    pub volume: String,
    pub error: io::Error,
}

impl LockError {
    /// What an operator can do about it.
    pub fn hint(&self) -> String {
        match self.error.raw_os_error() {
            Some(code) if code == ERROR_ACCESS_DENIED as i32 => {
                format!("{}: in use, or not run as Administrator", self.volume)
            }
            _ => format!("{}: {}", self.volume, self.error),
        }
    }
}

/// Locks and dismounts every volume on the disk numbered `disk`.
pub fn lock_volumes(disk: u32) -> Result<VolumeLocks, LockError> {
    let mut locked = Vec::new();
    for volume in volumes_on(disk) {
        let name = volume
            .mounts
            .first()
            .cloned()
            .unwrap_or_else(|| volume.path.clone());
        let fail = |error| LockError {
            volume: name.clone(),
            error,
        };
        let handle = ioctl::open_for_write(Path::new(&volume.path)).map_err(fail)?;
        ioctl::command(&handle, FSCTL_LOCK_VOLUME).map_err(fail)?;
        ioctl::command(&handle, FSCTL_DISMOUNT_VOLUME).map_err(fail)?;
        locked.push(handle);
    }
    Ok(VolumeLocks { _volumes: locked })
}
