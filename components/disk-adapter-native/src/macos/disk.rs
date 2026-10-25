use std::{
    collections::HashSet,
    ffi::{CStr, CString},
    fs::{self, File, OpenOptions},
    io::{self, Seek, SeekFrom, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt},
        },
    },
    path::{Path, PathBuf},
};

use error_stack::{Report, ResultExt};
use remora_disk::{
    adapter::{DiskHold, DiskTarget, DiskWrite, Error, Result},
    model::DiskInfo,
};

use super::{
    arbitration::{self, MountBlocker},
    ffi::{self, Disk, Session},
};
use remora_block_io::SectorIo;

/// Mount points that host the running system: on macOS, the sealed system
/// volume at `/` and its siblings (Data, Preboot, VM, Update...) under
/// `/System/Volumes`, all in the boot disk's APFS container. An external
/// disk mounts under `/Volumes` instead.
const CRITICAL_MOUNT_POINTS: &[&str] = &["/", "/private/var/vm"];
const CRITICAL_MOUNT_PREFIX: &str = "/System/Volumes/";

/// `DKIOCGETBLOCKSIZE`: `_IOR('d', 24, uint32_t)`.
const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;

pub fn enumerate() -> Result<Vec<DiskInfo>> {
    let session = Session::new().ok_or_else(|| Report::new(Error::Io))?;
    let system = system_disks(&session);

    let mut disks = Vec::new();
    for name in whole_disk_names()? {
        let Some(disk) = session.disk(&name) else {
            continue;
        };
        // An APFS container's disk is a view of a partition: what's under
        // it is the disk to flash, and is listed itself.
        if disk.lineage().synthesized {
            continue;
        }
        disks.push(disk_info(&disk, &name, &system));
    }
    Ok(disks)
}

/// The disk `path` really opens, matched by its device number the way
/// Linux's is: `devname(3)` names the node the kernel has for it, whatever
/// the path is called. Both `/dev/diskN` and its raw `/dev/rdiskN` are
/// the whole disk `diskN`.
pub fn info(path: &Path) -> Result<DiskInfo> {
    let canonical =
        fs::canonicalize(path).change_context_lazy(|| Error::NotFound(path.to_path_buf()))?;
    let name = whole_disk_of(path, &canonical)?;
    let session = Session::new().ok_or_else(|| Report::new(Error::Io))?;
    let disk = session
        .disk(&name)
        .ok_or_else(|| Report::new(Error::NotFound(path.to_path_buf())))?;
    if disk.lineage().synthesized {
        return Err(Report::new(Error::NotWholeDisk(path.to_path_buf())));
    }
    let mut info = disk_info(&disk, &name, &system_disks(&session));
    // The node actually opened, as on Linux: the flash guard compares it.
    info.path = canonical;
    Ok(info)
}

/// Opens the raw node of the disk `disk` describes, its volumes unmounted
/// first and kept from mounting again (the copy rewrites their partition
/// table under them) until the target is dropped. The raw node
/// (`/dev/rdiskN`) is many times faster than the buffered one, but takes
/// whole sectors only: writes go through a [`SectorIo`].
pub fn open(disk: &DiskInfo) -> Result<DiskTarget> {
    let path = &disk.path;
    let canonical = fs::canonicalize(path).change_context_lazy(|| Error::Open(path.clone()))?;
    let name = whole_disk_of(path, &canonical)?;
    let raw = PathBuf::from(format!("/dev/r{name}"));

    // The node can't be opened before its volumes are unmounted (it's
    // busy).
    check_writable(&raw)?;

    let blocker = MountBlocker::start(&name).change_context_lazy(|| Error::Open(path.clone()))?;
    arbitration::unmount(&name).change_context_lazy(|| Error::Unmount(path.clone()))?;

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&raw)
        .change_context_lazy(|| Error::Open(raw.clone()))?;
    let sector = block_size(&file).unwrap_or(512);
    Ok(DiskTarget::new(RawDisk {
        writer: SectorIo::new(file, sector),
        blocker,
        name,
    }))
}

