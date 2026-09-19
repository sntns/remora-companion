//! End-to-end exercise of `FactoryServiceInterface::provision` against a
//! real ext4 shared partition, using a fake `FactoryProvisioningAdapter`
//! (no network call -- see its doc comment) so the test only depends on
//! this crate's own logic: local keypair/CSR generation and writing every
//! item into the image.
//!
//! Disk fixture built the same way as `remora-etcher-batch-application`'s
//! `tests/batch.rs`: a real MBR partition table (`sfdisk`) plus a real
//! ext4 `shared` partition (`mke2fs`) -- dev-only tools, never shelled out
//! to by the shipped binary.

use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use remora_etcher_factory::{
    adapter::{FactoryProvisioningAdapter, FactoryProvisioningAdapterService, ProvisionedIdentity},
    application::FactoryServiceInterface,
    model::CertificateReference,
};
use remora_etcher_factory_application::FactoryControllerImpl;
use remora_etcher_fs_walk::FsWalkAdapterService;
use remora_etcher_image::{
    adapter::{ext4::Ext4Adapter, partition_table::PartitionTableAdapterService},
    application::ImageService,
};
use remora_etcher_image_adapter_ext4::Ext4AdapterImpl;
use remora_etcher_image_adapter_partition_table::PartitionTableAdapterImpl;
use remora_etcher_image_adapter_vfat::VfatAdapterImpl;
use remora_etcher_image_application::ImageControllerImpl;
use remora_etcher_progress::OperationContext;

const PARTITION_OFFSET: u64 = 2048 * 512;
const PARTITION_SIZE: u64 = 32 * 1024 * 1024;
const DISK_SIZE: u64 = 48 * 1024 * 1024;
const SFDISK_PARTITION_SECTORS: u64 = PARTITION_SIZE / 512;

/// Stands in for the real gateway call (see
/// `remora-etcher-factory-adapter-gateway`, gated on a platform endpoint
/// this test has no business depending on) -- returns a fixed, made-up but
/// correctly-shaped response, so this test only exercises this crate's own
/// logic (keygen/CSR/writing into the image).
struct FakeProvisioning;

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for FakeProvisioning {
    async fn provision(
        &self,
        _gateway_url: &str,
        _api_key: &str,
        device_name: &str,
        _csr_der: &[u8],
    ) -> remora_etcher_factory::adapter::Result<ProvisionedIdentity> {
        Ok(ProvisionedIdentity {
            factory_device_name: format!("test:remora:factory-device:{device_name}"),
            certificate_reference: CertificateReference {
                id: "cert-id".to_string(),
                urn: format!("test:kms:certificate:{device_name}"),
                name: "cert-name".to_string(),
            },
            certificate_der: b"fake-certificate-der".to_vec(),
            certificate_authority_der: b"fake-factory-ca-der".to_vec(),
            server_certificate_authority_der: b"fake-server-ca-der".to_vec(),
        })
    }
}

fn temp_path(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "remora-etcher-factory-test-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path
}

fn build_raw_disk_with_shared_partition() -> PathBuf {
    let disk = temp_path("disk");
    fs::File::create(&disk).unwrap().set_len(DISK_SIZE).unwrap();

    let script = format!(
        "label: dos\nunit: sectors\n\nstart=2048, size={SFDISK_PARTITION_SECTORS}, type=83\n"
    );
    let mut child = std::process::Command::new("sfdisk")
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
    let status = std::process::Command::new("mke2fs")
        .args(["-F", "-t", "ext4", "-q"])
        .arg(&standalone)
        .status()
        .expect("mke2fs not available");
    assert!(status.success(), "ext4 formatting failed");

    let fs_bytes = fs::read(&standalone).unwrap();
    let mut disk_file = fs::OpenOptions::new().write(true).open(&disk).unwrap();
    disk_file.seek(SeekFrom::Start(PARTITION_OFFSET)).unwrap();
    disk_file.write_all(&fs_bytes).unwrap();
    drop(disk_file);
    let _ = fs::remove_file(&standalone);

    disk
}

fn read_shared_partition_file(disk: &std::path::Path, dest_path: &str) -> Vec<u8> {
    Ext4AdapterImpl
        .read_file(disk, PARTITION_OFFSET, PARTITION_SIZE, dest_path)
        .unwrap()
}

fn controller() -> FactoryControllerImpl {
    let image = ImageService::new(ImageControllerImpl::new(
        PartitionTableAdapterService::new(PartitionTableAdapterImpl),
        Arc::new(Ext4AdapterImpl),
        Arc::new(VfatAdapterImpl),
        FsWalkAdapterService::new(remora_etcher_fs_walk::FsWalkAdapterImpl),
    ));
    FactoryControllerImpl::new(
        FactoryProvisioningAdapterService::new(FakeProvisioning),
        image,
    )
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk/mke2fs, Linux-only dev tools"
)]
async fn provisions_a_device_and_writes_every_item_into_the_image() {
    let disk = build_raw_disk_with_shared_partition();

    controller()
        .provision(
            "e2e-serial-0001",
            "https://remora.access.eu2.sntns.io/access/v1",
            "https://api.example.invalid",
            "unused-in-the-fake",
            &disk,
            &OperationContext::noop(),
        )
        .await
        .expect("provisioning should succeed against the fake adapter");

    assert!(!read_shared_partition_file(&disk, "/remora/factory/private_key.der").is_empty());
    assert_eq!(
        read_shared_partition_file(&disk, "/remora/factory/certificate.der"),
        b"fake-certificate-der"
    );
    assert_eq!(
        read_shared_partition_file(&disk, "/remora/factory/factory_ca.der"),
        b"fake-factory-ca-der"
    );
    assert_eq!(
        read_shared_partition_file(&disk, "/remora/factory/server_ca.der"),
        b"fake-server-ca-der"
    );
    assert_eq!(
        read_shared_partition_file(&disk, "/remora/factory/keyid"),
        b"test:kms:certificate:e2e-serial-0001"
    );
    assert_eq!(
        read_shared_partition_file(&disk, "/remora/factory/access_url"),
        b"https://remora.access.eu2.sntns.io/access/v1"
    );

    let _ = fs::remove_file(&disk);
}
