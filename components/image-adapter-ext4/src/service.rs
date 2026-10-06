use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
};

use error_stack::{Report, ResultExt};
use fs_ext4::block_io::BlockDevice;
use fs_ext4::Filesystem;
use remora_image::adapter::{
    ext4::{Error, Ext4Adapter, Result},
    partition_fs::{self, PartitionFilesystem},
};

/// A `BlockDevice` that's really just `[offset, offset + size)` of a larger
/// file — a wic image's `data` partition, typically — so `am-fs-ext4` can
/// mount that range directly without us copying the partition out to its
/// own temporary file first.
struct WindowedFileDevice {
    file: Mutex<File>,
    offset: u64,
    size: u64,
}

impl WindowedFileDevice {
    fn open_rw(path: &Path, offset: u64, size: u64) -> std::result::Result<Self, Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| Error::Io(path.to_path_buf(), e))?;
        Ok(Self {
            file: Mutex::new(file),
            offset,
            size,
        })
    }

    fn check_bounds(&self, offset: u64, len: usize) -> fs_ext4::Result<()> {
        if offset.saturating_add(len as u64) > self.size {
            return Err(fs_ext4::Error::Corrupt("access outside partition window"));
        }
        Ok(())
    }
}

impl BlockDevice for WindowedFileDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> fs_ext4::Result<()> {
        self.check_bounds(offset, buf.len())?;
        let mut f = self.file.lock().unwrap();
        f.seek(SeekFrom::Start(self.offset + offset))?;
        f.read_exact(buf)?;
        Ok(())
    }

    fn size_bytes(&self) -> u64 {
        self.size
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> fs_ext4::Result<()> {
        self.check_bounds(offset, buf.len())?;
        let mut f = self.file.lock().unwrap();
        f.seek(SeekFrom::Start(self.offset + offset))?;
        f.write_all(buf)?;
        Ok(())
    }

    fn flush(&self) -> fs_ext4::Result<()> {
        let mut f = self.file.lock().unwrap();
        f.flush()?;
        f.sync_data()?;
        Ok(())
    }

    fn is_writable(&self) -> bool {
        true
    }
}

// Standard ext2/3/4 on-disk superblock layout (e2fsprogs' ext2fs.h /
// linux/fs/ext4/ext4.h) — the superblock always starts 1024 bytes into the
// filesystem, regardless of block size.
const EXT4_SUPERBLOCK_OFFSET: u64 = 1024;
const EXT4_MAGIC_OFFSET: usize = 0x38;
const EXT4_FEATURE_INCOMPAT_OFFSET: usize = 0x60;
const EXT4_MAGIC: u16 = 0xEF53;
const EXT4_FEATURE_INCOMPAT_RECOVER: u32 = 0x0004;

/// Read just enough of the superblock to check `needs_recovery`, without
/// going through `am-fs-ext4` at all — see `Error::NeedsJournalRecovery`.
fn refuse_if_needs_journal_recovery(
    image: &Path,
    partition_offset: u64,
) -> std::result::Result<(), Error> {
    let mut file = File::open(image).map_err(|e| Error::Io(image.to_path_buf(), e))?;
    let mut buf = [0u8; 100];
    file.seek(SeekFrom::Start(partition_offset + EXT4_SUPERBLOCK_OFFSET))
        .and_then(|_| file.read_exact(&mut buf))
        .map_err(|e| Error::Io(image.to_path_buf(), e))?;

    let magic = u16::from_le_bytes([buf[EXT4_MAGIC_OFFSET], buf[EXT4_MAGIC_OFFSET + 1]]);
    if magic != EXT4_MAGIC {
        // Not our concern here — let Filesystem::mount() report the real
        // "this isn't ext4" error with its own context.
        return Ok(());
    }

    let feature_incompat = u32::from_le_bytes([
        buf[EXT4_FEATURE_INCOMPAT_OFFSET],
        buf[EXT4_FEATURE_INCOMPAT_OFFSET + 1],
        buf[EXT4_FEATURE_INCOMPAT_OFFSET + 2],
        buf[EXT4_FEATURE_INCOMPAT_OFFSET + 3],
    ]);
    if feature_incompat & EXT4_FEATURE_INCOMPAT_RECOVER != 0 {
        return Err(Error::NeedsJournalRecovery {
            path: image.to_path_buf(),
        });
    }
    Ok(())
}

