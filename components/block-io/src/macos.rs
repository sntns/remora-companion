use std::{
    fs::File,
    io,
    os::{fd::AsRawFd, unix::fs::FileTypeExt},
};

/// `DKIOCGETBLOCKSIZE`: `_IOR('d', 24, uint32_t)`.
const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
/// `DKIOCGETBLOCKCOUNT`: `_IOR('d', 25, uint64_t)`.
const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;

/// A disk device's capacity, which macOS doesn't seek to the end of (the
/// seek lands at 0); `None` for anything else.
pub fn device_len(file: &File) -> io::Result<Option<u64>> {
    let file_type = file.metadata()?.file_type();
    if !file_type.is_block_device() && !file_type.is_char_device() {
        return Ok(None);
    }
    let (mut size, mut count): (u32, u64) = (0, 0);
    // SAFETY: a live descriptor, and integers of the ioctls' own sizes for
    // them to fill.
    let asked = unsafe {
        libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKSIZE, &mut size as *mut u32) == 0
            && libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKCOUNT, &mut count as *mut u64) == 0
    };
    if !asked {
        // A character device that isn't a disk (`/dev/null`): no length.
        return Err(io::Error::last_os_error());
    }
    Ok(Some(count * u64::from(size)))
}
