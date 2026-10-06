//! End-to-end exercise of the batch vertical, reproducing the motivating
//! example: convert a packaged (gzip) image to raw, provision it (an
//! identity, a config file, and a directly-injected file), then convert it
//! back to its original packaged format — as one `BatchService::run` call
//! instead of five separate ones.
//!
//! Disk fixture built the same way as `remora-config-application`'s
//! `tests/config_upload.rs`: a real MBR partition table (`sfdisk`) plus a
//! real ext4 `shared` partition (`mke2fs`) — dev-only tools, never shelled
//! out to by the shipped binary.

use std::{
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use remora_batch::{application::BatchServiceInterface, model::BatchStep};
use remora_batch_application::BatchControllerImpl;
use remora_config::application::ConfigService;
use remora_config_application::ConfigControllerImpl;
use remora_context::application::ContextService;
use remora_context_application::ContextControllerImpl;
use remora_convert::{adapter::ContainerFormatAdapter, application::ConvertService};
use remora_convert_adapter_gzip::GzipAdapterImpl;
use remora_convert_adapter_qcow2::Qcow2AdapterImpl;
use remora_convert_application::ConvertControllerImpl;
use remora_factory::{
    adapter::{FactoryProvisioningAdapter, FactoryProvisioningAdapterService, ProvisionedIdentity},
    application::FactoryService,
    model::DeviceSerial,
};
use remora_factory_application::FactoryControllerImpl;
use remora_fs_walk::FsWalkAdapterService;
use remora_identity::application::IdentityService;
use remora_identity_adapter_keygen::KeygenAdapterImpl;
use remora_identity_application::IdentityControllerImpl;
use remora_image::{
    adapter::{
        ext4::{Ext4Adapter, Ext4AdapterService},
        partition_table::PartitionTableAdapterService,
    },
    application::ImageService,
    model::{InjectRequest, PartitionSelector},
};
use remora_image_adapter_ext4::Ext4AdapterImpl;
use remora_image_adapter_partition_table::PartitionTableAdapterImpl;
use remora_image_adapter_vfat::VfatAdapterImpl;
use remora_image_application::ImageControllerImpl;
use remora_progress::OperationContext;
use remora_squashfs::application::SquashfsService;
use remora_squashfs_adapter_backhand::SquashfsAdapterImpl;
use remora_squashfs_application::SquashfsControllerImpl;

const PARTITION_OFFSET: u64 = 2048 * 512;
const PARTITION_SIZE: u64 = 32 * 1024 * 1024;
const DISK_SIZE: u64 = 48 * 1024 * 1024;
const SFDISK_PARTITION_SECTORS: u64 = PARTITION_SIZE / 512;

fn temp_path(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "remora-batch-test-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path
}

/// A raw disk image with a single MBR partition (index 1 ==
/// `PartitionRole::Shared`) at `PARTITION_OFFSET`, formatted ext4.
fn build_raw_disk_with_shared_partition() -> PathBuf {
    let disk = temp_path("disk-raw");
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

/// Stands in for the real gateway call -- none of these tests exercise a
/// `FactoryProvision` step that actually reaches it, this only satisfies
/// `BatchControllerImpl::new`'s dependency on a wired `FactoryService`,
/// same as every other vertical's `controller()` helper below.
struct UnusedProvisioning;

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for UnusedProvisioning {
    async fn provision(
        &self,
        _context: &remora_context::model::ResolvedContext,
        _serial: &DeviceSerial,
        _csr_der: &[u8],
    ) -> remora_factory::adapter::Result<ProvisionedIdentity> {
        unreachable!("no test in this file exercises a FactoryProvision step")
    }
}

/// A context service with one context, `eu2`, logged in: the factory
/// resolves it before calling the (fake) platform. Its directory is left
/// behind on purpose -- the service outlives any one scope here.
fn logged_in_contexts() -> ContextService {
    use remora_context::{
        adapter::{
            credentials::{CredentialStoreAdapter, CredentialStoreAdapterService},
            platform::{self, PlatformSessionAdapter, PlatformSessionAdapterService},
            store::{ContextStoreAdapter, ContextStoreAdapterService},
        },
        model::{Context, Credentials, Endpoint, Principal, Secret, Tls},
    };
    use remora_context_adapter_file::{FileContextStoreImpl, FileCredentialStoreImpl};

    /// Resolving a context never calls its platform.
    struct NoPlatform;

    #[async_trait::async_trait]
    impl PlatformSessionAdapter for NoPlatform {
        async fn whoami(&self, _: &Context, _: &Credentials) -> platform::Result<Principal> {
            unreachable!("resolving a context does not log in")
        }
        async fn acting_account(
            &self,
            _: &Context,
            _: &Credentials,
            _: &str,
        ) -> platform::Result<Option<String>> {
            unreachable!("resolving a context does not log in")
        }
    }

    let root = tempfile::tempdir().unwrap().keep();
    let store = FileContextStoreImpl::new(&root);
    store
        .put(&Context {
            name: "eu2".into(),
            description: None,
            endpoint: Endpoint {
                address: "api.example.invalid:50051".into(),
                tls: Tls::default(),
            },
            roles: Default::default(),
            assumed_role: None,
            login: None,
        })
        .unwrap();
    let credentials = FileCredentialStoreImpl::new(&root);
    credentials
        .put(
            "eu2",
            &Credentials {
                secret: Secret::AccessKey {
                    token: "unused-in-the-fake".into(),
                },
            },
        )
        .unwrap();
    ContextService::new(ContextControllerImpl::new(
        ContextStoreAdapterService::new(store),
        CredentialStoreAdapterService::new(credentials),
        PlatformSessionAdapterService::new(NoPlatform),
    ))
}

fn read_shared_partition_file(disk: &Path, dest_path: &str) -> Vec<u8> {
    Ext4AdapterImpl
        .read_file(disk, PARTITION_OFFSET, PARTITION_SIZE, dest_path)
        .unwrap()
}

fn controller() -> BatchControllerImpl {
    let convert = ConvertService::new(ConvertControllerImpl::new(
        Arc::new(Qcow2AdapterImpl),
        Arc::new(GzipAdapterImpl),
    ));

    let image = ImageService::new(ImageControllerImpl::new(
        PartitionTableAdapterService::new(PartitionTableAdapterImpl),
        Arc::new(Ext4AdapterImpl),
        Arc::new(VfatAdapterImpl),
        FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
        Ext4AdapterService::new(Ext4AdapterImpl),
    ));

    let identity = IdentityService::new(IdentityControllerImpl::new(
        remora_identity::adapter::KeygenAdapterService::new(KeygenAdapterImpl),
        SquashfsService::new(SquashfsControllerImpl::new(
            FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
            remora_squashfs::adapter::SquashfsAdapterService::new(SquashfsAdapterImpl),
        )),
        image.clone(),
    ));

    let config = ConfigService::new(ConfigControllerImpl::new(
        FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
        image.clone(),
    ));

    let squashfs = SquashfsService::new(SquashfsControllerImpl::new(
        FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
        remora_squashfs::adapter::SquashfsAdapterService::new(SquashfsAdapterImpl),
    ));

    let factory = FactoryService::new(FactoryControllerImpl::new(
        logged_in_contexts(),
        FactoryProvisioningAdapterService::new(UnusedProvisioning),
    ));

    BatchControllerImpl::new(convert, identity, config, image, squashfs, factory)
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk/mke2fs, Linux-only dev tools"
)]
async fn runs_the_convert_provision_convert_back_recipe() {
    let raw = build_raw_disk_with_shared_partition();
    let packaged = temp_path("packaged").with_extension("gz");
    GzipAdapterImpl.encode_from_raw(&raw, &packaged).unwrap();

    let identity_inputs = temp_path("identity-inputs");
    fs::create_dir_all(&identity_inputs).unwrap();
    fs::write(identity_inputs.join("hostname"), b"my-device\n").unwrap();

    let config_source = temp_path("config-source");
    fs::write(&config_source, b"Europe/Paris\n").unwrap();

    let inject_source = temp_path("inject-source");
    fs::write(&inject_source, b"hello from batch\n").unwrap();

    let working_raw = temp_path("working.raw");
    let final_packaged = temp_path("final").with_extension("gz");

    let steps = vec![
        BatchStep::ConvertToRaw {
            image: packaged.clone(),
            output: working_raw.clone(),
        },
        BatchStep::IdentityCreate {
            inputs: vec![identity_inputs.clone()],
            image: working_raw.clone(),
            hostname: None,
            machine_id: None,
        },
        BatchStep::ConfigUpload {
            image: working_raw.clone(),
            source: config_source.clone(),
            dest_relative_path: "/timezone".to_string(),
            slot: "slot-A".to_string(),
            mode: 0o644,
        },
        BatchStep::ImageInject(InjectRequest {
            image: working_raw.clone(),
            source: inject_source.clone(),
            dest_path: "/hello.txt".to_string(),
            partition: PartitionSelector::Index(1),
            boot_mode: None,
            mode: 0o644,
        }),
        BatchStep::ConvertFromRaw {
            raw_image: working_raw.clone(),
            output: final_packaged.clone(),
        },
    ];

    controller()
        .run(steps, None, &OperationContext::noop())
        .await
        .expect("batch should succeed");

    // Every step actually landed on the intermediate raw image.
    assert_eq!(
        read_shared_partition_file(&working_raw, "/hello.txt"),
        b"hello from batch\n"
    );
    assert!(!read_shared_partition_file(&working_raw, "/remora/identity").is_empty());
    let config_bytes = read_shared_partition_file(&working_raw, "/remora/slot-A/config");
    assert!(!config_bytes.is_empty());

    // The final convert-back step produced a real gzip round-tripping back
    // to the exact same bytes as the (now fully provisioned) raw image.
    assert!(final_packaged.exists());
    let round_tripped = temp_path("round-tripped.raw");
    GzipAdapterImpl
        .decode_to_raw(&final_packaged, &round_tripped)
        .unwrap();
    assert_eq!(
        fs::read(&working_raw).unwrap(),
        fs::read(&round_tripped).unwrap()
    );

    for p in [
        raw,
        packaged,
        identity_inputs,
        config_source,
        inject_source,
        working_raw,
        final_packaged,
        round_tripped,
    ] {
        let _ = fs::remove_file(&p);
        let _ = fs::remove_dir_all(&p);
    }
}

#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk/mke2fs, Linux-only dev tools"
)]
async fn stops_at_the_first_failing_step_without_running_the_rest() {
    let raw = build_raw_disk_with_shared_partition();
    let packaged = temp_path("packaged2").with_extension("gz");
    GzipAdapterImpl.encode_from_raw(&raw, &packaged).unwrap();

    let inject_source = temp_path("inject-source-2");
    fs::write(&inject_source, b"unreachable\n").unwrap();

    let working_raw = temp_path("working2.raw");
    let final_packaged = temp_path("final2").with_extension("gz");

    let steps = vec![
        BatchStep::ConvertToRaw {
            image: packaged.clone(),
            output: working_raw.clone(),
        },
        // Fails: `inject` never creates intermediate directories, and
        // `/does/not/exist` isn't one.
        BatchStep::ImageInject(InjectRequest {
            image: working_raw.clone(),
            source: inject_source.clone(),
            dest_path: "/does/not/exist/hello.txt".to_string(),
            partition: PartitionSelector::Index(1),
            boot_mode: None,
            mode: 0o644,
        }),
        // Must never run.
        BatchStep::ConvertFromRaw {
            raw_image: working_raw.clone(),
            output: final_packaged.clone(),
        },
    ];

    let err = controller()
        .run(steps, None, &OperationContext::noop())
        .await
        .expect_err("the second step should fail");
    let message = format!("{err:?}");
    assert!(
        message.contains("step 1"),
        "expected the error to name the failing step's index, got: {message}"
    );

    assert!(
        !final_packaged.exists(),
        "the convert-back step must not have run after an earlier step failed"
    );

    for p in [raw, packaged, inject_source, working_raw, final_packaged] {
        let _ = fs::remove_file(&p);
    }
}

