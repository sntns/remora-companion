use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
};

use super::error::Error;

/// `Read`/`Write`/`Seek` over `[offset, offset + size)` of a larger file, so
/// `fatfs` can mount that range directly — the vfat analogue of
/// `ext4::WindowedFileDevice`, just against `fatfs`'s
/// `Read + Write + Seek` storage trait instead of `fs_ext4::BlockDevice`.
struct WindowedFile {
    file: File,
    offset: u64,
    size: u64,
    pos: u64,
}

impl WindowedFile {
    fn open_rw(path: &std::path::Path, offset: u64, size: u64) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| Error::Open {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            file,
            offset,
            size,
            pos: 0,
        })
    }
}

impl Read for WindowedFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.size.saturating_sub(self.pos);
        let len = remaining.min(buf.len() as u64) as usize;
        self.file.seek(SeekFrom::Start(self.offset + self.pos))?;
        let n = self.file.read(&mut buf[..len])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for WindowedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let remaining = self.size.saturating_sub(self.pos);
        if remaining == 0 && !buf.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "write would go past the end of the partition window",
            ));
        }
        let len = remaining.min(buf.len() as u64) as usize;
        self.file.seek(SeekFrom::Start(self.offset + self.pos))?;
        let n = self.file.write(&buf[..len])?;
        self.pos += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for WindowedFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::End(p) => self.size as i128 + p as i128,
            SeekFrom::Current(p) => self.pos as i128 + p as i128,
        };
        if new_pos < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the partition window",
            ));
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

fn mount_window(
    image: &std::path::Path,
    offset: u64,
    size: u64,
) -> Result<fatfs::FileSystem<fatfs::StdIoWrapper<WindowedFile>>, Error> {
    let image_len = std::fs::metadata(image)
        .map_err(|source| Error::Open {
            path: image.to_path_buf(),
            source,
        })?
        .len();
    let window_end = offset.checked_add(size).filter(|&end| end <= image_len);
    if window_end.is_none() {
        return Err(Error::OutOfWindow {
            path: image.to_path_buf(),
            offset,
            len: size,
            window_size: image_len,
        });
    }

    let windowed = WindowedFile::open_rw(image, offset, size)?;
    let fs = fatfs::FileSystem::new(windowed, fatfs::FsOptions::new()).map_err(Error::Fat)?;
    Ok(fs)
}

/// `/`-separated absolute path → the relative, leading-`/`-stripped form
/// `fatfs`'s `Dir::create_file`/`create_dir`/`open_dir` expect.
fn relative(dest_path: &str) -> &str {
    dest_path.trim_start_matches('/')
}

/// Create `dest_path` (parent directory must already exist) inside the vfat
/// filesystem occupying `[offset, offset + size)` of `image`, and write
/// `contents` to it. `mode` is accepted for signature parity with
/// `ext4::write_file` but has no effect — FAT has no unix permission
/// bits.
pub fn write_file(
    image: &std::path::Path,
    offset: u64,
    size: u64,
    dest_path: &str,
    contents: &[u8],
    _mode: u16,
) -> Result<(), Error> {
    let fs = mount_window(image, offset, size)?;
    let root = fs.root_dir();
    let mut file = root.create_file(relative(dest_path)).map_err(Error::Fat)?;
    file.truncate().map_err(Error::Fat)?;
    file.write_all(contents).map_err(|e| Error::Fat(e.into()))?;
    Ok(())
}

/// Create directory `dest_path` (parent directory must already exist)
/// inside the vfat filesystem occupying `[offset, offset + size)` of
/// `image`. `mode` is accepted for signature parity with
/// `ext4::create_dir` but has no effect.
pub fn create_dir(
    image: &std::path::Path,
    offset: u64,
    size: u64,
    dest_path: &str,
    _mode: u16,
) -> Result<(), Error> {
    let fs = mount_window(image, offset, size)?;
    let root = fs.root_dir();
    root.create_dir(relative(dest_path)).map_err(Error::Fat)?;
    Ok(())
}

/// Read the whole content of `dest_path` inside the vfat filesystem
/// occupying `[offset, offset + size)` of `image`.
pub fn read_file(
    image: &std::path::Path,
    offset: u64,
    size: u64,
    dest_path: &str,
) -> Result<Vec<u8>, Error> {
    let fs = mount_window(image, offset, size)?;
    let root = fs.root_dir();
    let mut file = root.open_file(relative(dest_path)).map_err(Error::Fat)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| Error::Fat(e.into()))?;
    Ok(buf)
}

