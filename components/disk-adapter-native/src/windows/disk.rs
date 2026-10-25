use std::{
    collections::HashSet,
    io::{self, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use error_stack::Report;
use remora_block_io::BlockFile;
use remora_disk::{
    adapter::{DiskHold, DiskTarget, DiskWrite, Error, Result},
    model::DiskInfo,
};
use windows_sys::Win32::{
    Storage::FileSystem::{BusTypeMmc, BusTypeSd, BusTypeUsb},
    System::{
        Ioctl::{
            PropertyStandardQuery, StorageDeviceProperty, DISK_GEOMETRY_EX,
            IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_UPDATE_PROPERTIES,
            IOCTL_STORAGE_EJECT_MEDIA, IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_DEVICE_DESCRIPTOR,
            STORAGE_PROPERTY_QUERY,
        },
        SystemInformation::GetSystemWindowsDirectoryW,
    },
};

use super::{
    ioctl::{self, from_wide},
    volume::{self, VolumeLocks},
};

/// How many `\\.\PhysicalDriveN` are tried: numbers are reused as disks
/// come and go, so there may be gaps below the highest.
const MAX_DISKS: u32 = 64;

pub fn enumerate() -> Result<Vec<DiskInfo>> {
    let system = system_disks();
    Ok((0..MAX_DISKS)
        .filter_map(|number| disk_info(number, &system))
        .collect())
}

/// The disk `path` names: `\\.\PhysicalDriveN`, any case, also spelled
/// `\\?\`. A volume (`E:`, `\\.\E:`) is refused as not a whole disk, the
/// error naming the disk it's on.
pub fn info(path: &Path) -> Result<DiskInfo> {
    let number = match parse(path) {
        Some(Target::Disk(number)) => number,
        Some(Target::Volume(volume)) => {
            let report = Report::new(Error::NotWholeDisk(path.to_path_buf()));
            return Err(match disk_of_volume(&volume) {
                Some(number) => {
                    report.attach(format!("it is a volume of {}", disk_path(number).display()))
                }
                None => report,
            });
        }
        None => return Err(Report::new(Error::NotBlockDevice(path.to_path_buf()))),
    };
    disk_info(number, &system_disks())
        .ok_or_else(|| Report::new(Error::NotFound(path.to_path_buf())))
}

/// Opens the disk `disk` describes, its volumes locked and dismounted first
/// (Windows refuses a write into a mounted volume's sectors) and kept so
/// until the target is dropped. A disk takes whole sectors only: the
/// [`BlockFile`] makes every access one.
pub fn open(disk: &DiskInfo) -> Result<DiskTarget> {
    let number = match parse(&disk.path) {
        Some(Target::Disk(number)) => number,
        _ => return Err(Report::new(Error::NotWholeDisk(disk.path.clone()))),
    };
    check_writable(disk)?;
    let locks = lock(number, &disk.path)?;
    let file = BlockFile::open_rw(&disk_path(number)).map_err(|error| {
        let context = if error.kind() == io::ErrorKind::PermissionDenied {
            Error::PermissionDenied(disk.path.clone())
        } else {
            Error::Open(disk.path.clone())
        };
        Report::new(error).change_context(context)
    })?;
    Ok(DiskTarget::new(PhysicalDrive {
        file: Some(file),
        _locks: locks,
    }))
}

/// See [`DiskAdapter::check_writable`](remora_disk::adapter::DiskAdapter::check_writable):
/// Windows lets only an elevated Administrator write a physical drive or
/// lock its volumes.
pub fn check_writable(disk: &DiskInfo) -> Result<()> {
    if remora_elevate::is_elevated() {
        return Ok(());
    }
    Err(Report::new(Error::PermissionDenied(disk.path.clone())))
}

/// The whole disk behind `path` -- a `\\.\PhysicalDriveN`, or one of its
/// volumes (`\\.\E:`) -- its volumes locked and dismounted until the hold
/// is dropped. `None` for anything else: a file.
pub fn hold(path: &Path) -> Result<Option<DiskHold>> {
    let number = match parse(path) {
        Some(Target::Disk(number)) => number,
        Some(Target::Volume(volume)) => disk_of_volume(&volume)
            .ok_or_else(|| Report::new(Error::NotFound(path.to_path_buf())))?,
        None => return Ok(None),
    };
    if system_disks().contains(&number) {
        return Err(Report::new(Error::SystemDisk(path.to_path_buf())));
    }
    if !remora_elevate::is_elevated() {
        return Err(Report::new(Error::PermissionDenied(path.to_path_buf())));
    }
    Ok(Some(DiskHold::new(lock(number, path)?)))
}

fn lock(number: u32, path: &Path) -> Result<VolumeLocks> {
    volume::lock_volumes(number).map_err(|e| {
        let hint = e.hint();
        Report::new(e.error)
            .change_context(Error::Unmount(path.to_path_buf()))
            .attach(hint)
    })
}

/// A physical drive open for writing, its volumes locked.
struct PhysicalDrive {
    /// Taken by `eject`, which closes it itself.
    file: Option<BlockFile>,
    _locks: VolumeLocks,
}

impl PhysicalDrive {
    fn file(&mut self) -> &mut BlockFile {
        self.file.as_mut().expect("the drive is open until dropped")
    }
}

impl Write for PhysicalDrive {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file().flush()
    }
}

impl Seek for PhysicalDrive {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.file().seek(pos)
    }
}

