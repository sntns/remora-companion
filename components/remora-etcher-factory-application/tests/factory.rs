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

/// A real credential set issued by `sntns-dev` for a throwaway test device
/// (`etcher-e2e-0002`), captured by the platform-side session that built
/// the gateway endpoint -- lets this test verify the image-writing path
/// against real platform output, not just made-up fixture bytes, and
/// separately verify that output is itself a well-formed, correctly
/// chained credential (the cross-check the platform session asked for).
mod real_captured_credential {
    pub const PRIVATE_KEY_DER_BASE64: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgRDmWuA7Vhh2feCB19f1cziWoEnt9ylaz7X7dVdKxp6yhRANCAASalLdwe6Y/6MEyJF/v8ZFaJQnMK01EMn24E0JEOgD4SbsBqinNHJPSkp2l7JYGc18SdXVhR79a869vBW18021p";
    pub const CERTIFICATE_DER_BASE64: &str = "MIIB9zCCAZygAwIBAgIRAK1AQ7vYxYqni769sqVGSEcwCgYIKoZIzj0EAwIwIzEhMB8GA1UEAxMYUmVtb3JhIEZhY3RvcnkgQXV0aG9yaXR5MB4XDTI2MDkxOTEzNTExOVoXDTM2MDgyODEwMTQyM1owGjEYMBYGA1UEAxMPZXRjaGVyLWUyZS0wMDAyMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEmpS3cHumP+jBMiRf7/GRWiUJzCtNRDJ9uBNCRDoA+Em7AaopzRyT0pKdpeyWBnNfEnV1YUe/WvOvbwVtfNNtaaOBuTCBtjAOBgNVHQ8BAf8EBAMCB4AwEwYDVR0lBAwwCgYIKwYBBQUHAwIwDAYDVR0TAQH/BAIwADAfBgNVHSMEGDAWgBTavJCbgtXPXB5qiKEbckDfoHtTYzBgBgNVHREEWTBXhlVsb2NhbDpyZW1vcmE6dGVzdDo1NDExNDRmYS04NjJhLTQyYmUtOGMyOC1jOGZlZTRmZmYyMTE6ZmFjdG9yeS1kZXZpY2U6ZXRjaGVyLWUyZS0wMDAyMAoGCCqGSM49BAMCA0kAMEYCIQDwat42oEkoYr8UP3098PcQmoMeUSN19sFrLaKqx202RQIhAKD9lUkGdAZTeo6sal7k09ZbQXgojlSH/Wkuy+Kv2ngG";
    pub const FACTORY_CA_DER_BASE64: &str = "MIIBhjCCASygAwIBAgIQS/N1W7Oq8pLhx31cFqzhLTAKBggqhkjOPQQDAjAjMSEwHwYDVQQDExhSZW1vcmEgRmFjdG9yeSBBdXRob3JpdHkwHhcNMjYwODMxMTAxNDIzWhcNMzYwODI4MTAxNDIzWjAjMSEwHwYDVQQDExhSZW1vcmEgRmFjdG9yeSBBdXRob3JpdHkwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAASo7CxfHieOeLpwISehevK5iQlpco/JrjzBUz4Hb/nEplOy+pHtMZBwiS88SFbfJ+OFZIvP3BAvbYJVC0lCiE1Ho0IwQDAOBgNVHQ8BAf8EBAMCAQYwDwYDVR0TAQH/BAUwAwEB/zAdBgNVHQ4EFgQU2ryQm4LVz1weaoihG3JA36B7U2MwCgYIKoZIzj0EAwIDSAAwRQIhAPnhL/Yc8pshLe+CDV6wDxp+3UQ/2CxZDw3WBHeYWurtAiB9u69m6rpOJjas/w8SqFKGiNN8x0SGLe5aGzounW0TpQ==";
    pub const SERVER_CA_DER_BASE64: &str = "MIIBhDCCASqgAwIBAgIQFoFk5k3bI45MlK4+jls7/DAKBggqhkjOPQQDAjAiMSAwHgYDVQQDExdSZW1vcmEgQWNjZXNzIEF1dGhvcml0eTAeFw0yNjA4MzExMDE0MjNaFw0zNjA4MjgxMDE0MjNaMCIxIDAeBgNVBAMTF1JlbW9yYSBBY2Nlc3MgQXV0aG9yaXR5MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE9/FRLEAsRymoDtbuM7OQ7qbV/ItnHZvXnpONk097aGO2MnvVoOZlZqVPASPCKjAZ75iSePlkxxWvqSCoch5VIaNCMEAwDgYDVR0PAQH/BAQDAgEGMA8GA1UdEwEB/wQFMAMBAf8wHQYDVR0OBBYEFPVHLpKuWW18JNSTWAQHBzRBGzz4MAoGCCqGSM49BAMCA0gAMEUCIBdUmn1hw4YtONRUF7+rhpzuwA7yuQovjSeJ5ma1lL1WAiEA1sdjPosKDyNhNFLE6QU7vaYKaKymMCPA8gV5RjM8Bpw=";
    pub const KEYID: &str =
        "local:kms:test:541144fa-862a-42be-8c28-c8fee4fff211:certificate:ad4043bbd8c58aa78bbebdb2a5464847";
    pub const DEVICE_NAME: &str = "etcher-e2e-0002";
    pub const EXPECTED_URI_SAN: &str =
        "local:remora:test:541144fa-862a-42be-8c28-c8fee4fff211:factory-device:etcher-e2e-0002";
}