/// `image`'s size in bytes, a block device's included: its metadata length
/// is 0, only seeking to its end tells its capacity.
fn image_len(image: &Path) -> std::result::Result<u64, Error> {
    File::open(image)
        .and_then(|mut file| file.seek(SeekFrom::End(0)))
        .map_err(|e| Error::Io(image.to_path_buf(), e))
}

fn mount_window(image: &Path, offset: u64, size: u64) -> std::result::Result<Filesystem, Error> {
    let image_len = image_len(image)?;
    let window_end = offset.checked_add(size).filter(|&end| end <= image_len);
    if window_end.is_none() {
        return Err(Error::OutOfWindow {
            path: image.to_path_buf(),
            offset,
            len: size,
            window_size: image_len,
        });
    }

    refuse_if_needs_journal_recovery(image, offset)?;

    let dev = Arc::new(WindowedFileDevice::open_rw(image, offset, size)?);
    let fs = Filesystem::mount(dev)?;
    fs.replay_journal_if_dirty()?;
    Ok(fs)
}

/// Create `dest_path` (parent directory must already exist) inside the
/// ext4 filesystem occupying `[offset, offset + size)` of `image`, and
/// write `contents` to it. If `dest_path` already exists, it's unlinked and
/// recreated fresh rather than erroring — deliberately *not*
/// `apply_replace_file_content`, which frees the old blocks and allocates
/// the new run inside one journal transaction: for a multi-megabyte file
/// (e.g. an 8 MiB config.ext4 written into a real, journaled shared
/// partition) that transaction's descriptor block overflows
/// ("too many tags"). Unlink-then-create keeps each step within its own,
/// already-exercised transaction shape.
fn write_file(
    image: &Path,
    offset: u64,
    size: u64,
    dest_path: &str,
    contents: &[u8],
    mode: u16,
) -> std::result::Result<(), Error> {
    let fs = mount_window(image, offset, size)?;
    match fs.apply_create(dest_path, mode) {
        Ok(_) => {
            fs.apply_pwrite(dest_path, 0, contents)?;
        }
        Err(fs_ext4::Error::AlreadyExists) => {
            fs.apply_unlink(dest_path)?;
            fs.apply_create(dest_path, mode)?;
            fs.apply_pwrite(dest_path, 0, contents)?;
        }
        Err(e) => return Err(Error::Ext4(e)),
    }
    Ok(())
}

/// Read the whole content of `dest_path` inside the ext4 filesystem
/// occupying `[offset, offset + size)` of `image`.
fn read_file(
    image: &Path,
    offset: u64,
    size: u64,
    dest_path: &str,
) -> std::result::Result<Vec<u8>, Error> {
    let fs = mount_window(image, offset, size)?;
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(inode, _)| inode);
    let ino = fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, dest_path)?;
    let (inode, raw) = fs.read_inode_verified(ino)?;
    let bytes = if inode.has_inline_data() {
        fs_ext4::file_io::read_inline(&fs, &inode, &raw)?
    } else {
        fs_ext4::file_io::read_all(&fs, &inode)?
    };
    Ok(bytes)
}

/// Whether `dest_path` exists inside the ext4 filesystem occupying
/// `[offset, offset + size)` of `image`.
fn exists(
    image: &Path,
    offset: u64,
    size: u64,
    dest_path: &str,
) -> std::result::Result<bool, Error> {
    let fs = mount_window(image, offset, size)?;
    let mut reader = |ino: u32| fs.read_inode_verified(ino).map(|(inode, _)| inode);
    match fs_ext4::path::lookup(fs.dev.as_ref(), &fs.sb, &mut reader, dest_path) {
        Ok(_) => Ok(true),
        Err(fs_ext4::Error::NotFound) => Ok(false),
        Err(e) => Err(Error::Ext4(e)),
    }
}