/// The serial a fake adapter "issues": the explicit name as-is, or a
/// made-up allocation standing in for the platform's policy.
fn serial_of(serial: &DeviceSerial) -> String {
    match serial {
        DeviceSerial::Explicit { device_name, .. } => device_name.clone(),
        DeviceSerial::FromPolicy(policy) => format!("{policy}-1H7Z"),
    }
}

struct FakeProvisioning;

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for FakeProvisioning {
    async fn provision(
        &self,
        _context: &remora_context::model::ResolvedContext,
        serial: &DeviceSerial,
        _csr_der: &[u8],
    ) -> remora_factory::adapter::Result<ProvisionedIdentity> {
        let device_name = serial_of(serial);
        Ok(ProvisionedIdentity {
            serial_number: device_name.clone(),
            factory_device_name: format!("test:remora:factory-device:{device_name}"),
            certificate_der: b"fake-certificate-der".to_vec(),
            certificate_authority_der: b"fake-factory-ca-der".to_vec(),
            server_certificate_authority_der: b"fake-server-ca-der".to_vec(),
            key_id: format!("test:kms:certificate:{device_name}"),
            access_url: "https://remora.access.eu2.sntns.io/access/v1".to_string(),
        })
    }
}

/// `FactoryProvision` is the one step that isn't locally reproducible (a
/// real run calls out to sntns-platform) -- exercised here with a fake
/// adapter standing in for the network call, same posture as
/// `remora-factory-application`'s own tests, to prove it dispatches
/// correctly as a batch step and produces the same `remora-factory.yaml` a
/// standalone `factory provision` invocation would.
#[tokio::test]
async fn runs_a_factory_provision_step() {
    let convert = ConvertService::new(ConvertControllerImpl::new(
        Arc::new(Qcow2AdapterImpl),
        Arc::new(GzipAdapterImpl),
    ));
    let image = ImageService::new(ImageControllerImpl::new(
        PartitionTableAdapterService::new(PartitionTableAdapterImpl),
        Arc::new(Ext4AdapterImpl),
        Arc::new(VfatAdapterImpl),
        FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
        Ext4AdapterService::new(Ext4AdapterImpl),
    ));
    let identity = IdentityService::new(IdentityControllerImpl::new(
        remora_identity::adapter::KeygenAdapterService::new(KeygenAdapterImpl),
        SquashfsService::new(SquashfsControllerImpl::new(
            FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
            remora_squashfs::adapter::SquashfsAdapterService::new(SquashfsAdapterImpl),
        )),
        image.clone(),
    ));
    let config = ConfigService::new(ConfigControllerImpl::new(
        FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
        image.clone(),
    ));
    let squashfs = SquashfsService::new(SquashfsControllerImpl::new(
        FsWalkAdapterService::new(remora_fs_walk::FsWalkAdapterImpl),
        remora_squashfs::adapter::SquashfsAdapterService::new(SquashfsAdapterImpl),
    ));
    let factory = FactoryService::new(FactoryControllerImpl::new(
        logged_in_contexts(),
        FactoryProvisioningAdapterService::new(FakeProvisioning),
    ));
    let controller = BatchControllerImpl::new(convert, identity, config, image, squashfs, factory);

    let output = temp_path("remora-factory.yaml");
    controller
        .run(
            vec![BatchStep::FactoryProvision {
                device_name: Some("batch-e2e-0001".to_string()),
                serial_number_policy: None,
                context: None,
                access_url: None,
                force: false,
                output: output.clone(),
            }],
            None,
            &OperationContext::noop(),
        )
        .await
        .expect("the factory-provision step should succeed against the fake adapter");

    let yaml = fs::read_to_string(&output).expect("the step should have written the yaml");
    let _ = fs::remove_file(&output);
    assert!(yaml.contains("key-id: \"test:kms:certificate:batch-e2e-0001\""));
    assert!(yaml.starts_with("url: https://remora.access.eu2.sntns.io/access/v1\n"));
}
