use std::{fs::File, io};

use remora_unpack::BLOCK_SIZE;

const BLOCK: u64 = BLOCK_SIZE as u64;

/// The blocks of `file` (`len` bytes) that hold data, as inclusive ranges
/// of block numbers in order -- the `.bmap`'s `Range`s. A block is mapped
/// when any of it is data, as bmaptool's FIEMAP map has it.
///
/// Data is what the file's own holes say, not what reads as zero: a block
/// written with zeros (inside a file of the image's filesystem, say) must
/// be written to the disk too, whatever an earlier use of it left there --
/// only a hole is a block nothing ever wrote, which nothing reads.
pub(crate) fn mapped_blocks(file: &File, len: u64) -> io::Result<Vec<(u64, u64)>> {
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    for (start, end) in data_extents(file, len)? {
        let first = start / BLOCK;
        let last = end.div_ceil(BLOCK) - 1;
        match ranges.last_mut() {
            // Two extents within one block, or back to back.
            Some((_, previous)) if first <= *previous + 1 => *previous = last.max(*previous),
            _ => ranges.push((first, last)),
        }
    }
    Ok(ranges)
}

/// `file`'s data, as non-empty `[start, end)` byte extents in order.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
))]
fn data_extents(file: &File, len: u64) -> io::Result<Vec<(u64, u64)>> {
    use std::os::fd::AsRawFd;

    let fd = file.as_raw_fd();
    let mut extents = Vec::new();
    let mut offset = 0u64;
    while offset < len {
        // SAFETY: lseek only moves the offset of a descriptor `file` owns
        // and keeps open for the whole call.
        let data = unsafe { libc::lseek(fd, offset as libc::off_t, libc::SEEK_DATA) };
        if data < 0 {
            let err = io::Error::last_os_error();
            return match err.raw_os_error() {
                // Nothing but holes from `offset` on.
                Some(libc::ENXIO) => Ok(extents),
                // A system that can't tell: all of it is data.
                Some(libc::EINVAL) => Ok(vec![(0, len)]),
                _ => Err(err),
            };
        }
        // SAFETY: as above.
        let hole = unsafe { libc::lseek(fd, data, libc::SEEK_HOLE) };
        if hole < 0 {
            return Err(io::Error::last_os_error());
        }
        let (data, hole) = (data as u64, (hole as u64).min(len));
        if data >= hole {
            break;
        }
        extents.push((data, hole));
        offset = hole;
    }
    Ok(extents)
}

/// No way to ask for holes here: all of it is data, as bmaptool maps a file
/// on a system without FIEMAP or SEEK_HOLE.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd"
)))]
fn data_extents(_file: &File, len: u64) -> io::Result<Vec<(u64, u64)>> {
    Ok(if len == 0 { Vec::new() } else { vec![(0, len)] })
}

// Holes as Linux filesystems make them, block by block.
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::os::unix::fs::FileExt;

    use remora_scratch::ScratchDir;

    use super::*;

    #[test]
    fn holes_are_unmapped_and_written_zeros_mapped() {
        let dir = ScratchDir::new("remora-convert-adapter-bmaptar-test").unwrap();
        let path = dir.join("disk.wic");
        let file = File::create(&path).unwrap();
        file.set_len(100 * BLOCK + 10).unwrap();
        file.write_all_at(&[1; 10], 5).unwrap();
        // Written, if only with zeros: data.
        file.write_all_at(&[0; 2 * BLOCK_SIZE], 10 * BLOCK).unwrap();
        file.write_all_at(&[2; 1], 11 * BLOCK + 7).unwrap();
        file.write_all_at(&[3; 1], 100 * BLOCK + 9).unwrap();

        let ranges = mapped_blocks(&File::open(&path).unwrap(), 100 * BLOCK + 10).unwrap();

        assert_eq!(ranges, vec![(0, 0), (10, 11), (100, 100)]);
    }

    #[test]
    fn a_file_of_holes_maps_nothing() {
        let dir = ScratchDir::new("remora-convert-adapter-bmaptar-test").unwrap();
        let path = dir.join("disk.wic");
        File::create(&path).unwrap().set_len(1 << 20).unwrap();

        let ranges = mapped_blocks(&File::open(&path).unwrap(), 1 << 20).unwrap();

        assert!(ranges.is_empty());
    }
}
