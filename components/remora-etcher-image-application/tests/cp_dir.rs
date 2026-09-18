//! End-to-end exercise of `ImageServiceInterface::cp_dir` against a small
//! synthetic disk image with a real MBR partition table and a real
//! partition formatted either ext4 or vfat — covering both filesystem kinds
//! the partition-table adapter's `detect_fs_kind` has to tell apart. Built
//! with the real `sfdisk`/`mke2fs`/`mkfs.vfat`/`fsck.ext4`/`fsck.vfat`
//! (dev-only tools, never shelled out to by the shipped binary): the small
//! filesystem is built standalone (no loop devices, no root needed) and its
//! bytes copied into place at the partition's offset. Same fixture recipe
//! as `remora-etcher-config-application`'s `config_upload` test.

use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use remora_etcher_fs_walk::FsWalkAdapterService;
use remora_etcher_image::{
    adapter::{
        ext4::Ext4Adapter, partition_table::PartitionTableAdapterService, vfat::VfatAdapter,
    },
    application::ImageService,
    model::{CpDirRequest, PartitionSelector},
};
use remora_etcher_image_adapter_ext4::Ext4AdapterImpl;
use remora_etcher_image_adapter_partition_table::PartitionTableAdapterImpl;
use remora_etcher_image_adapter_vfat::VfatAdapterImpl;
use remora_etcher_image_application::ImageControllerImpl;

const PARTITION_OFFSET: u64 = 2048 * 512;
const PARTITION_SIZE: u64 = 32 * 1024 * 1024;
const DISK_SIZE: u64 = 40 * 1024 * 1024;
const SFDISK_PARTITION_SECTORS: u64 = PARTITION_SIZE / 512;

fn controller() -> ImageService {
    ImageService::new(ImageControllerImpl::new(
        PartitionTableAdapterService::new(PartitionTableAdapterImpl),
        Arc::new(Ext4AdapterImpl),
        Arc::new(VfatAdapterImpl),
        FsWalkAdapterService::new(remora_etcher_fs_walk::FsWalkAdapterImpl),
    ))
}

fn temp_path(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "remora-etcher-image-application-cp-dir-test-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path
}

/// A disk image with a single MBR partition (index 1) at `PARTITION_OFFSET`,
/// containing a real filesystem of kind `mkfs` ("ext4" or "vfat").
fn build_disk_with_partition(mkfs: &str) -> PathBuf {
    let disk = temp_path("disk");
    fs::File::create(&disk).unwrap().set_len(DISK_SIZE).unwrap();

    let script = format!(
        "label: dos\nunit: sectors\n\nstart=2048, size={SFDISK_PARTITION_SECTORS}, type=83\n"
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

fn read_back(disk: &std::path::Path, mkfs: &str, dest_path: &str) -> Vec<u8> {
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

async fn run_cp_dir_round_trip(mkfs: &str) {
    let disk = build_disk_with_partition(mkfs);

    let source = temp_path("source-dir");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::write(source.join("top.txt"), b"top-level\n").unwrap();
    fs::write(source.join("nested/inner.txt"), b"nested-file\n").unwrap();

    controller()
        .cp_dir(
            &CpDirRequest {
                image: disk.clone(),
                source_dir: source.clone(),
                dest_path: "/".to_string(),
                partition: PartitionSelector::Index(1),
                boot_mode: None,
            },
            &remora_etcher_progress::OperationContext::noop(),
        )
        .await
        .unwrap();

    assert_eq!(read_back(&disk, mkfs, "/top.txt"), b"top-level\n");
    assert_eq!(
        read_back(&disk, mkfs, "/nested/inner.txt"),
        b"nested-file\n"
    );

    let _ = fs::remove_dir_all(&source);
    let _ = fs::remove_file(&disk);
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk/mke2fs, Linux-only dev tools"
)]
async fn cp_dir_round_trips_on_an_ext4_partition() {
    run_cp_dir_round_trip("ext4").await;
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk/mkfs.vfat, Linux-only dev tools"
)]
async fn cp_dir_round_trips_on_a_vfat_partition() {
    run_cp_dir_round_trip("vfat").await;
}
