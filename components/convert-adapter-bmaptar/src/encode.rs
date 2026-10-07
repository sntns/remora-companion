use std::{
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    thread,
    time::UNIX_EPOCH,
};

use error_stack::{Report, ResultExt};
use remora_convert::adapter::{Error, Result};
use remora_unpack::BLOCK_SIZE;
use sha2::{Digest, Sha256};

use crate::{bmap, map};

/// oe-core's `ZSTD_COMPRESSION_LEVEL`, which meta-remora's images are
/// compressed at.
const LEVEL: i32 = 3;
/// A tar header's, and its members' padding unit.
const TAR_BLOCK: u64 = 512;
/// What GNU tar pads an archive to (its default blocking factor, 20).
const TAR_RECORD: u64 = 20 * TAR_BLOCK;

/// Encode `input_raw` into a `.bmaptar` at `output`, laid out as
/// meta-remora's: `<name>.bmap` then `<name>.zst`, `<name>` being
/// `output`'s file name without `.bmaptar` (`x.wic.bmaptar` holds
/// `x.wic.bmap` and `x.wic.zst`, which `bmaptool copy` pairs once
/// extracted).
///
/// Two passes over the raw image: the `.bmap` must come first in the tar,
/// so it's made first (mapping the file's holes, hashing its data), then
/// the image is compressed straight into the tar, its member's header
/// written last, once its size is known.
pub(crate) fn encode(input_raw: &Path, output: &Path) -> Result<()> {
    let read_err = || Error::ReadFile(input_raw.to_path_buf());
    let write_err = || Error::WriteFile(output.to_path_buf());
    let name = member_stem(output)?;

    let mut raw = File::open(input_raw).change_context_lazy(read_err)?;
    let meta = raw.metadata().change_context_lazy(read_err)?;
    let len = meta.len();
    if len == 0 {
        // bmaptool can't map one either.
        return Err(Report::new(Error::UnsupportedFeature(
            input_raw.to_path_buf(),
            "an empty image".into(),
        )));
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());

    let mut blocks = map::mapped_blocks(&raw, len).change_context_lazy(read_err)?;
    if blocks.is_empty() {
        // Nothing but holes. bmaptool would write an empty block map, which
        // bmap-parser (flash's, this crate's) can't read: map the first
        // block instead, zeros written over whatever the disk held there.
        blocks.push((0, 0));
    }
    let mut ranges = Vec::with_capacity(blocks.len());
    for (first, last) in blocks {
        let checksum = checksum(&mut raw, first, last, len).change_context_lazy(read_err)?;
        ranges.push(bmap::Range {
            first,
            last,
            checksum,
        });
    }
    let xml = bmap::to_xml(len, &ranges);

    let mut out = BufWriter::new(File::create(output).change_context_lazy(write_err)?);
    let bmap_header =
        header(&format!("{name}.bmap"), xml.len() as u64, mtime).change_context_lazy(write_err)?;
    // Checked before compressing anything, written once the size is known.
    let image_name = format!("{name}.zst");
    header(&image_name, 0, mtime).change_context_lazy(write_err)?;
    out.write_all(bmap_header.as_bytes())
        .and_then(|()| out.write_all(xml.as_bytes()))
        .and_then(|()| pad(&mut out, xml.len() as u64))
        .and_then(|()| out.write_all(&[0; TAR_BLOCK as usize]))
        .change_context_lazy(write_err)?;
    let image_header_at = TAR_BLOCK + padded(xml.len() as u64);

    raw.seek(SeekFrom::Start(0)).change_context_lazy(read_err)?;
    let mut counted = Counted::new(&mut out);
    compress(&mut BufReader::new(raw), len, &mut counted).change_context_lazy(write_err)?;
    let size = counted.written;

    // The end of the archive (two zero blocks) and GNU tar's padding to a
    // whole record.
    let end = image_header_at + TAR_BLOCK + padded(size);
    let archive = (end + 2 * TAR_BLOCK).div_ceil(TAR_RECORD) * TAR_RECORD;
    pad(&mut out, size)
        .and_then(|()| io::copy(&mut io::repeat(0).take(archive - end), &mut out).map(|_| ()))
        .change_context_lazy(write_err)?;
    let mut file = out
        .into_inner()
        .map_err(|e| e.into_error())
        .change_context_lazy(write_err)?;
    let image_header = header(&image_name, size, mtime).change_context_lazy(write_err)?;
    file.seek(SeekFrom::Start(image_header_at))
        .and_then(|_| file.write_all(image_header.as_bytes()))
        .change_context_lazy(write_err)
}

