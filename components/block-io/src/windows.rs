use std::{fs::File, mem, os::windows::io::AsRawHandle, path::Path, ptr};

use windows_sys::Win32::System::{
    Ioctl::{
        DISK_GEOMETRY, GET_LENGTH_INFORMATION, IOCTL_DISK_GET_DRIVE_GEOMETRY,
        IOCTL_DISK_GET_LENGTH_INFO,
    },
    IO::DeviceIoControl,
};

/// A path in the Win32 device namespace (`\\.\PhysicalDrive1`, `\\.\E:`),
/// or a volume by its GUID (`\\?\Volume{...}`): a disk, not a file.
pub fn is_device_path(path: &Path) -> bool {
    let path = path.as_os_str().to_string_lossy();
    path.starts_with(r"\\.\")
        || path
            .get(..11)
            .is_some_and(|p| p.eq_ignore_ascii_case(r"\\?\Volume{"))
}

/// The output `T` of the input-less `ioctl` on `file`, if it answers it.
fn query<T: Default>(file: &File, ioctl: u32) -> Option<T> {
    let mut out = T::default();
    let mut returned = 0;
    // SAFETY: a live handle, no input, and an output buffer of `T`'s size.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            ioctl,
            ptr::null(),
            0,
            (&mut out as *mut T).cast(),
            mem::size_of::<T>() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    };
    (ok != 0).then_some(out)
}

/// A disk's or a volume's capacity; `None` for a file.
pub fn device_len(file: &File) -> Option<u64> {
    query::<GET_LENGTH_INFORMATION>(file, IOCTL_DISK_GET_LENGTH_INFO)
        .and_then(|info| u64::try_from(info.Length).ok())
}

/// The sector size every access to the disk must be made of.
pub fn sector_size(file: &File) -> Option<usize> {
    query::<DISK_GEOMETRY>(file, IOCTL_DISK_GET_DRIVE_GEOMETRY)
        .map(|geometry| geometry.BytesPerSector as usize)
        .filter(|&size| size > 0)
}