/// Whether `dest_path` exists inside the vfat filesystem occupying
/// `[offset, offset + size)` of `image`.
pub fn exists(
    image: &std::path::Path,
    offset: u64,
    size: u64,
    dest_path: &str,
) -> Result<bool, Error> {
    let fs = mount_window(image, offset, size)?;
    let root = fs.root_dir();
    let result = match root.open_file(relative(dest_path)) {
        Ok(_) => Ok(true),
        Err(fatfs::Error::NotFound) => Ok(false),
        Err(e) => Err(Error::Fat(e)),
    };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::PathBuf, process::Command};

    fn temp_image(size_bytes: u64) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-etcher-fat-fatfs-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        File::create(&path).unwrap().set_len(size_bytes).unwrap();
        path
    }

    /// Dev-only round-trip against a real `mkfs.vfat`-formatted fixture,
    /// checked back with real `fsck.vfat` afterwards — same posture as the
    /// ext4 phase-3 matrix: never shelled out to by the shipped binary.
    #[test]
    fn create_dir_and_write_file_round_trip_on_a_real_mkfs_vfat_fixture() {
        let path = temp_image(16 * 1024 * 1024);
        let status = Command::new("mkfs.vfat")
            .arg(&path)
            .status()
            .expect("mkfs.vfat not available");
        assert!(status.success());

        create_dir(&path, 0, 16 * 1024 * 1024, "/remora", 0o755).unwrap();
        create_dir(&path, 0, 16 * 1024 * 1024, "/remora/slot-A", 0o755).unwrap();
        write_file(
            &path,
            0,
            16 * 1024 * 1024,
            "/remora/identity",
            b"squashfs-bytes-go-here",
            0o644,
        )
        .unwrap();
        write_file(
            &path,
            0,
            16 * 1024 * 1024,
            "/remora/slot-A/config",
            b"ext4-bytes-go-here",
            0o644,
        )
        .unwrap();

        // Overwriting an existing file must replace its content, not
        // append to or corrupt it.
        write_file(
            &path,
            0,
            16 * 1024 * 1024,
            "/remora/identity",
            b"shorter",
            0o644,
        )
        .unwrap();

        let status = Command::new("fsck.vfat")
            .args(["-n"])
            .arg(&path)
            .status()
            .expect("fsck.vfat not available");
        assert!(status.success(), "fsck.vfat reported the image as unclean");

        // Read back through fatfs itself too, not just fsck.
        let fs = mount_window(&path, 0, 16 * 1024 * 1024).unwrap();
        let root = fs.root_dir();
        let mut file = root.open_file("remora/identity").unwrap();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"shorter");

        let mut config = root.open_file("remora/slot-A/config").unwrap();
        let mut buf = Vec::new();
        config.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"ext4-bytes-go-here");
    }

    #[test]
    fn write_file_overwrites_and_read_file_and_exists_round_trip() {
        let path = temp_image(16 * 1024 * 1024);
        Command::new("mkfs.vfat")
            .arg(&path)
            .status()
            .expect("mkfs.vfat not available");

        assert!(!exists(&path, 0, 16 * 1024 * 1024, "/config").unwrap());

        write_file(&path, 0, 16 * 1024 * 1024, "/config", b"first", 0o644).unwrap();
        assert!(exists(&path, 0, 16 * 1024 * 1024, "/config").unwrap());
        assert_eq!(
            read_file(&path, 0, 16 * 1024 * 1024, "/config").unwrap(),
            b"first"
        );

        write_file(
            &path,
            0,
            16 * 1024 * 1024,
            "/config",
            b"second-value",
            0o644,
        )
        .unwrap();
        assert_eq!(
            read_file(&path, 0, 16 * 1024 * 1024, "/config").unwrap(),
            b"second-value"
        );
    }

    #[test]
    fn refuses_a_window_past_the_end_of_the_file() {
        let path = temp_image(1024);
        match mount_window(&path, 0, 2048) {
            Err(Error::OutOfWindow { .. }) => {}
            other => panic!("expected OutOfWindow, got {}", describe(other)),
        }
    }

    fn describe(
        result: Result<fatfs::FileSystem<fatfs::StdIoWrapper<WindowedFile>>, Error>,
    ) -> String {
        match result {
            Ok(_) => "Ok(..)".to_string(),
            Err(e) => format!("Err({e})"),
        }
    }
}