/// The whole physical disk behind `path` -- a disk, a partition, or a
/// volume of an APFS container on one -- its volumes unmounted and kept
/// from mounting again until the hold is dropped: macOS refuses to open a
/// disk for writing under a mounted volume, and remounts a card on its
/// own. `None` for an image file (or one not created yet).
pub fn hold(path: &Path) -> Result<Option<DiskHold>> {
    let Ok(canonical) = fs::canonicalize(path) else {
        return Ok(None);
    };
    let Some(name) = fs::metadata(&canonical)
        .ok()
        .and_then(|meta| disk_node_name(&meta))
    else {
        return Ok(None);
    };
    let session = Session::new().ok_or_else(|| Report::new(Error::Io))?;
    let lineage = session
        .disk(&name)
        .ok_or_else(|| Report::new(Error::NotFound(path.to_path_buf())))?
        .lineage();
    let whole = lineage
        .whole_disks
        .into_iter()
        .find(|whole| {
            session
                .disk(whole)
                .is_some_and(|disk| !disk.lineage().synthesized)
        })
        .ok_or_else(|| Report::new(Error::NotFound(path.to_path_buf())))?;
    if system_disks(&session).contains(&whole) {
        return Err(Report::new(Error::SystemDisk(path.to_path_buf())));
    }
    check_writable(&canonical)?;

    let blocker =
        MountBlocker::start(&whole).change_context_lazy(|| Error::Unmount(path.to_path_buf()))?;
    arbitration::unmount(&whole).change_context_lazy(|| Error::Unmount(path.to_path_buf()))?;
    Ok(Some(DiskHold::new(blocker)))
}

/// A raw disk node open for writing. Fields drop in order: the writer
/// (writing what it gathered, then closing the node) before the blocker.
struct RawDisk {
    writer: SectorIo<File>,
    blocker: MountBlocker,
    /// The whole disk's BSD name, `disk4`.
    name: String,
}

impl Write for RawDisk {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl Seek for RawDisk {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.writer.seek(pos)
    }
}

impl DiskWrite for RawDisk {
    /// A raw node has no buffer cache to flush: what's left is ours, and
    /// the drive's own cache (see `remora_block_io::sync`; `sync_all`
    /// would be refused).
    fn sync(&mut self) -> io::Result<()> {
        self.writer.flush()?;
        remora_block_io::sync(self.writer.get_ref())
    }

    /// Closes the node, then ejects the disk while mounts are still
    /// refused: closing it has macOS read the new partition table, and
    /// mount its volumes if let -- unplugging it then would be an unsafe
    /// removal. Anything mounted all the same is unmounted first.
    fn eject(self: Box<Self>) -> io::Result<bool> {
        let RawDisk {
            mut writer,
            blocker,
            name,
        } = *self;
        writer.flush()?;
        drop(writer);
        arbitration::unmount(&name).map_err(io::Error::other)?;
        arbitration::eject(&name).map_err(io::Error::other)?;
        drop(blocker);
        Ok(true)
    }
}

/// The disk's logical sector size, which every raw write must be a
/// multiple of.
fn block_size(file: &File) -> Option<usize> {
    let mut size: u32 = 0;
    // SAFETY: a live descriptor and a u32 for the ioctl to fill.
    let status = unsafe { libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKSIZE, &mut size as *mut u32) };
    (status == 0 && size > 0).then_some(size as usize)
}

/// The BSD name (`disk4`) of the whole disk the node at `canonical` is,
/// refusing a partition (`disk4s1`) and anything not a disk node.
fn whole_disk_of(path: &Path, canonical: &Path) -> Result<String> {
    let meta =
        fs::metadata(canonical).change_context_lazy(|| Error::NotFound(path.to_path_buf()))?;
    let name = disk_node_name(&meta)
        .ok_or_else(|| Report::new(Error::NotBlockDevice(path.to_path_buf())))?;
    if !is_whole_disk_name(&name) {
        return Err(Report::new(Error::NotWholeDisk(path.to_path_buf())));
    }
    Ok(name)
}

/// The BSD name (`disk4`, `disk4s1`) of the disk node `meta` describes,
/// matched by its device number (`devname(3)`), whatever its path is
/// called; `None` for a file, or a device that isn't a disk.
fn disk_node_name(meta: &fs::Metadata) -> Option<String> {
    let file_type = meta.file_type();
    let mode = if file_type.is_block_device() {
        libc::S_IFBLK
    } else if file_type.is_char_device() {
        libc::S_IFCHR
    } else {
        return None;
    };
    // SAFETY: devname returns a static buffer (or null), read at once.
    let node = unsafe {
        let name = libc::devname(meta.rdev() as libc::dev_t, mode);
        (!name.is_null()).then(|| CStr::from_ptr(name).to_string_lossy().into_owned())
    }?;
    bsd_disk_name(&node, mode == libc::S_IFCHR)
}

