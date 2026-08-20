use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::model::disk::DiskInfo;

use super::Error;

const SYS_BLOCK: &str = "/sys/block";

pub fn enumerate() -> Result<Vec<DiskInfo>, Error> {
    let root_dev = root_device_majmin();

    let mut disks = Vec::new();
    for entry in fs::read_dir(SYS_BLOCK)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        disks.push(disk_info(&name, root_dev.as_deref())?);
    }
    disks.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(disks)
}

pub fn info(path: &Path) -> Result<DiskInfo, Error> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| Error::NotFound {
            path: path.to_path_buf(),
        })?;

    if !Path::new(SYS_BLOCK).join(&name).exists() {
        return Err(Error::NotFound {
            path: path.to_path_buf(),
        });
    }

    disk_info(&name, root_device_majmin().as_deref())
}

fn disk_info(name: &str, root_dev: Option<&str>) -> Result<DiskInfo, Error> {
    let sys_dir = Path::new(SYS_BLOCK).join(name);

    let size_bytes = read_trimmed(&sys_dir.join("size"))
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
        // /sys/block/*/size is always in 512-byte sectors, regardless of the
        // device's real sector size (this is a long-standing kernel ABI).
        .saturating_mul(512);

    let is_removable = read_trimmed(&sys_dir.join("removable"))
        .map(|s| s.trim() == "1")
        .unwrap_or(false);

    let model = read_trimmed(&sys_dir.join("device/model"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let is_system_disk = root_dev.is_some_and(|root| device_owns_majmin(&sys_dir, name, root));

    Ok(DiskInfo {
        path: PathBuf::from("/dev").join(name),
        size_bytes,
        model,
        is_removable,
        is_system_disk,
    })
}

/// True if `sys_dir` itself, or any of its partition subdirectories, has the
/// `dev` (major:minor) that backs the current root filesystem.
fn device_owns_majmin(sys_dir: &Path, name: &str, root_majmin: &str) -> bool {
    if read_trimmed(&sys_dir.join("dev"))
        .map(|d| d == root_majmin)
        .unwrap_or(false)
    {
        return true;
    }

    let Ok(entries) = fs::read_dir(sys_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let entry_name = entry.file_name().to_string_lossy().into_owned();
        // Partitions of e.g. `sda` are named `sda1`, `sda2`, ...
        if !entry_name.starts_with(name) || entry_name == name {
            continue;
        }
        if read_trimmed(&entry.path().join("dev"))
            .map(|d| d == root_majmin)
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// The `major:minor` of the block device backing the current root filesystem,
/// read from `/proc/self/mountinfo` (field 3, per `proc(5)`).
fn root_device_majmin() -> Option<String> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").ok()?;
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Fields: id parent-id major:minor root mount-point ...
        if fields.len() > 4 && fields[4] == "/" {
            return Some(fields[2].to_string());
        }
    }
    None
}

fn read_trimmed(path: &Path) -> std::io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_string())
}
