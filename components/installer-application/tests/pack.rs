//! `InstallerServiceInterface::pack` end to end, with the real convert and
//! image verticals: an installer `.bmaptar` (an ESP, then its payload
//! partition, partitioned by the real `sfdisk`, a dev-only tool) packed
//! with a product image given as a `.bmaptar` and as a raw `.wic`, the
//! output decoded back to check its ESP is untouched and its payload is
//! the image, byte for byte once decoded.

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

use remora_convert::application::ConvertService;
use remora_convert_adapter_bmaptar::BmaptarAdapterImpl;
use remora_convert_adapter_bzip2::Bzip2AdapterImpl;
use remora_convert_adapter_gzip::GzipAdapterImpl;
use remora_convert_adapter_qcow2::Qcow2AdapterImpl;
use remora_convert_adapter_zstd::ZstdAdapterImpl;
use remora_convert_application::{ContainerFormats, ConvertControllerImpl};
use remora_fs_walk::FsWalkAdapterService;
use remora_image::{
    adapter::{ext4::Ext4AdapterService, partition_table::PartitionTableAdapterService},
    application::ImageService,
    model::PartitionRole,
};
use remora_image_adapter_ext4::Ext4AdapterImpl;
use remora_image_adapter_partition_table::PartitionTableAdapterImpl;
use remora_image_adapter_vfat::VfatAdapterImpl;
use remora_image_application::ImageControllerImpl;
use remora_installer::{
    application::{Error, InstallerService},
    model::PackRequest,
};
use remora_installer_application::InstallerControllerImpl;
use remora_progress::OperationContext;

const SECTOR: usize = 512;
const ESP: std::ops::Range<usize> = 2048 * SECTOR..4096 * SECTOR;

fn convert() -> ConvertService {
    ConvertService::new(ConvertControllerImpl::new(ContainerFormats {
        qcow2: Arc::new(Qcow2AdapterImpl),
        gzip: Arc::new(GzipAdapterImpl),
        zstd: Arc::new(ZstdAdapterImpl),
        bzip2: Arc::new(Bzip2AdapterImpl),
        bmaptar: Arc::new(BmaptarAdapterImpl),
    }))
}

fn image() -> ImageService {
    ImageService::new(ImageControllerImpl::new(
        PartitionTableAdapterService::new(PartitionTableAdapterImpl),
        Arc::new(Ext4AdapterImpl),
        Arc::new(VfatAdapterImpl),
        FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
        Ext4AdapterService::new(Ext4AdapterImpl),
    ))
}

fn installer() -> InstallerService {
    InstallerService::new(InstallerControllerImpl::new(convert(), image()))
}

/// A throwaway directory, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "remora-installer-pack-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A raw image of `len` bytes partitioned by `sfdisk` from `script`.
fn sfdisk(path: &Path, len: u64, script: &str) {
    fs::File::create(path).unwrap().set_len(len).unwrap();
    let mut child = Command::new("sfdisk")
        .arg(path)
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
}

/// An installer as meta-remora's wks lays it out: an ESP of recognisable
/// bytes, then a 1 MiB payload partition, encoded as a `.bmaptar`.
async fn installer_bmaptar(dir: &TempDir) -> PathBuf {
    let raw = dir.join("installer.wic");
    sfdisk(
        &raw,
        8 * 1024 * 1024,
        "label: gpt\nunit: sectors\nfirst-lba: 34\n\n\
         start=2048, size=2048, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name=\"EFI\"\n\
         start=4096, size=2048, name=\"installer\"\n",
    );
    let mut bytes = fs::read(&raw).unwrap();
    bytes[ESP].fill(0xE5);
    fs::write(&raw, bytes).unwrap();

    let bundle = dir.join("remora-installer.wic.bmaptar");
    convert()
        .from_raw(&raw, &bundle, &OperationContext::noop())
        .await
        .unwrap();
    bundle
}

/// A product disk image bigger than the installer's payload partition:
/// some data at its start, then mostly holes, as a real one is.
fn product_wic(dir: &TempDir) -> PathBuf {
    let raw = dir.join("product.wic");
    sfdisk(
        &raw,
        12 * 1024 * 1024,
        "label: gpt\nunit: sectors\n\nstart=2048, size=8192, name=\"data\"\n",
    );
    let mut bytes = fs::read(&raw).unwrap();
    for (i, b) in bytes[2048 * SECTOR..2048 * SECTOR + 3 * 1024 * 1024]
        .iter_mut()
        .enumerate()
    {
        *b = (i % 251) as u8;
    }
    fs::write(&raw, bytes).unwrap();
    raw
}

/// The packed installer, decoded: its bytes, and its payload partition's.
async fn unpack(dir: &TempDir, output: &Path) -> (Vec<u8>, Vec<u8>) {
    let raw = dir.join("output.wic");
    convert()
        .to_raw(output, &raw, &OperationContext::noop())
        .await
        .unwrap();
    let table = image().inspect(&raw).await.unwrap();
    let payload = table.select_role(PartitionRole::Installer).unwrap();
    let bytes = fs::read(&raw).unwrap();
    let start = payload.start_bytes as usize;
    let partition = bytes[start..start + payload.size_bytes as usize].to_vec();
    let _ = fs::remove_file(&raw);
    (bytes, partition)
}