/// Refuses a node this user can't write, before any of its volumes is
/// unmounted for nothing: [`Error::PermissionDenied`] when only root may
/// (macOS lets no one else write a disk's node).
fn check_writable(node: &Path) -> Result<()> {
    let c_node = CString::new(node.as_os_str().as_bytes())
        .change_context_lazy(|| Error::Open(node.into()))?;
    // SAFETY: a NUL-terminated path.
    if unsafe { libc::access(c_node.as_ptr(), libc::R_OK | libc::W_OK) } != 0 {
        let error = io::Error::last_os_error();
        let context = if error.kind() == io::ErrorKind::PermissionDenied {
            Error::PermissionDenied(node.into())
        } else {
            Error::Open(node.into())
        };
        return Err(Report::new(error).change_context(context));
    }
    Ok(())
}

/// See [`DiskAdapter::check_writable`](remora_disk::adapter::DiskAdapter::check_writable):
/// the raw node `open` writes.
pub fn check_disk_writable(disk: &DiskInfo) -> Result<()> {
    let path = &disk.path;
    let canonical = fs::canonicalize(path).change_context_lazy(|| Error::Open(path.clone()))?;
    let name = whole_disk_of(path, &canonical)?;
    check_writable(&PathBuf::from(format!("/dev/r{name}")))
}

/// The BSD disk name a `/dev` node is called: `disk4s1` for `disk4s1`, or
/// for `rdisk4s1` when the node is a character (raw) one. Anything else
/// (`/dev/null` is a character device too) isn't a disk.
fn bsd_disk_name(node: &str, raw: bool) -> Option<String> {
    let name = if raw { node.strip_prefix('r')? } else { node };
    name.starts_with("disk").then(|| name.to_owned())
}

/// `diskN`, not a partition (`diskNsM`) of it.
fn is_whole_disk_name(name: &str) -> bool {
    name.strip_prefix("disk")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Every whole disk with a node in `/dev`, in disk number order.
pub(super) fn whole_disk_names() -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir("/dev").change_context(Error::Io)? {
        let name = entry
            .change_context(Error::Io)?
            .file_name()
            .to_string_lossy()
            .into_owned();
        if is_whole_disk_name(&name) {
            names.push(name);
        }
    }
    names.sort_by_key(|name| name[4..].parse::<u64>().unwrap_or(u64::MAX));
    Ok(names)
}

fn disk_info(disk: &Disk, name: &str, system: &HashSet<String>) -> DiskInfo {
    let description = disk.description();
    // SAFETY: values are borrowed from the description, which outlives
    // every read of them.
    let (size_bytes, removable, ejectable, model) = match &description {
        Some(d) => unsafe {
            (
                ffi::number(ffi::dict_get(d, ffi::kDADiskDescriptionMediaSizeKey)),
                ffi::boolean(ffi::dict_get(d, ffi::kDADiskDescriptionMediaRemovableKey)),
                ffi::boolean(ffi::dict_get(d, ffi::kDADiskDescriptionMediaEjectableKey)),
                ffi::string(ffi::dict_get(d, ffi::kDADiskDescriptionDeviceModelKey))
                    .or_else(|| ffi::string(ffi::dict_get(d, ffi::kDADiskDescriptionMediaNameKey))),
            )
        },
        None => (None, None, None, None),
    };
    DiskInfo {
        path: PathBuf::from("/dev").join(name),
        size_bytes: size_bytes.unwrap_or(0),
        model: model
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty()),
        // An SD card reader's media is removable; a USB stick's usually
        // isn't, but it is ejectable -- as is anything external.
        is_removable: removable.unwrap_or(false) || ejectable.unwrap_or(false),
        is_system_disk: system.contains(name),
    }
}

/// The whole physical disks (and APFS containers) behind a
/// [`CRITICAL_MOUNT_POINTS`] mount: `/` is a snapshot of a volume of a
/// container, which sits on a partition of the boot disk -- the disk that
/// must never be flashed.
fn system_disks(session: &Session) -> HashSet<String> {
    let mut disks = HashSet::new();
    for (from, on) in mounts() {
        let critical =
            CRITICAL_MOUNT_POINTS.contains(&on.as_str()) || on.starts_with(CRITICAL_MOUNT_PREFIX);
        if !critical {
            continue;
        }
        let Some(name) = from.strip_prefix("/dev/") else {
            continue;
        };
        if let Some(disk) = session.disk(name) {
            disks.extend(disk.lineage().whole_disks);
        }
    }
    disks
}

