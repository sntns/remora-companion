use std::{
    collections::HashSet,
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
};

use error_stack::{Report, ResultExt};
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

/// The disk `path` really opens. A device node's *name* says nothing about
/// which disk it is (`~/sdc -> /dev/sda` is named `sdc`), so the node is
/// resolved through every symlink and then matched by its device number
/// against `/sys/block/*/dev` -- the same identity the kernel uses when the
/// flash opens it.
pub fn info(path: &Path) -> Result<DiskInfo> {
    let canonical =
        fs::canonicalize(path).change_context_lazy(|| Error::NotFound(path.to_path_buf()))?;
    let meta =
        fs::metadata(&canonical).change_context_lazy(|| Error::NotFound(path.to_path_buf()))?;
    if !meta.file_type().is_block_device() {
        return Err(Report::new(Error::NotBlockDevice(path.to_path_buf())));
    }

    let name = whole_disk_named(Path::new(SYS_BLOCK), &majmin(meta.rdev()))?
        .ok_or_else(|| Report::new(Error::NotWholeDisk(path.to_path_buf())))?;
    let mut info = disk_info(&name, &system_device_majmins())?;
    // The node actually opened, not `/dev/<name>`: the two only differ for
    // a node outside `/dev`, and it's this one the flash guard compares.
    info.path = canonical;
    Ok(info)
}

/// `major:minor`, as `/sys/block/*/dev` spells it, of a `st_rdev` (glibc's
/// `gnu_dev_major`/`gnu_dev_minor` encoding).
fn majmin(rdev: u64) -> String {
    let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff);
    let minor = (rdev & 0xff) | ((rdev >> 12) & !0xff);
    format!("{major}:{minor}")
}

/// The `/sys/block` entry whose `dev` is `majmin`, if any. Partitions live
/// one level down (`/sys/block/sda/sda1`), so they never match here.
fn whole_disk_named(sys_block: &Path, majmin: &str) -> Result<Option<String>> {
    for entry in fs::read_dir(sys_block).change_context(Error::Io)? {
        let entry = entry.change_context(Error::Io)?;
        if read_trimmed(&entry.path().join("dev")).is_ok_and(|dev| dev == majmin) {
            return Ok(Some(entry.file_name().to_string_lossy().into_owned()));
        }
    }
    Ok(None)
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
    fn majmin_decodes_glibc_dev_t() {
        assert_eq!(majmin(0x0802), "8:2");
        assert_eq!(majmin((259 << 8) | 1), "259:1");
        // Minors past 255 spill into the high bits.
        assert_eq!(majmin((8 << 8) | (1 << 20) | 0x10), "8:272");
    }

    #[test]
    fn whole_disk_named_matches_a_disk_never_a_partition() {
        let fixture = SysBlockFixture::new();
        fixture.disk("sda", "8:0");
        fixture.partition("sda", "sda1", "8:1");
        fixture.disk("sdc", "8:32");

        assert_eq!(
            whole_disk_named(&fixture.dir, "8:32").unwrap().as_deref(),
            Some("sdc")
        );
        assert_eq!(whole_disk_named(&fixture.dir, "8:1").unwrap(), None);
    }

    /// A whole disk of this machine with its `/dev` node, if any.
    fn some_real_disk() -> Option<String> {
        fs::read_dir(SYS_BLOCK)
            .ok()?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|name| Path::new("/dev").join(name).exists())
    }

    #[test]
    fn info_resolves_a_symlink_to_the_disk_it_opens() {
        let Some(name) = some_real_disk() else {
            eprintln!("no block device on this machine, skipping");
            return;
        };
        let fixture = SysBlockFixture::new();
        // Named like another disk: only the target may count.
        let link = fixture.dir.join("sdzz");
        std::os::unix::fs::symlink(Path::new("/dev").join(&name), &link).unwrap();

        let info = info(&link).unwrap();
        assert_eq!(info.path, Path::new("/dev").join(&name));
    }

    #[test]
    fn info_refuses_a_node_that_is_not_a_block_device() {
        let err = info(Path::new("/dev/null")).unwrap_err();
        assert!(matches!(err.current_context(), Error::NotBlockDevice(_)));
    }

    #[test]
    fn info_refuses_a_partition() {
        let partition = fs::read_dir(SYS_BLOCK).ok().and_then(|disks| {
            disks.flatten().find_map(|disk| {
                let disk = disk.file_name().to_string_lossy().into_owned();
                fs::read_dir(Path::new(SYS_BLOCK).join(&disk))
                    .ok()?
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .find(|n| n.starts_with(&disk) && Path::new("/dev").join(n).exists())
            })
        });
        let Some(partition) = partition else {
            eprintln!("no partitioned block device on this machine, skipping");
            return;
        };
        let err = info(&Path::new("/dev").join(partition)).unwrap_err();
        assert!(matches!(err.current_context(), Error::NotWholeDisk(_)));
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