/// What the embedded `.bmaptar` decodes to (its zero tail, up to the
/// partition's end, past the tar's own end).
async fn decoded_payload(dir: &TempDir, partition: &[u8]) -> Vec<u8> {
    let bundle = dir.join("payload.wic.bmaptar");
    fs::write(&bundle, partition).unwrap();
    let raw = dir.join("payload.wic");
    convert()
        .to_raw(&bundle, &raw, &OperationContext::noop())
        .await
        .unwrap();
    fs::read(&raw).unwrap()
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk, a Linux-only dev tool"
)]
async fn packs_a_bmaptar_as_is_and_a_raw_image_bundled() {
    let dir = TempDir::new();
    let installer_bundle = installer_bmaptar(&dir).await;
    let product = product_wic(&dir);
    let product_bundle = dir.join("product.wic.bmaptar");
    convert()
        .from_raw(&product, &product_bundle, &OperationContext::noop())
        .await
        .unwrap();

    for (image, bundled) in [(&product_bundle, false), (&product, true)] {
        let output = dir.join("packed-installer.wic.bmaptar");
        let outcome = installer()
            .pack(
                &PackRequest {
                    installer: installer_bundle.clone(),
                    image: image.clone(),
                    output: output.clone(),
                },
                &OperationContext::noop(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.partition_index, 2);
        assert_eq!(outcome.bundled, bundled);
        assert!(outcome.partition_bytes >= outcome.payload_bytes);

        let (bytes, partition) = unpack(&dir, &output).await;
        assert!(bytes[ESP].iter().all(|&b| b == 0xE5), "the ESP is kept");
        assert_eq!(partition.len() as u64, outcome.partition_bytes);
        if !bundled {
            let given = fs::read(image).unwrap();
            assert_eq!(partition[..given.len()], given[..]);
        }
        assert_eq!(
            decoded_payload(&dir, &partition).await,
            fs::read(&product).unwrap()
        );

        // Nothing left behind but the output.
        let left: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".remora-installer-"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk, a Linux-only dev tool"
)]
async fn packs_an_mbr_installer_through_its_payload_type() {
    let dir = TempDir::new();
    // As remora-installer.bootmode-{rpi,uboot}.wks.in lay it out: a FAT
    // boot partition, then the payload, of type 0xDA.
    let raw = dir.join("installer.wic");
    sfdisk(
        &raw,
        8 * 1024 * 1024,
        "label: dos\nunit: sectors\n\n\
         start=2048, size=2048, type=c, bootable\n\
         start=4096, size=2048, type=da\n",
    );
    let mut bytes = fs::read(&raw).unwrap();
    bytes[ESP].fill(0xE5);
    fs::write(&raw, bytes).unwrap();
    let product = product_wic(&dir);

    let output = dir.join("packed-installer.wic");
    let outcome = installer()
        .pack(
            &PackRequest {
                installer: raw,
                image: product.clone(),
                output: output.clone(),
            },
            &OperationContext::noop(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.partition_index, 2);

    let (bytes, partition) = unpack(&dir, &output).await;
    assert!(
        bytes[ESP].iter().all(|&b| b == 0xE5),
        "the boot partition is kept"
    );
    assert_eq!(
        decoded_payload(&dir, &partition).await,
        fs::read(&product).unwrap()
    );
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk, a Linux-only dev tool"
)]
async fn refuses_a_bmaptar_that_is_not_a_tar() {
    let dir = TempDir::new();
    let fake = dir.join("product.wic.bmaptar");
    fs::write(&fake, vec![0u8; 4096]).unwrap();
    let installer_raw = dir.join("installer.wic");
    sfdisk(
        &installer_raw,
        8 * 1024 * 1024,
        "label: gpt\nunit: sectors\n\nstart=2048, size=2048, name=\"installer\"\n",
    );

    let err = installer()
        .pack(
            &PackRequest {
                installer: installer_raw,
                image: fake,
                output: dir.join("out.wic"),
            },
            &OperationContext::noop(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err.current_context(), Error::NotBmaptar(_)),
        "{err:?}"
    );
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk, a Linux-only dev tool"
)]
async fn refuses_an_installer_without_a_payload_partition() {
    let dir = TempDir::new();
    let not_an_installer = product_wic(&dir);
    let product_bundle = dir.join("product.wic.bmaptar");
    convert()
        .from_raw(
            &not_an_installer,
            &product_bundle,
            &OperationContext::noop(),
        )
        .await
        .unwrap();

    let err = installer()
        .pack(
            &PackRequest {
                installer: not_an_installer,
                image: product_bundle,
                output: dir.join("out.wic.bmaptar"),
            },
            &OperationContext::noop(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err.current_context(), Error::Embed), "{err:?}");
    assert!(!dir.join("out.wic.bmaptar").exists());
}