impl DiskWrite for PhysicalDrive {
    fn sync(&mut self) -> io::Result<()> {
        self.file().sync_all()
    }

    /// Ejects the media, its volumes still locked so nothing of the new
    /// image mounts first. A card in a reader goes; a USB stick or disk
    /// whose media isn't removable refuses, and is only closed: Windows
    /// mounts its new volumes then, as on Linux.
    fn eject(mut self: Box<Self>) -> io::Result<bool> {
        let mut file = self.file.take().expect("the drive is open until dropped");
        file.flush()?;
        let ejected = ioctl::command(file.get_ref(), IOCTL_STORAGE_EJECT_MEDIA).is_ok();
        if !ejected {
            refresh(&file);
        }
        Ok(ejected)
    }
}

impl Drop for PhysicalDrive {
    fn drop(&mut self) {
        if let Some(mut file) = self.file.take() {
            let _ = file.flush();
            refresh(&file);
        }
    }
}

/// Has Windows read the disk's partition table again, the image's own
/// now, before its volumes are let go of.
fn refresh(file: &BlockFile) {
    let _ = ioctl::command(file.get_ref(), IOCTL_DISK_UPDATE_PROPERTIES);
}

/// What a path given for a disk names.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// `\\.\PhysicalDriveN`.
    Disk(u32),
    /// A volume, by the path to open it with (`\\.\E:`).
    Volume(String),
}

fn parse(path: &Path) -> Option<Target> {
    let path = path.to_str()?;
    let device = path
        .strip_prefix(r"\\.\")
        .or_else(|| path.strip_prefix(r"\\?\"));
    let name = device.unwrap_or(path).trim_end_matches('\\');
    if let Some(number) = name
        .get(..13)
        .filter(|prefix| prefix.eq_ignore_ascii_case("PhysicalDrive"))
        .and_then(|_| name[13..].parse::<u32>().ok())
    {
        return device.is_some().then_some(Target::Disk(number));
    }
    // A drive letter: `E:`, `E:\`, `\\.\E:`.
    let bytes = name.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Some(Target::Volume(format!(r"\\.\{name}")));
    }
    None
}

fn disk_path(number: u32) -> PathBuf {
    PathBuf::from(format!(r"\\.\PhysicalDrive{number}"))
}

/// The one disk `volume` lies on.
fn disk_of_volume(volume: &str) -> Option<u32> {
    let disks = ioctl::open_for_query(Path::new(volume))
        .and_then(|file| volume::disks_of(&file))
        .ok()?;
    let mut disks = disks.into_iter();
    match (disks.next(), disks.next()) {
        (Some(number), None) => Some(number),
        _ => None,
    }
}

fn disk_info(number: u32, system: &HashSet<u32>) -> Option<DiskInfo> {
    let path = disk_path(number);
    let file = ioctl::open_for_query(&path).ok()?;
    let descriptor = descriptor(&file);
    // No media (an empty card reader): no geometry, size 0.
    let size_bytes = ioctl::query::<DISK_GEOMETRY_EX>(&file, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX)
        .ok()
        .and_then(|geometry| u64::try_from(geometry.DiskSize).ok())
        .unwrap_or(0);
    let (removable_media, bus, model) = descriptor.unwrap_or((false, 0, None));
    Some(DiskInfo {
        path,
        size_bytes,
        model,
        // An SD card reader's media is removable; a USB stick's often
        // isn't, but it hangs off USB like any external disk.
        is_removable: removable_media || [BusTypeUsb, BusTypeSd, BusTypeMmc].contains(&bus),
        is_system_disk: system.contains(&number),
    })
}

/// The disk's removable-media flag, bus type, and vendor and product, from
/// its `STORAGE_DEVICE_DESCRIPTOR`.
fn descriptor(file: &std::fs::File) -> Option<(bool, i32, Option<String>)> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: StorageDeviceProperty,
        QueryType: PropertyStandardQuery,
        AdditionalParameters: [0],
    };
    // Room for the descriptor and the strings after it, as u64s for the
    // struct's alignment.
    let mut buffer = [0u64; 128];
    // SAFETY: the u64 buffer viewed as bytes, for the ioctl to fill.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), buffer.len() * 8)
    };
    let len = ioctl::control(
        file,
        IOCTL_STORAGE_QUERY_PROPERTY,
        ioctl::bytes_of(&query),
        bytes,
    )
    .ok()?;
    if len < std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() {
        return None;
    }
    // SAFETY: the buffer starts with the descriptor, aligned for it.
    let header = unsafe { &*buffer.as_ptr().cast::<STORAGE_DEVICE_DESCRIPTOR>() };
    let bytes = &bytes[..len];
    let string_at = |offset: u32| {
        let offset = offset as usize;
        (offset != 0 && offset < bytes.len()).then(|| {
            let s = &bytes[offset..];
            let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
            String::from_utf8_lossy(&s[..end]).trim().to_owned()
        })
    };
    let model = [
        string_at(header.VendorIdOffset),
        string_at(header.ProductIdOffset),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    Some((
        header.RemovableMedia,
        header.BusType,
        (!model.is_empty()).then_some(model),
    ))
}

