//! End-to-end exercise of `ConfigServiceInterface::upload` against a small
//! synthetic disk image with a real MBR partition table and a real `shared`
//! partition formatted either ext4 or vfat — covering both filesystem kinds
//! the partition-table adapter's `detect_fs_kind` has to tell apart. Built
//! with the real `sfdisk`/`mke2fs`/`mkfs.vfat`/`fsck.ext4`/`fsck.vfat`
//! (dev-only tools, never shelled out to by the shipped binary): each small
//! filesystem is built standalone (no loop devices, no root needed) and its
//! bytes copied into place at the partition's offset.

use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use remora_config::application::ConfigServiceInterface;
use remora_config_application::ConfigControllerImpl;
use remora_fs_walk::FsWalkAdapterService;
use remora_image::{
    adapter::{
        ext4::{Ext4Adapter, Ext4AdapterService},
        partition_table::PartitionTableAdapterService,
        vfat::VfatAdapter,
    },
    application::ImageService,
};
use remora_image_adapter_ext4::Ext4AdapterImpl;
use remora_image_adapter_partition_table::PartitionTableAdapterImpl;
use remora_image_adapter_vfat::VfatAdapterImpl;
use remora_image_application::ImageControllerImpl;

const PARTITION_OFFSET: u64 = 2048 * 512; // matches the real .wks.in alignment
                                          // 64 MiB matches the real shared/boot partition size in the .wks.in
                                          // templates (`--fixed-size 64M`) — small synthetic sizes leave too little
                                          // contiguous free space for a second 8 MiB config.ext4 write to reallocate
                                          // into after the first is freed, which is a real allocator limitation
                                          // unrelated to what this test is exercising.
const PARTITION_SIZE: u64 = 64 * 1024 * 1024;
const DISK_SIZE: u64 = 96 * 1024 * 1024;
const SFDISK_PARTITION_SECTORS: u64 = PARTITION_SIZE / 512;

fn controller() -> ConfigControllerImpl {
    let image = ImageService::new(ImageControllerImpl::new(
        PartitionTableAdapterService::new(PartitionTableAdapterImpl),
        Arc::new(Ext4AdapterImpl),
        Arc::new(VfatAdapterImpl),
        FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
        Ext4AdapterService::new(Ext4AdapterImpl),
    ));
    ConfigControllerImpl::new(
        FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
        image,
    )
}

fn temp_path(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "remora-config-upload-test-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path
}

/// A disk image with Remora's 4-partition MBR layout, its shared partition
/// (index 1 == `PartitionRole::Shared`) at `PARTITION_OFFSET` containing a
/// real filesystem of kind `mkfs` ("ext4" or "vfat"); slotA/slotB/data are
/// 1 MiB placeholders, unformatted.
fn build_disk_with_shared_partition(mkfs: &str) -> PathBuf {
    let disk = temp_path("disk");
    fs::File::create(&disk).unwrap().set_len(DISK_SIZE).unwrap();

    let script = format!(
        "label: dos\nunit: sectors\n\nstart=2048, size={SFDISK_PARTITION_SECTORS}, type=83\n\
         size=2048, type=83\nsize=2048, type=83\nsize=2048, type=83\n"
    );
    let mut child = Command::new("sfdisk")
        .arg(&disk)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("sfdisk not available");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "sfdisk failed");

    // Build the partition's filesystem standalone (no offset, no loop
    // device needed), then copy its bytes into place.
    let standalone = temp_path("standalone-fs");
    fs::File::create(&standalone)
        .unwrap()
        .set_len(PARTITION_SIZE)
        .unwrap();
    let status = match mkfs {
        "vfat" => Command::new("mkfs.vfat").arg(&standalone).status(),
        "ext4" => Command::new("mke2fs")
            .args(["-F", "-t", "ext4", "-q"])
            .arg(&standalone)
            .status(),
        other => panic!("unknown mkfs kind {other}"),
    }
    .expect("mkfs tool not available");
    assert!(status.success(), "{mkfs} formatting failed");

    let fs_bytes = fs::read(&standalone).unwrap();
    let mut disk_file = fs::OpenOptions::new().write(true).open(&disk).unwrap();
    disk_file.seek(SeekFrom::Start(PARTITION_OFFSET)).unwrap();
    disk_file.write_all(&fs_bytes).unwrap();
    drop(disk_file);
    let _ = fs::remove_file(&standalone);

    disk
}