/// `output`'s file name without its `.bmaptar`.
fn member_stem(output: &Path) -> Result<String> {
    let invalid = || {
        Report::new(Error::UnsupportedFeature(
            output.to_path_buf(),
            "a name that doesn't end in .bmaptar".into(),
        ))
    };
    let name = output
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(invalid)?;
    let cut = name
        .len()
        .checked_sub(".bmaptar".len())
        .ok_or_else(invalid)?;
    if cut == 0 || !name.is_char_boundary(cut) || !name[cut..].eq_ignore_ascii_case(".bmaptar") {
        return Err(invalid());
    }
    Ok(name[..cut].to_string())
}

/// A member's header, as `tar -c --owner=0 --group=0 --numeric-owner`
/// writes a regular file's: GNU format, root's, no user or group name.
fn header(name: &str, size: u64, mtime: u64) -> io::Result<tar::Header> {
    let mut header = tar::Header::new_gnu();
    // Too long for the header's own field: GNU tar would need an extra
    // long-name member, which this doesn't write.
    header.set_path(name)?;
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(size);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(mtime);
    header.set_cksum();
    Ok(header)
}

/// `size` rounded up to whole tar blocks.
fn padded(size: u64) -> u64 {
    size.div_ceil(TAR_BLOCK) * TAR_BLOCK
}

/// The zeros after a `size`-byte member, up to its last block's end.
fn pad(out: &mut impl Write, size: u64) -> io::Result<()> {
    let zeros = [0; TAR_BLOCK as usize];
    out.write_all(&zeros[..(padded(size) - size) as usize])
}

/// The SHA-256 of blocks `first..=last` of `raw` (`len` bytes), the last
/// one cut at the image's end, as bmaptool hashes a range.
fn checksum(raw: &mut File, first: u64, last: u64, len: u64) -> io::Result<[u8; 32]> {
    let block = BLOCK_SIZE as u64;
    let start = first * block;
    let end = ((last + 1) * block).min(len);
    raw.seek(SeekFrom::Start(start))?;
    let mut hasher = Sha256::new();
    let mut range = raw.take(end - start);
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = range.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    if range.limit() != 0 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    Ok(hasher.finalize().into())
}

/// `input` (`len` bytes) as one zstd frame into `output`, the way
/// `zstd -3 --threads=N` writes it: on every core, with the content size
/// and checksum in the frame.
fn compress(input: &mut impl Read, len: u64, output: &mut impl Write) -> io::Result<()> {
    let mut encoder = zstd::stream::write::Encoder::new(output, LEVEL)?;
    let threads = thread::available_parallelism().map_or(1, |n| n.get());
    encoder.multithread(threads as u32)?;
    encoder.include_checksum(true)?;
    encoder.set_pledged_src_size(Some(len))?;
    io::copy(input, &mut encoder)?;
    encoder.finish()?;
    Ok(())
}

/// A writer that counts what goes through it.
struct Counted<W> {
    inner: W,
    written: u64,
}

impl<W> Counted<W> {
    fn new(inner: W) -> Self {
        Self { inner, written: 0 }
    }
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_are_named_after_the_bundle() {
        assert_eq!(
            member_stem(Path::new("out/laplaylist-image-f3apl.wic.bmaptar")).unwrap(),
            "laplaylist-image-f3apl.wic"
        );
        assert_eq!(member_stem(Path::new("disk.BMAPTAR")).unwrap(), "disk");
        assert!(member_stem(Path::new(".bmaptar")).is_err());
        assert!(member_stem(Path::new("disk.tar")).is_err());
    }
}