/// The disks the running Windows lives on: the one holding its own
/// directory's volume (`C:`).
fn system_disks() -> HashSet<u32> {
    let mut buffer = [0u16; 260];
    // SAFETY: a buffer of the length given.
    let len = unsafe { GetSystemWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if len == 0 || len as usize >= buffer.len() {
        return HashSet::new();
    }
    let windows = from_wide(&buffer);
    let Some(drive) = windows.get(..2).filter(|d| d.ends_with(':')) else {
        return HashSet::new();
    };
    ioctl::open_for_query(Path::new(&format!(r"\\.\{drive}")))
        .and_then(|volume| volume::disks_of(&volume))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_physical_drive_path_names_its_disk() {
        assert_eq!(
            parse(Path::new(r"\\.\PhysicalDrive1")),
            Some(Target::Disk(1))
        );
        assert_eq!(
            parse(Path::new(r"\\?\physicaldrive12")),
            Some(Target::Disk(12))
        );
        // Without the device namespace, it's a file of that name.
        assert_eq!(parse(Path::new("PhysicalDrive1")), None);
        assert_eq!(parse(Path::new(r"\\.\PhysicalDrive")), None);
    }

    #[test]
    fn a_drive_letter_is_a_volume() {
        for path in ["E:", r"E:\", r"\\.\E:", r"\\.\e:\"] {
            assert!(
                matches!(parse(Path::new(path)), Some(Target::Volume(v)) if v.eq_ignore_ascii_case(r"\\.\E:")),
                "{path}"
            );
        }
        assert_eq!(parse(Path::new(r"C:\images\disk.img")), None);
    }

    #[test]
    fn the_system_disk_is_listed_as_such() {
        let disks = enumerate().unwrap();
        assert!(
            disks.iter().any(|d| d.is_system_disk),
            "no system disk among {disks:?}"
        );
    }

    #[test]
    fn info_describes_a_listed_disk() {
        let disk = enumerate().unwrap().into_iter().next().expect("a disk");
        let info = info(&disk.path).unwrap();
        assert_eq!(info.path, disk.path);
        assert_eq!(info.size_bytes, disk.size_bytes);
    }

    #[test]
    fn info_refuses_the_system_volume_naming_its_disk() {
        let err = info(Path::new("C:")).unwrap_err();
        assert!(matches!(err.current_context(), Error::NotWholeDisk(_)));
    }

    #[test]
    fn hold_has_nothing_to_hold_for_an_image_file() {
        let image = std::env::temp_dir().join("remora-hold.img");
        assert!(hold(&image).unwrap().is_none());
    }

    #[test]
    fn hold_refuses_the_system_disk() {
        let err = hold(Path::new(r"\\.\C:")).unwrap_err();
        assert!(matches!(err.current_context(), Error::SystemDisk(_)));
    }
}