fn fsck_shared_partition(disk: &PathBuf, mkfs: &str) {
    let standalone = temp_path("fsck-copy");
    let mut disk_file = fs::File::open(disk).unwrap();
    disk_file.seek(SeekFrom::Start(PARTITION_OFFSET)).unwrap();
    let mut buf = vec![0u8; PARTITION_SIZE as usize];
    disk_file.read_exact(&mut buf).unwrap();
    fs::write(&standalone, &buf).unwrap();

    let status = match mkfs {
        "vfat" => Command::new("fsck.vfat")
            .args(["-n"])
            .arg(&standalone)
            .status(),
        "ext4" => Command::new("fsck.ext4")
            .args(["-n", "-f"])
            .arg(&standalone)
            .status(),
        other => panic!("unknown mkfs kind {other}"),
    }
    .expect("fsck tool not available");
    assert!(
        status.success(),
        "{mkfs} fsck reported the shared partition as unclean"
    );
    let _ = fs::remove_file(&standalone);
}

fn read_back(disk: &Path, mkfs: &str, dest_path: &str) -> Vec<u8> {
    match mkfs {
        "vfat" => VfatAdapterImpl
            .read_file(disk, PARTITION_OFFSET, PARTITION_SIZE, dest_path)
            .unwrap(),
        "ext4" => Ext4AdapterImpl
            .read_file(disk, PARTITION_OFFSET, PARTITION_SIZE, dest_path)
            .unwrap(),
        other => panic!("unknown mkfs kind {other}"),
    }
}

async fn run_config_upload_round_trip(mkfs: &str) {
    let disk = build_disk_with_shared_partition(mkfs);
    let controller = controller();

    let source = temp_path("source-file");
    fs::write(&source, b"Europe/Paris\n").unwrap();

    // First upload: shared:/remora/slot-A/config doesn't exist yet, so a
    // fresh config.ext4 is built (seeded with tzdata/) and injected.
    controller
        .upload(&disk, &source, "/timezone", "slot-A", 0o644)
        .await
        .unwrap();

    let config_bytes = read_back(&disk, mkfs, "/remora/slot-A/config");
    assert!(!config_bytes.is_empty());

    let inner = temp_path("inner-config");
    fs::write(&inner, &config_bytes).unwrap();
    assert_eq!(
        Ext4AdapterImpl
            .read_file(&inner, 0, config_bytes.len() as u64, "/timezone")
            .unwrap(),
        b"Europe/Paris\n"
    );
    assert!(Ext4AdapterImpl
        .exists(&inner, 0, config_bytes.len() as u64, "/tzdata")
        .unwrap());

    // Second upload: shared:/remora/slot-A/config already exists — must
    // update it in place (replace /timezone, keep /tzdata) rather than
    // wiping it out or erroring.
    let source2 = temp_path("source-file-2");
    fs::write(&source2, b"UTC\n").unwrap();
    controller
        .upload(&disk, &source2, "/timezone", "slot-A", 0o644)
        .await
        .unwrap();

    let config_bytes2 = read_back(&disk, mkfs, "/remora/slot-A/config");
    let inner2 = temp_path("inner-config-2");
    fs::write(&inner2, &config_bytes2).unwrap();
    assert_eq!(
        Ext4AdapterImpl
            .read_file(&inner2, 0, config_bytes2.len() as u64, "/timezone")
            .unwrap(),
        b"UTC\n"
    );
    assert!(Ext4AdapterImpl
        .exists(&inner2, 0, config_bytes2.len() as u64, "/tzdata")
        .unwrap());

    fsck_shared_partition(&disk, mkfs);

    for p in [source, source2, inner, inner2, disk] {
        let _ = fs::remove_file(p);
    }
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk/mke2fs/fsck.ext4, Linux-only dev tools"
)]
async fn config_upload_round_trips_on_an_ext4_shared_partition() {
    run_config_upload_round_trip("ext4").await;
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk/mkfs.vfat/fsck.vfat, Linux-only dev tools"
)]
async fn config_upload_round_trips_on_a_vfat_shared_partition() {
    run_config_upload_round_trip("vfat").await;
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk/mke2fs/fsck.ext4, Linux-only dev tools"
)]
async fn config_upload_creates_missing_ancestor_directories() {
    // A fresh config.ext4 is seeded with only `tzdata/` at its root -- any
    // caller uploading to a nested path (e.g. tplsth's `/c/<file>` coder
    // config) must not have to create `/c` itself first.
    let disk = build_disk_with_shared_partition("ext4");
    let controller = controller();

    let source = temp_path("coder-source");
    fs::write(&source, b"{\"coder\":1}").unwrap();

    controller
        .upload(&disk, &source, "/c/coder1.json", "slot-A", 0o644)
        .await
        .unwrap();

    let config_bytes = read_back(&disk, "ext4", "/remora/slot-A/config");
    let inner = temp_path("inner-config-nested");
    fs::write(&inner, &config_bytes).unwrap();
    assert_eq!(
        Ext4AdapterImpl
            .read_file(&inner, 0, config_bytes.len() as u64, "/c/coder1.json")
            .unwrap(),
        b"{\"coder\":1}"
    );

    fsck_shared_partition(&disk, "ext4");

    for p in [source, inner, disk] {
        let _ = fs::remove_file(p);
    }
}
