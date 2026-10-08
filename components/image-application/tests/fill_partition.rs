//! `ImageServiceInterface::fill_partition` against a real GPT image (built
//! with the real `sfdisk`, a dev-only tool): an installer's payload
//! partition replaced by a bigger, then a smaller source, its old content
//! never left behind, the image ending with a valid backup GPT
//! (`sgdisk -v`), and a partition that isn't the last one refused before
//! anything is written.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use remora_fs_walk::FsWalkAdapterService;
use remora_image::{
    adapter::{ext4::Ext4AdapterService, partition_table::PartitionTableAdapterService},
    application::{Error, ImageService},
    model::{FillOutcome, FillRequest, PartitionRole, PartitionSelector, FILL_ALIGNMENT},
};
use remora_image_adapter_ext4::Ext4AdapterImpl;
use remora_image_adapter_partition_table::PartitionTableAdapterImpl;
use remora_image_adapter_vfat::VfatAdapterImpl;
use remora_image_application::ImageControllerImpl;
use remora_progress::OperationContext;

const SECTOR: u64 = 512;
const PAYLOAD_START: u64 = 4096 * SECTOR;

fn controller() -> ImageService {
    ImageService::new(ImageControllerImpl::new(
        PartitionTableAdapterService::new(PartitionTableAdapterImpl),
        Arc::new(Ext4AdapterImpl),
        Arc::new(VfatAdapterImpl),
        FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
        Ext4AdapterService::new(Ext4AdapterImpl),
    ))
}

fn temp_path(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "remora-image-application-fill-test-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// An ESP of recognisable bytes, then a 2 MiB payload partition full of
/// 0xAA, the way an installer's previous payload would sit there.
fn installer_image() -> PathBuf {
    let image = temp_path("installer");
    fs::File::create(&image)
        .unwrap()
        .set_len(16 * 1024 * 1024)
        .unwrap();
    let script = "label: gpt\nunit: sectors\nfirst-lba: 34\n\n\
                  start=2048, size=2048, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name=\"EFI\"\n\
                  start=4096, size=4096, name=\"installer\"\n";
    let mut child = Command::new("sfdisk")
        .arg(&image)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("sfdisk not available");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "sfdisk failed");

    let mut bytes = fs::read(&image).unwrap();
    bytes[2048 * SECTOR as usize..PAYLOAD_START as usize].fill(0xE5);
    bytes[PAYLOAD_START as usize..(PAYLOAD_START + 2 * 1024 * 1024) as usize].fill(0xAA);
    fs::write(&image, bytes).unwrap();
    image
}

fn source(len: usize) -> PathBuf {
    let path = temp_path("source");
    fs::write(&path, (0..len).map(|i| (i % 251) as u8).collect::<Vec<_>>()).unwrap();
    path
}

async fn fill(image: &Path, source: &Path, role: PartitionRole) -> FillOutcome {
    controller()
        .fill_partition(
            &FillRequest {
                image: image.to_path_buf(),
                source: source.to_path_buf(),
                partition: PartitionSelector::Role(role),
            },
            &OperationContext::noop(),
        )
        .await
        .unwrap()
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk/sgdisk, Linux-only dev tools"
)]
async fn fills_the_payload_partition_grown_then_shrunk_to_its_source() {
    let image = installer_image();

    // Bigger than the partition, then smaller than the 0xAA it held: each
    // time the partition is the source, rounded up, zeros after it.
    for len in [5 * 1024 * 1024 + 7, 300 * 1024] {
        let source = source(len);
        let outcome = fill(&image, &source, PartitionRole::Installer).await;
        let partition_bytes = (len as u64).div_ceil(FILL_ALIGNMENT) * FILL_ALIGNMENT;
        assert_eq!(
            outcome,
            FillOutcome {
                index: 2,
                content_bytes: len as u64,
                partition_bytes,
            }
        );

        let bytes = fs::read(&image).unwrap();
        assert_eq!(
            bytes.len() as u64,
            PAYLOAD_START + partition_bytes + 33 * SECTOR
        );
        let start = PAYLOAD_START as usize;
        assert_eq!(bytes[start..start + len], fs::read(&source).unwrap()[..]);
        assert!(bytes[start + len..start + partition_bytes as usize]
            .iter()
            .all(|&b| b == 0));
        assert!(bytes[2048 * SECTOR as usize..start]
            .iter()
            .all(|&b| b == 0xE5));

        let verify = Command::new("sgdisk")
            .arg("-v")
            .arg(&image)
            .output()
            .expect("sgdisk not available");
        let verify = String::from_utf8_lossy(&verify.stdout);
        assert!(verify.contains("No problems found"), "{len}: {verify}");
        let _ = fs::remove_file(&source);
    }
    let _ = fs::remove_file(&image);
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk, a Linux-only dev tool"
)]
async fn refuses_a_partition_that_is_not_the_last_before_writing() {
    let image = installer_image();
    let before = fs::read(&image).unwrap();
    let source = source(1024);

    let err = controller()
        .fill_partition(
            &FillRequest {
                image: image.clone(),
                source: source.clone(),
                partition: PartitionSelector::Role(PartitionRole::Efi),
            },
            &OperationContext::noop(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err.current_context(), Error::Resize(1)), "{err:?}");
    assert_eq!(fs::read(&image).unwrap(), before);

    let _ = fs::remove_file(&source);
    let _ = fs::remove_file(&image);
}
