//! Reads and writes of any length at any offset, made of whole sectors
//! only: what a raw disk accepts (macOS's `/dev/rdiskN`, a Windows
//! `\\.\PhysicalDriveN`), while a copy writes whatever its decompressor
//! hands it and a filesystem reads a few bytes of a superblock. A write that
//! starts or ends inside a sector reads that sector first, so the bytes
//! around it are kept.

use std::io::{self, Read, Seek, SeekFrom, Write};

/// How much is gathered before it's written: big writes are what make a
/// raw node fast.
const CHUNK: usize = 4 << 20;

pub struct SectorIo<F: Read + Write + Seek> {
    inner: F,
    sector: usize,
    /// Bytes not written yet, from `start` (a sector boundary) to `pos`.
    pending: Vec<u8>,
    start: u64,
    /// Where the next write lands.
    pos: u64,
}

impl<F: Read + Write + Seek> SectorIo<F> {
    pub fn new(inner: F, sector: usize) -> Self {
        assert!(sector > 0, "a sector has a size");
        Self {
            inner,
            sector,
            pending: Vec::new(),
            start: 0,
            pos: 0,
        }
    }

    pub fn get_ref(&self) -> &F {
        &self.inner
    }

    /// The sector at `offset`, as the disk holds it; zeros past its end.
    fn read_sector(&mut self, offset: u64) -> io::Result<Vec<u8>> {
        let mut sector = vec![0; self.sector];
        self.inner.seek(SeekFrom::Start(offset))?;
        let mut filled = 0;
        while filled < sector.len() {
            match self.inner.read(&mut sector[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(sector)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.inner.seek(SeekFrom::Start(offset))?;
        self.inner.write_all(bytes)
    }

    /// Writes the whole sectors gathered so far, keeping a partial last
    /// one for what comes next.
    fn write_whole_sectors(&mut self) -> io::Result<()> {
        let whole = self.pending.len() - self.pending.len() % self.sector;
        if whole > 0 {
            let pending = std::mem::take(&mut self.pending);
            self.write_at(self.start, &pending[..whole])?;
            self.pending = pending;
            self.pending.drain(..whole);
            self.start += whole as u64;
        }
        Ok(())
    }

    /// Writes everything gathered, its partial last sector completed with
    /// what the disk already holds after it.
    fn write_pending(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let tail = self.pending.len() % self.sector;
        if tail > 0 {
            let last = self.start + (self.pending.len() - tail) as u64;
            let existing = self.read_sector(last)?;
            self.pending.extend_from_slice(&existing[tail..]);
        }
        let pending = std::mem::take(&mut self.pending);
        self.write_at(self.start, &pending)?;
        self.start += pending.len() as u64;
        Ok(())
    }
}

impl<F: Read + Write + Seek> Read for SectorIo<F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // What's gathered is on the disk before anything is read back.
        self.write_pending()?;
        let head = (self.pos % self.sector as u64) as usize;
        // Already aligned: whole sectors come straight in, uncopied.
        if head == 0 && buf.len() >= self.sector {
            let whole = buf.len() - buf.len() % self.sector;
            self.inner.seek(SeekFrom::Start(self.pos))?;
            let n = self.inner.read(&mut buf[..whole])?;
            self.pos += n as u64;
            return Ok(n);
        }
        // The sectors around what's asked for (at most a chunk of them),
        // of which only the asked-for part is handed back.
        let span = (head + buf.len()).min(CHUNK).div_ceil(self.sector) * self.sector;
        let mut sectors = vec![0; span];
        self.inner.seek(SeekFrom::Start(self.pos - head as u64))?;
        let mut got = 0;
        while got < span {
            match self.inner.read(&mut sectors[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        let n = got.saturating_sub(head).min(buf.len());
        buf[..n].copy_from_slice(&sectors[head..head + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<F: Read + Write + Seek> Write for SectorIo<F> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        if self.pending.is_empty() {
            let head = (self.pos % self.sector as u64) as usize;
            // Already aligned: whole sectors go straight through, uncopied.
            if head == 0 && data.len() >= self.sector {
                let whole = data.len() - data.len() % self.sector;
                self.write_at(self.pos, &data[..whole])?;
                self.pos += whole as u64;
                return Ok(whole);
            }
            self.start = self.pos - head as u64;
            if head > 0 {
                self.pending = self.read_sector(self.start)?;
                self.pending.truncate(head);
            }
        }
        self.pending.extend_from_slice(data);
        self.pos += data.len() as u64;
        if self.pending.len() >= CHUNK {
            self.write_whole_sectors()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.write_pending()?;
        self.inner.flush()
    }
}

impl<F: Read + Write + Seek> Seek for SectorIo<F> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "seek before the disk's start")
            })?,
            SeekFrom::End(_) => {
                self.write_pending()?;
                self.pos = self.inner.seek(to)?;
                return Ok(self.pos);
            }
        };
        // The copy seeks by 0 between adjacent ranges: no reason to cut a
        // write short for it.
        if target != self.pos {
            self.write_pending()?;
            self.pos = target;
        }
        Ok(target)
    }
}

impl<F: Read + Write + Seek> Drop for SectorIo<F> {
    fn drop(&mut self) {
        let _ = self.write_pending();
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const SECTOR: usize = 512;

    /// A disk that, like a raw node, refuses any read or write not made of
    /// whole sectors at a sector boundary.
    struct RawDisk<'a>(Cursor<&'a mut Vec<u8>>);

