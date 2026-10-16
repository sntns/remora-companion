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

/// `DKIOCSYNCHRONIZECACHE`: `_IO('d', 22)`.
const DKIOCSYNCHRONIZECACHE: libc::c_ulong = 0x2000_6416;

/// Flushes what was written to a disk device, `None` for anything else.
/// `std`'s `sync_all` is `F_FULLFSYNC` here, a filesystem's request a
/// device node refuses (`ENOTTY`): a plain `fsync` flushes a block
/// device's buffer cache (a raw node has none), then the disk driver is
/// asked to empty the drive's own cache -- best effort, as not every
/// driver implements it.
pub fn device_sync(file: &File) -> Option<io::Result<()>> {
    let file_type = match file.metadata() {
        Ok(meta) => meta.file_type(),
        Err(e) => return Some(Err(e)),
    };
    if !file_type.is_block_device() && !file_type.is_char_device() {
        return None;
    }
    // SAFETY: a live descriptor.
    if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::ENOTTY | libc::ENOTSUP | libc::EINVAL)
        ) {
            return Some(Err(error));
        }
    }
    // SAFETY: a live descriptor; the ioctl takes no argument.
    unsafe { libc::ioctl(file.as_raw_fd(), DKIOCSYNCHRONIZECACHE) };
    Some(Ok(()))
}
