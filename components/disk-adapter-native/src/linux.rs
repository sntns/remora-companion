use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use error_stack::ResultExt;
use remora_disk::{
    adapter::{Error, Result},
    model::DiskInfo,
};

const SYS_BLOCK: &str = "/sys/block";

/// Mount points that host the currently running system. A disk backing any
/// of these is never a legitimate flash target — unlike, say, an external
/// drive mounted under `/media` or `/mnt`, which is the normal case for a
/// tool whose whole job is to overwrite plugged-in removable disks.
const CRITICAL_MOUNT_POINTS: &[&str] = &["/", "/boot", "/boot/efi", "/usr", "/var"];

pub fn enumerate() -> Result<Vec<DiskInfo>> {
    let system_devs = system_device_majmins();

    let mut disks = Vec::new();
    for entry in fs::read_dir(SYS_BLOCK).change_context(Error::Io)? {
        let entry = entry.change_context(Error::Io)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        disks.push(disk_info(&name, &system_devs)?);
    }
    disks.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(disks)
}

pub fn info(path: &Path) -> Result<DiskInfo> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| error_stack::Report::new(Error::NotFound(path.to_path_buf())))?;

    if !Path::new(SYS_BLOCK).join(&name).exists() {
        return Err(error_stack::Report::new(Error::NotFound(
            path.to_path_buf(),
        )));
    }

    disk_info(&name, &system_device_majmins())
}

fn disk_info(name: &str, system_devs: &HashSet<String>) -> Result<DiskInfo> {
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

    let is_system_disk = device_owns_majmin(&sys_dir, name, system_devs);

    Ok(DiskInfo {
        path: PathBuf::from("/dev").join(name),
        size_bytes,
        model,
        is_removable,
        is_system_disk,
    })
}

/// True if `sys_dir` itself, or any of its partition subdirectories, has the
/// `dev` (major:minor) of a device in `system_devs`.
fn device_owns_majmin(sys_dir: &Path, name: &str, system_devs: &HashSet<String>) -> bool {
    if read_trimmed(&sys_dir.join("dev"))
        .map(|d| system_devs.contains(&d))
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
            .map(|d| system_devs.contains(&d))
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

/// The `major:minor` of every block device backing a [`CRITICAL_MOUNT_POINTS`]
/// mount point, read from `/proc/self/mountinfo` (per `proc(5)`).
fn system_device_majmins() -> HashSet<String> {
    fs::read_to_string("/proc/self/mountinfo")
        .map(|s| parse_system_device_majmins(&s))
        .unwrap_or_default()
}

/// Pure parsing step, split out from [`system_device_majmins`] so it's
/// testable without touching the real filesystem.
fn parse_system_device_majmins(mountinfo: &str) -> HashSet<String> {
    mountinfo
        .lines()
        .filter_map(|line| {
            // Fields: id parent-id major:minor root mount-point ...
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() > 4 && CRITICAL_MOUNT_POINTS.contains(&fields[4]) {
                Some(fields[2].to_string())
            } else {
                None
            }
        })
        .collect()
}

fn read_trimmed(path: &Path) -> std::io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    #[test]
    fn parse_system_device_majmins_picks_up_root() {
        let mountinfo = "36 35 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n";
        let devs = parse_system_device_majmins(mountinfo);
        assert_eq!(devs, HashSet::from(["8:1".to_string()]));
    }

    #[test]
    fn parse_system_device_majmins_picks_up_boot_efi_on_a_separate_disk() {
        let mountinfo = "\
36 35 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
37 36 8:1 / /usr rw,relatime shared:1 - ext4 /dev/sda1 rw
38 36 259:1 / /boot rw,relatime shared:1 - ext4 /dev/nvme0n1p1 rw
39 38 259:2 / /boot/efi rw,relatime shared:1 - vfat /dev/nvme0n1p2 rw
";
        let devs = parse_system_device_majmins(mountinfo);
        assert_eq!(
            devs,
            HashSet::from(["8:1".to_string(), "259:1".to_string(), "259:2".to_string()])
        );
    }

    #[test]
    fn parse_system_device_majmins_ignores_unrelated_mounts() {
        let mountinfo = "\
36 35 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
40 35 8:33 / /media/user/BACKUP rw,relatime shared:1 - ext4 /dev/sdc1 rw
41 35 8:34 / /home rw,relatime shared:1 - ext4 /dev/sdd1 rw
";
        let devs = parse_system_device_majmins(mountinfo);
        assert_eq!(devs, HashSet::from(["8:1".to_string()]));
    }

    #[test]
    fn parse_system_device_majmins_ignores_malformed_lines() {
        let mountinfo = "not enough fields here\n\n36 35 8:1 /\n";
        assert!(parse_system_device_majmins(mountinfo).is_empty());
    }

    static FIXTURE_COUNTER: AtomicU32 = AtomicU32::new(0);

    /// Builds a throwaway `/sys/block/<name>`-shaped tree under the OS temp
    /// dir and removes it on drop.
    struct SysBlockFixture {
        dir: PathBuf,
    }

    impl SysBlockFixture {
        fn new() -> Self {
            let id = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "remora-disk-adapter-native-test-{}-{}",
                std::process::id(),
                id
            ));
            fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn disk(&self, name: &str, dev: &str) -> PathBuf {
            let disk_dir = self.dir.join(name);
            fs::create_dir_all(&disk_dir).unwrap();
            fs::write(disk_dir.join("dev"), dev).unwrap();
            disk_dir
        }

        fn partition(&self, disk_name: &str, partition_name: &str, dev: &str) {
            let part_dir = self.dir.join(disk_name).join(partition_name);
            fs::create_dir_all(&part_dir).unwrap();
            fs::write(part_dir.join("dev"), dev).unwrap();
        }
    }

    impl Drop for SysBlockFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn device_owns_majmin_matches_the_whole_disk_dev() {
        let fixture = SysBlockFixture::new();
        let sys_dir = fixture.disk("sda", "8:0");
        let system_devs = HashSet::from(["8:0".to_string()]);

        assert!(device_owns_majmin(&sys_dir, "sda", &system_devs));
    }

    #[test]
    fn device_owns_majmin_matches_a_partition_dev() {
        let fixture = SysBlockFixture::new();
        let sys_dir = fixture.disk("nvme0n1", "259:0");
        fixture.partition("nvme0n1", "nvme0n1p2", "259:2");
        let system_devs = HashSet::from(["259:2".to_string()]);

        assert!(device_owns_majmin(&sys_dir, "nvme0n1", &system_devs));
    }

    #[test]
    fn device_owns_majmin_is_false_for_an_unrelated_disk() {
        let fixture = SysBlockFixture::new();
        let sys_dir = fixture.disk("sdb", "8:16");
        fixture.partition("sdb", "sdb1", "8:17");
        let system_devs = HashSet::from(["8:0".to_string(), "259:2".to_string()]);

        assert!(!device_owns_majmin(&sys_dir, "sdb", &system_devs));
    }
}