/// Create directory `dest_path` (parent directory must already exist)
/// inside the ext4 filesystem occupying `[offset, offset + size)` of
/// `image`. Not recursive — same narrow-surface rule as `write_file`.
fn create_dir(
    image: &Path,
    offset: u64,
    size: u64,
    dest_path: &str,
    mode: u16,
) -> std::result::Result<(), Error> {
    let fs = mount_window(image, offset, size)?;
    fs.apply_mkdir(dest_path, mode)?;
    Ok(())
}

/// Format `image` (created/truncated to exactly `size_bytes`) as a fresh
/// ext4 filesystem — the "mkfs" half of `mkfs.ext4 -d CONFIG_DIR`, decomposed
/// since `am-fs-ext4`'s `format_filesystem` has no populate-at-format option
/// (the populate half is `create_dir`/`write_file` above).
fn format(
    image: &Path,
    size_bytes: u64,
    block_size: u32,
    label: Option<&str>,
) -> std::result::Result<(), Error> {
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(image)
        .map_err(|e| Error::Io(image.to_path_buf(), e))?;
    file.set_len(size_bytes)
        .map_err(|e| Error::Io(image.to_path_buf(), e))?;
    drop(file);

    let dev = WindowedFileDevice::open_rw(image, 0, size_bytes)?;
    fs_ext4::mkfs::format_filesystem(&dev, label, None, size_bytes, block_size)?;
    Ok(())
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Ext4AdapterImpl;

impl Ext4Adapter for Ext4AdapterImpl {
    fn write_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> Result<()> {
        write_file(image, offset, size, dest_path, contents, mode).map_err(Report::new)
    }

    fn read_file(&self, image: &Path, offset: u64, size: u64, dest_path: &str) -> Result<Vec<u8>> {
        read_file(image, offset, size, dest_path).map_err(Report::new)
    }

    fn exists(&self, image: &Path, offset: u64, size: u64, dest_path: &str) -> Result<bool> {
        exists(image, offset, size, dest_path).map_err(Report::new)
    }

    fn create_dir(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        mode: u16,
    ) -> Result<()> {
        create_dir(image, offset, size, dest_path, mode).map_err(Report::new)
    }

    fn format(
        &self,
        image: &Path,
        size_bytes: u64,
        block_size: u32,
        label: Option<&str>,
    ) -> Result<()> {
        format(image, size_bytes, block_size, label).map_err(Report::new)
    }
}

impl PartitionFilesystem for Ext4AdapterImpl {
    fn write_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        contents: &[u8],
        mode: u16,
    ) -> partition_fs::Result<()> {
        Ext4Adapter::write_file(self, image, offset, size, dest_path, contents, mode)
            .change_context(partition_fs::Error::Ext4)
    }

    fn read_file(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
    ) -> partition_fs::Result<Vec<u8>> {
        Ext4Adapter::read_file(self, image, offset, size, dest_path)
            .change_context(partition_fs::Error::Ext4)
    }

    fn exists(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
    ) -> partition_fs::Result<bool> {
        Ext4Adapter::exists(self, image, offset, size, dest_path)
            .change_context(partition_fs::Error::Ext4)
    }

    fn create_dir(
        &self,
        image: &Path,
        offset: u64,
        size: u64,
        dest_path: &str,
        mode: u16,
    ) -> partition_fs::Result<()> {
        Ext4Adapter::create_dir(self, image, offset, size, dest_path, mode)
            .change_context(partition_fs::Error::Ext4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal buffer with a valid ext4 magic at the real superblock
    /// offset, and `feature_incompat` set from the given flags.
    fn fake_filesystem_bytes(feature_incompat: u32) -> Vec<u8> {
        let mut buf = vec![0u8; 1024 + 128];
        buf[1024 + EXT4_MAGIC_OFFSET..1024 + EXT4_MAGIC_OFFSET + 2]
            .copy_from_slice(&EXT4_MAGIC.to_le_bytes());
        buf[1024 + EXT4_FEATURE_INCOMPAT_OFFSET..1024 + EXT4_FEATURE_INCOMPAT_OFFSET + 4]
            .copy_from_slice(&feature_incompat.to_le_bytes());
        buf
    }

    fn write_temp(bytes: &[u8]) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-etcher-ext4-fsext4-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::File::create(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        path
    }

    #[test]
    fn image_len_measures_a_block_device() {
        // Read-only, and only when this user may open one (group `disk`).
        let device = std::fs::read_dir("/sys/block").ok().and_then(|disks| {
            disks.flatten().find_map(|disk| {
                let sectors: u64 = std::fs::read_to_string(disk.path().join("size"))
                    .ok()?
                    .trim()
                    .parse()
                    .ok()?;
                let node = Path::new("/dev").join(disk.file_name());
                (sectors > 0 && File::open(&node).is_ok()).then_some((node, sectors * 512))
            })
        });
        let Some((node, size)) = device else {
            eprintln!("no readable block device on this machine, skipping");
            return;
        };
        assert_eq!(std::fs::metadata(&node).unwrap().len(), 0);
        assert_eq!(image_len(&node).unwrap(), size);
    }

    #[test]
    fn refuses_when_needs_recovery_is_set() {
        let path = write_temp(&fake_filesystem_bytes(
            EXT4_FEATURE_INCOMPAT_RECOVER | 0x0040,
        ));
        let err = refuse_if_needs_journal_recovery(&path, 0).unwrap_err();
        assert!(matches!(err, Error::NeedsJournalRecovery { .. }));
    }

    #[test]
    fn allows_a_clean_journal_through() {
        // extents (0x0040) set, RECOVER not set.
        let path = write_temp(&fake_filesystem_bytes(0x0040));
        assert!(refuse_if_needs_journal_recovery(&path, 0).is_ok());
    }

    #[test]
    fn defers_to_mount_when_magic_does_not_match() {
        let mut bytes = fake_filesystem_bytes(EXT4_FEATURE_INCOMPAT_RECOVER);
        bytes[1024 + EXT4_MAGIC_OFFSET] = 0x00;
        bytes[1024 + EXT4_MAGIC_OFFSET + 1] = 0x00;
        let path = write_temp(&bytes);
        assert!(refuse_if_needs_journal_recovery(&path, 0).is_ok());
    }

    #[test]
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "requires fsck.ext4, a Linux-only dev tool"
    )]
    fn format_then_mkdir_and_write_file_round_trips() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-etcher-ext4-fsext4-format-test-{}",
            std::process::id()
        ));

        format(&path, 8 * 1024 * 1024, 1024, Some("config")).unwrap();
        create_dir(&path, 0, 8 * 1024 * 1024, "/tzdata", 0o755).unwrap();
        write_file(
            &path,
            0,
            8 * 1024 * 1024,
            "/hello",
            b"hello from remora-etcher\n",
            0o644,
        )
        .unwrap();

        let status = std::process::Command::new("fsck.ext4")
            .args(["-n", "-f"])
            .arg(&path)
            .status()
            .expect("fsck.ext4 not available");
        assert!(status.success(), "fsck.ext4 reported the image as unclean");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_file_overwrites_and_read_file_and_exists_round_trip() {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-etcher-ext4-fsext4-overwrite-test-{}",
            std::process::id()
        ));

        format(&path, 8 * 1024 * 1024, 1024, Some("config")).unwrap();

        assert!(!exists(&path, 0, 8 * 1024 * 1024, "/timezone").unwrap());

        write_file(
            &path,
            0,
            8 * 1024 * 1024,
            "/timezone",
            b"Europe/Paris\n",
            0o644,
        )
        .unwrap();
        assert!(exists(&path, 0, 8 * 1024 * 1024, "/timezone").unwrap());
        assert_eq!(
            read_file(&path, 0, 8 * 1024 * 1024, "/timezone").unwrap(),
            b"Europe/Paris\n"
        );

        // Overwriting with shorter content must replace, not append.
        write_file(&path, 0, 8 * 1024 * 1024, "/timezone", b"UTC\n", 0o644).unwrap();
        assert_eq!(
            read_file(&path, 0, 8 * 1024 * 1024, "/timezone").unwrap(),
            b"UTC\n"
        );

        let _ = std::fs::remove_file(&path);
    }
}