    impl Read for RawDisk<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            assert_eq!(
                self.0.position() % SECTOR as u64,
                0,
                "unaligned read offset"
            );
            assert_eq!(buf.len() % SECTOR, 0, "unaligned read length");
            self.0.read(buf)
        }
    }

    impl Write for RawDisk<'_> {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            assert_eq!(self.0.position() % SECTOR as u64, 0, "unaligned offset");
            assert_eq!(buf.len() % SECTOR, 0, "unaligned length");
            self.0.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for RawDisk<'_> {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.0.seek(pos)
        }
    }

    /// What's on the disk before a test writes to it: every byte its own.
    fn existing(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 % 251) as u8).collect()
    }

    /// Runs `ops` (an optional seek, then a write of `len` bytes of `fill`)
    /// on both a plain cursor and a `SectorIo` over a raw disk; their
    /// bytes must end up the same.
    fn same_as_plain(ops: &[(Option<u64>, usize, u8)]) {
        let len = 64 * SECTOR;
        let mut plain = Cursor::new(existing(len));
        let mut disk = existing(len);
        let mut writer = SectorIo::new(RawDisk(Cursor::new(&mut disk)), SECTOR);
        for &(seek, n, fill) in ops {
            if let Some(offset) = seek {
                plain.seek(SeekFrom::Start(offset)).unwrap();
                writer.seek(SeekFrom::Start(offset)).unwrap();
            }
            let data = vec![fill; n];
            plain.write_all(&data).unwrap();
            writer.write_all(&data).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);
        assert_eq!(&disk, plain.get_ref());
    }

    #[test]
    fn small_unaligned_writes_keep_the_bytes_around_them() {
        same_as_plain(&[(Some(100), 3, 0xAA), (None, 1000, 0xBB), (None, 7, 0xCC)]);
    }

    #[test]
    fn a_write_inside_one_sector_keeps_both_its_sides() {
        same_as_plain(&[(Some(SECTOR as u64 + 10), 20, 0xAA)]);
    }

    #[test]
    fn aligned_writes_go_straight_through() {
        same_as_plain(&[(Some(0), 4 * SECTOR, 0xAA), (None, 2 * SECTOR + 1, 0xBB)]);
    }

    #[test]
    fn seeks_between_ranges_write_what_was_gathered() {
        same_as_plain(&[
            (Some(0), 700, 0xAA),
            (Some(10 * SECTOR as u64 + 3), 5000, 0xBB),
            (Some(30 * SECTOR as u64), SECTOR, 0xCC),
            (Some(5), 2, 0xDD),
        ]);
    }

    #[test]
    fn more_than_a_chunk_is_written_as_it_comes() {
        let len = 2 * CHUNK + 4 * SECTOR;
        let mut disk = vec![0; len];
        let mut writer = SectorIo::new(RawDisk(Cursor::new(&mut disk)), SECTOR);
        writer.seek(SeekFrom::Start(1)).unwrap();
        for _ in 0..CHUNK / 1000 + 2 {
            writer.write_all(&[0xAA; 1000]).unwrap();
        }
        assert!(writer.pending.len() < SECTOR + 1000);
    }

    #[test]
    fn a_seek_to_where_it_is_doesnt_cut_a_write() {
        let mut disk = existing(8 * SECTOR);
        let mut writer = SectorIo::new(RawDisk(Cursor::new(&mut disk)), SECTOR);
        writer.write_all(&[1; 10]).unwrap();
        // The copy's `seek_forward(0)` between adjacent ranges.
        writer.stream_position().unwrap();
        writer.write_all(&[2; 10]).unwrap();
        assert_eq!(writer.pending.len(), 20);
    }

    #[test]
    fn unaligned_reads_hand_back_just_what_was_asked() {
        let mut disk = existing(64 * SECTOR);
        let expected = disk.clone();
        let mut io = SectorIo::new(RawDisk(Cursor::new(&mut disk)), SECTOR);
        for (offset, len) in [
            (0, 3),
            (100, 1000),
            (SECTOR + 7, 2 * SECTOR),
            (0, 4 * SECTOR),
        ] {
            io.seek(SeekFrom::Start(offset as u64)).unwrap();
            let mut buf = vec![0; len];
            io.read_exact(&mut buf).unwrap();
            assert_eq!(buf, expected[offset..offset + len], "at {offset}+{len}");
        }
    }

    #[test]
    fn a_read_past_the_end_is_short() {
        let mut disk = existing(2 * SECTOR);
        let mut io = SectorIo::new(RawDisk(Cursor::new(&mut disk)), SECTOR);
        io.seek(SeekFrom::Start(2 * SECTOR as u64 - 4)).unwrap();
        let mut buf = [0; 10];
        assert_eq!(io.read(&mut buf).unwrap(), 4);
        assert_eq!(io.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn a_read_sees_what_was_just_written() {
        let mut disk = existing(8 * SECTOR);
        let mut io = SectorIo::new(RawDisk(Cursor::new(&mut disk)), SECTOR);
        io.seek(SeekFrom::Start(10)).unwrap();
        io.write_all(&[0xAB; 5]).unwrap();
        io.seek(SeekFrom::Start(8)).unwrap();
        let mut buf = [0; 9];
        io.read_exact(&mut buf).unwrap();
        let mut expected = existing(8 * SECTOR)[8..17].to_vec();
        expected[2..7].fill(0xAB);
        assert_eq!(buf, expected[..]);
    }

    #[test]
    fn dropping_it_writes_what_was_gathered() {
        let mut disk = existing(4 * SECTOR);
        let mut writer = SectorIo::new(RawDisk(Cursor::new(&mut disk)), SECTOR);
        writer.seek(SeekFrom::Start(3)).unwrap();
        writer.write_all(&[0xEE; 3]).unwrap();
        drop(writer);
        let mut expected = existing(4 * SECTOR);
        expected[3..6].fill(0xEE);
        assert_eq!(disk, expected);
    }
}