/// Every mounted filesystem, as `(device, mount point)`, per
/// `getmntinfo(3)`.
fn mounts() -> Vec<(String, String)> {
    let mut stats: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: getmntinfo points `stats` at `count` entries of its own
    // buffer, valid until the next call on this thread; read at once.
    unsafe {
        let count = libc::getmntinfo(&mut stats, libc::MNT_NOWAIT);
        if count <= 0 || stats.is_null() {
            return Vec::new();
        }
        std::slice::from_raw_parts(stats, count as usize)
            .iter()
            .map(|stat| {
                (
                    CStr::from_ptr(stat.f_mntfromname.as_ptr())
                        .to_string_lossy()
                        .into_owned(),
                    CStr::from_ptr(stat.f_mntonname.as_ptr())
                        .to_string_lossy()
                        .into_owned(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_disk_names_are_disk_and_a_number() {
        assert!(is_whole_disk_name("disk0"));
        assert!(is_whole_disk_name("disk12"));
        assert!(!is_whole_disk_name("disk"));
        assert!(!is_whole_disk_name("disk4s1"));
        assert!(!is_whole_disk_name("rdisk4"));
    }

    #[test]
    fn a_raw_node_names_its_disk() {
        assert_eq!(bsd_disk_name("rdisk4", true).as_deref(), Some("disk4"));
        assert_eq!(bsd_disk_name("disk4s1", false).as_deref(), Some("disk4s1"));
        assert_eq!(bsd_disk_name("null", true), None);
        assert_eq!(bsd_disk_name("rdisk4", false), None);
    }

    #[test]
    fn the_boot_disk_is_a_system_disk() {
        let disks = enumerate().unwrap();
        assert!(
            disks.iter().any(|d| d.is_system_disk),
            "no system disk among {disks:?}"
        );
    }

    #[test]
    fn info_resolves_the_raw_node_to_its_disk() {
        let disk = enumerate().unwrap().into_iter().next().expect("a disk");
        let name = disk
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let raw = PathBuf::from(format!("/dev/r{name}"));
        let info = info(&raw).unwrap();
        assert_eq!(info.path, raw);
        assert_eq!(info.size_bytes, disk.size_bytes);
        assert_eq!(info.is_system_disk, disk.is_system_disk);
    }

    #[test]
    fn hold_has_nothing_to_hold_for_an_image_file() {
        let image = std::env::temp_dir().join(format!("remora-hold-{}.img", std::process::id()));
        fs::write(&image, b"").unwrap();
        assert!(hold(&image).unwrap().is_none());
        fs::remove_file(&image).unwrap();
        assert!(hold(&image).unwrap().is_none());
        assert!(hold(Path::new("/dev/null")).unwrap().is_none());
    }

    #[test]
    fn hold_refuses_the_system_disk() {
        let system = enumerate()
            .unwrap()
            .into_iter()
            .find(|d| d.is_system_disk)
            .expect("a system disk");
        let err = hold(&system.path).unwrap_err();
        assert!(matches!(err.current_context(), Error::SystemDisk(_)));
    }

    #[test]
    fn info_refuses_a_node_that_is_not_a_disk() {
        let err = info(Path::new("/dev/null")).unwrap_err();
        assert!(matches!(err.current_context(), Error::NotBlockDevice(_)));
    }

    #[test]
    fn info_refuses_a_partition() {
        let partition = fs::read_dir("/dev")
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| {
                n.strip_prefix("disk")
                    .is_some_and(|rest| rest.contains('s'))
            })
            .expect("a partition");
        let err = info(&Path::new("/dev").join(partition)).unwrap_err();
        assert!(matches!(err.current_context(), Error::NotWholeDisk(_)));
    }

    #[test]
    fn info_refuses_an_apfs_container() {
        let Some(container) = whole_disk_names().unwrap().into_iter().find(|n| {
            Session::new()
                .and_then(|s| s.disk(n))
                .is_some_and(|d| d.lineage().synthesized)
        }) else {
            eprintln!("no APFS container on this machine, skipping");
            return;
        };
        let err = info(&Path::new("/dev").join(container)).unwrap_err();
        assert!(matches!(err.current_context(), Error::NotWholeDisk(_)));
    }
}
