use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
};

use error_stack::ResultExt;

use crate::error::{Error, Result};

/// The block a zero run must fill to be skipped: bmaptool's, and every
/// usual filesystem's.
pub const BLOCK_SIZE: usize = 4096;

/// Decoded input read at a time: a whole number of blocks.
const CHUNK: usize = 256 * BLOCK_SIZE;

/// Copy `reader` into `output` (empty, at its start), seeking over each
/// all-zero block instead of writing it, as `zstd -d` does: the raw image
/// comes out sparse on any filesystem that has holes, a decoded disk being
/// mostly empty. Says how many bytes the image is.
pub fn write_sparse(reader: &mut impl Read, output: &mut File) -> Result<u64> {
    let mut buf = vec![0; CHUNK];
    // Where `buf` starts in the image, and where `output`'s cursor is.
    let mut position = 0u64;
    let mut cursor = 0u64;
    loop {
        let n = fill(reader, &mut buf).change_context(Error::Read)?;
        if n == 0 {
            break;
        }
        let mut at = 0;
        while at < n {
            if is_zero(block(&buf[..n], at)) {
                at += BLOCK_SIZE;
                continue;
            }
            // One write for a run of data blocks, not one per block.
            let start = at;
            while at < n && !is_zero(block(&buf[..n], at)) {
                at += BLOCK_SIZE;
            }
            let end = at.min(n);
            let offset = position + start as u64;
            if cursor != offset {
                output
                    .seek(SeekFrom::Start(offset))
                    .change_context(Error::Write)?;
            }
            output
                .write_all(&buf[start..end])
                .change_context(Error::Write)?;
            cursor = position + end as u64;
        }
        position += n as u64;
    }
    // Covers a trailing run of zero blocks, never written.
    output.set_len(position).change_context(Error::Write)?;
    Ok(position)
}

/// The block of `data` at `at`; the last one may be short.
fn block(data: &[u8], at: usize) -> &[u8] {
    &data[at..(at + BLOCK_SIZE).min(data.len())]
}

fn is_zero(block: &[u8]) -> bool {
    block.iter().all(|&b| b == 0)
}

/// Read into `buf` until it's full or the input ends, so blocks stay
/// aligned however the reader splits its reads. 0: the end.
fn fill(reader: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match reader.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(read) => n += read,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use remora_scratch::ScratchDir;

    use super::*;

    /// Reads at most 1000 bytes at a time: never block-aligned.
    struct Trickle<R>(R);

    impl<R: Read> Read for Trickle<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let len = buf.len().min(1000);
            self.0.read(&mut buf[..len])
        }
    }

    #[test]
    fn zero_blocks_are_skipped_and_the_bytes_kept() {
        let dir = ScratchDir::new("remora-unpack-test").unwrap();
        let mut image = vec![0u8; 3 * CHUNK + 1234];
        image[10] = 1;
        image[CHUNK - 1] = 2;
        image[CHUNK] = 3;
        image[2 * CHUNK + 5 * BLOCK_SIZE..2 * CHUNK + 9 * BLOCK_SIZE].fill(4);
        let path = dir.join("raw");
        let mut output = File::create(&path).unwrap();

        let len = write_sparse(&mut Trickle(&image[..]), &mut output).unwrap();

        assert_eq!(len, image.len() as u64);
        assert_eq!(fs::read(&path).unwrap(), image);
    }

    #[test]
    fn a_trailing_short_block_of_data_is_written() {
        let dir = ScratchDir::new("remora-unpack-test").unwrap();
        let mut image = vec![0u8; 2 * BLOCK_SIZE + 10];
        image[2 * BLOCK_SIZE + 9] = 0xee;
        let path = dir.join("raw");
        let mut output = File::create(&path).unwrap();

        write_sparse(&mut &image[..], &mut output).unwrap();

        assert_eq!(fs::read(&path).unwrap(), image);
    }

    #[test]
    #[cfg(unix)]
    fn an_empty_disk_takes_no_room() {
        use std::os::unix::fs::MetadataExt;

        let dir = ScratchDir::new("remora-unpack-test").unwrap();
        let image = vec![0u8; 64 << 20];
        let path = dir.join("raw");
        let mut output = File::create(&path).unwrap();

        write_sparse(&mut &image[..], &mut output).unwrap();

        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.len(), image.len() as u64);
        assert_eq!(meta.blocks(), 0);
    }
}
