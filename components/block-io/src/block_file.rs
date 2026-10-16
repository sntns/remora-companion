use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

use crate::{len, sync, SectorIo};

/// A file, or a disk device given in its place, read and written like a
/// file: a device's end is its capacity (where Linux's seek would land,
/// and macOS's and Windows' don't), and a Windows disk is accessed in
/// whole sectors only, as it accepts nothing else.
pub struct BlockFile {
    io: Io,
    /// A device's capacity, for seeks from its end; `None` for a file,
    /// whose end moves as it's written.
    end: Option<u64>,
}

enum Io {
    File(File),
    #[cfg_attr(not(windows), allow(dead_code))]
    Sectors(SectorIo<File>),
}

impl BlockFile {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::with(path, OpenOptions::new().read(true))
    }

    pub fn open_rw(path: &Path) -> io::Result<Self> {
        Self::with(path, OpenOptions::new().read(true).write(true))
    }

    fn with(path: &Path, options: &OpenOptions) -> io::Result<Self> {
        let file = options.open(path)?;
        if !is_device(path, &file)? {
            return Ok(Self {
                io: Io::File(file),
                end: None,
            });
        }
        let end = Some(len(&file)?);
        #[cfg(windows)]
        let io = {
            let sector = crate::windows::sector_size(&file).unwrap_or(512);
            Io::Sectors(SectorIo::new(file, sector))
        };
        #[cfg(not(windows))]
        let io = Io::File(file);
        Ok(Self { io, end })
    }

    /// Its length in bytes: a device's capacity.
    pub fn len(&self) -> io::Result<u64> {
        match self.end {
            Some(end) => Ok(end),
            None => len(self.get_ref()),
        }
    }

    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// What was written, on the disk.
    pub fn sync_all(&mut self) -> io::Result<()> {
        self.flush()?;
        sync(self.get_ref())
    }

    /// What was written, on the disk (its data, if not its metadata). A
    /// device has no metadata of its own: synced whole.
    pub fn sync_data(&mut self) -> io::Result<()> {
        self.flush()?;
        match self.end {
            Some(_) => sync(self.get_ref()),
            None => self.get_ref().sync_data(),
        }
    }

    /// The file (or device) underneath, for what only it can be asked (a
    /// disk's ioctls).
    pub fn get_ref(&self) -> &File {
        match &self.io {
            Io::File(file) => file,
            Io::Sectors(io) => io.get_ref(),
        }
    }
}

/// A disk device rather than a file: a block (or, for macOS's raw nodes,
/// character) device on Unix, a device-namespace path on Windows.
fn is_device(path: &Path, file: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let _ = path;
        let file_type = file.metadata()?.file_type();
        Ok(file_type.is_block_device() || file_type.is_char_device())
    }
    #[cfg(windows)]
    {
        let _ = file;
        Ok(crate::windows::is_device_path(path))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, file);
        Ok(false)
    }
}

impl Read for BlockFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.io {
            Io::File(file) => file.read(buf),
            Io::Sectors(io) => io.read(buf),
        }
    }
}

impl Write for BlockFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.io {
            Io::File(file) => file.write(buf),
            Io::Sectors(io) => io.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.io {
            Io::File(file) => file.flush(),
            Io::Sectors(io) => io.flush(),
        }
    }
}

impl Seek for BlockFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let pos = match (pos, self.end) {
            (SeekFrom::End(delta), Some(end)) => {
                SeekFrom::Start(end.checked_add_signed(delta).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "seek before the start")
                })?)
            }
            (pos, _) => pos,
        };
        match &mut self.io {
            Io::File(file) => file.seek(pos),
            Io::Sectors(io) => io.seek(pos),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_reads_writes_and_seeks_like_one() {
        let path =
            std::env::temp_dir().join(format!("remora-block-io-file-{}", std::process::id()));
        std::fs::write(&path, (0..=255).collect::<Vec<u8>>()).unwrap();
        let mut file = BlockFile::open_rw(&path).unwrap();
        assert_eq!(file.len().unwrap(), 256);
        assert_eq!(file.seek(SeekFrom::End(-1)).unwrap(), 255);
        let mut byte = [0];
        file.read_exact(&mut byte).unwrap();
        assert_eq!(byte, [255]);
        // A file's end moves as it grows.
        file.write_all(&[1, 2]).unwrap();
        assert_eq!(file.len().unwrap(), 258);
        assert_eq!(file.seek(SeekFrom::End(0)).unwrap(), 258);
        file.sync_all().unwrap();
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    /// A disk device, if this machine has one this user can read.
    #[test]
    fn a_disk_device_ends_at_its_capacity() {
        let candidates = [
            "/dev/sda",
            "/dev/nvme0n1",
            "/dev/vda",
            "/dev/disk0",
            r"\\.\PhysicalDrive0",
        ];
        let Some(mut disk) = candidates
            .iter()
            .find_map(|p| BlockFile::open(Path::new(p)).ok())
        else {
            eprintln!("no readable disk device on this machine, skipping");
            return;
        };
        let len = disk.len().unwrap();
        assert!(len > 0);
        assert_eq!(disk.seek(SeekFrom::End(0)).unwrap(), len);
        // The first sector, through whatever access the disk takes.
        disk.seek(SeekFrom::Start(3)).unwrap();
        let mut buf = [0; 7];
        disk.read_exact(&mut buf).unwrap();
        // A device node refuses the filesystem's sync (macOS: `F_FULLFSYNC`,
        // `ENOTTY`); a device's own one goes through.
        disk.sync_all().unwrap();
        disk.sync_data().unwrap();
    }
}