struct RealCapturedProvisioning {
    certificate_der: Vec<u8>,
    certificate_authority_der: Vec<u8>,
    server_certificate_authority_der: Vec<u8>,
    keyid: String,
}

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for RealCapturedProvisioning {
    async fn provision(
        &self,
        _gateway_url: &str,
        _api_key: &str,
        device_name: &str,
        _csr_der: &[u8],
    ) -> remora_etcher_factory::adapter::Result<ProvisionedIdentity> {
        Ok(ProvisionedIdentity {
            factory_device_name: format!("local:remora:test:factory-device:{device_name}"),
            certificate_reference: CertificateReference {
                id: "captured".to_string(),
                urn: self.keyid.clone(),
                name: "captured".to_string(),
            },
            certificate_der: self.certificate_der.clone(),
            certificate_authority_der: self.certificate_authority_der.clone(),
            server_certificate_authority_der: self.server_certificate_authority_der.clone(),
        })
    }
}

/// Verifies the image-writing path against a *real* sntns-dev-issued
/// credential set (captured by the platform-side session), not fixture
/// bytes -- both that the bytes survive the write/read round trip exactly,
/// and that the credential itself is the well-formed, correctly chained
/// artifact the platform session verified when they issued it.
#[tokio::test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk/mke2fs, Linux-only dev tools"
)]
async fn writes_a_real_platform_issued_credential_set_correctly() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use real_captured_credential as fixture;

    let decode = |s: &str| STANDARD.decode(s).unwrap();
    let private_key_der = decode(fixture::PRIVATE_KEY_DER_BASE64);
    let certificate_der = decode(fixture::CERTIFICATE_DER_BASE64);
    let factory_ca_der = decode(fixture::FACTORY_CA_DER_BASE64);
    let server_ca_der = decode(fixture::SERVER_CA_DER_BASE64);

    // Sanity-check the captured data itself before trusting it as a test
    // oracle: the platform session's own claims about what it issued.
    let (_, cert) = x509_parser::parse_x509_certificate(&certificate_der)
        .expect("real captured certificate must parse");
    let (_, factory_ca) = x509_parser::parse_x509_certificate(&factory_ca_der)
        .expect("real captured factory CA must parse");
    let (_, server_ca) = x509_parser::parse_x509_certificate(&server_ca_der)
        .expect("real captured server CA must parse");

    assert!(
        cert.verify_signature(Some(factory_ca.public_key())).is_ok(),
        "certificate must chain to the factory CA, not some other key"
    );
    assert_ne!(
        factory_ca.public_key().subject_public_key.data,
        server_ca.public_key().subject_public_key.data,
        "factory CA and server CA must be two different keys/authorities"
    );

    let key_pair = rcgen::KeyPair::try_from(private_key_der.as_slice())
        .expect("real captured private key must parse as PKCS#8 EC");
    assert_eq!(
        cert.public_key().subject_public_key.data.as_ref(),
        key_pair.public_key_raw(),
        "the certificate's public key must match the private key it was issued for"
    );

    let eku = cert
        .extended_key_usage()
        .unwrap()
        .expect("certificate must carry an extended key usage extension");
    assert!(
        eku.value.client_auth,
        "IDevID must be usable for client auth"
    );

    let basic_constraints = cert
        .basic_constraints()
        .unwrap()
        .expect("certificate must carry a basic constraints extension");
    assert!(
        !basic_constraints.value.ca,
        "a leaf IDevID must not be CA:TRUE"
    );

    let san = cert
        .subject_alternative_name()
        .unwrap()
        .expect("certificate must carry a URI SAN");
    let uri = san
        .value
        .general_names
        .iter()
        .find_map(|name| match name {
            x509_parser::extensions::GeneralName::URI(uri) => Some(*uri),
            _ => None,
        })
        .expect("certificate must carry a URI SAN entry");
    assert_eq!(uri, fixture::EXPECTED_URI_SAN);

    // Now the part that's actually this crate's own responsibility: does
    // `provision` write these exact bytes into the image without
    // mangling them anywhere along the way.
    let disk = build_raw_disk_with_shared_partition();
    let image = ImageService::new(ImageControllerImpl::new(
        PartitionTableAdapterService::new(PartitionTableAdapterImpl),
        Arc::new(Ext4AdapterImpl),
        Arc::new(VfatAdapterImpl),
        FsWalkAdapterService::new(remora_etcher_fs_walk::FsWalkAdapterImpl),
    ));
    let provisioning = RealCapturedProvisioning {
        certificate_der: certificate_der.clone(),
        certificate_authority_der: factory_ca_der.clone(),
        server_certificate_authority_der: server_ca_der.clone(),
        keyid: fixture::KEYID.to_string(),
    };
    let controller =
        FactoryControllerImpl::new(FactoryProvisioningAdapterService::new(provisioning), image);

    controller
        .provision(
            fixture::DEVICE_NAME,
            "https://remora.access.sntns.dev/access/v1",
            "https://api.example.invalid",
            "unused",
            &disk,
            &OperationContext::noop(),
        )
        .await
        .expect("provisioning against the real captured data should succeed");

    assert_eq!(
        read_shared_partition_file(&disk, "/remora/factory/certificate.der"),
        certificate_der
    );
    assert_eq!(
        read_shared_partition_file(&disk, "/remora/factory/factory_ca.der"),
        factory_ca_der
    );
    assert_eq!(
        read_shared_partition_file(&disk, "/remora/factory/server_ca.der"),
        server_ca_der
    );
    assert_eq!(
        read_shared_partition_file(&disk, "/remora/factory/keyid").as_slice(),
        fixture::KEYID.as_bytes()
    );
    assert_eq!(
        read_shared_partition_file(&disk, "/remora/factory/access_url").as_slice(),
        b"https://remora.access.sntns.dev/access/v1"
    );

    // The private key remora-etcher writes is one it generated itself
    // (never the platform's), so it won't equal `private_key_der` -- but
    // it must still be present, non-empty, and load-bearing as a real key.
    let written_key = read_shared_partition_file(&disk, "/remora/factory/private_key.der");
    assert!(rcgen::KeyPair::try_from(written_key.as_slice()).is_ok());

    let _ = fs::remove_file(&disk);
}
