//! End-to-end exercise of `FactoryServiceInterface::provision`, using a fake
//! `FactoryProvisioningAdapter` (no network call -- see its doc comment) and
//! the real local adapters (`remora-factory-adapter-local`: keypair/CSR,
//! `remora-factory.yaml`), whose own tests cover the file's exact layout and
//! a real captured credential set. Bundling the resulting file into an
//! image is `identity create`'s job, not this vertical's -- see
//! `FactoryServiceInterface`'s own doc comment -- so these tests stop at
//! "is the written file correct", not "is it injected somewhere".

use std::{
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use remora_context::{application::ContextService, model::ResolvedContext};
use remora_context_application::ContextControllerImpl;
use remora_factory::{
    adapter::{
        credential::CredentialWriterAdapterService,
        key::DeviceKeyAdapterService,
        provisioning::{
            self, FactoryProvisioningAdapter, FactoryProvisioningAdapterService,
            ProvisionedIdentity,
        },
    },
    application::FactoryServiceInterface,
    model::DeviceSerial,
};
use remora_factory_adapter_local::{CredentialWriterAdapterImpl, DeviceKeyAdapterImpl};
use remora_factory_application::FactoryControllerImpl;
use remora_progress::OperationContext;

/// Stands in for the real gateway call (see
/// `remora-factory-adapter-grpc`, gated on a platform endpoint
/// this test has no business depending on) -- returns a fixed, made-up but
/// correctly-shaped response, so this test only exercises this crate's own
/// logic (keygen/CSR/rendering the yaml).
struct FakeProvisioning;

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for FakeProvisioning {
    async fn provision(
        &self,
        _context: &ResolvedContext,
        serial: &DeviceSerial,
        _csr_der: &[u8],
    ) -> provisioning::Result<ProvisionedIdentity> {
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

/// The serial a fake adapter "issues": the explicit name as-is, or a
/// made-up allocation standing in for the platform's policy.
fn serial_of(serial: &DeviceSerial) -> String {
    match serial {
        DeviceSerial::Explicit { device_name, .. } => device_name.clone(),
        DeviceSerial::FromPolicy(policy) => format!("{policy}-1H7Z"),
    }
}

fn temp_path(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "remora-factory-test-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path
}

fn controller(provisioning: impl FactoryProvisioningAdapter + 'static) -> FactoryControllerImpl {
    FactoryControllerImpl::new(
        logged_in_contexts(),
        DeviceKeyAdapterService::new(DeviceKeyAdapterImpl),
        FactoryProvisioningAdapterService::new(provisioning),
        CredentialWriterAdapterService::new(CredentialWriterAdapterImpl),
    )
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

/// Pulls the dedented body of a `key: |` block scalar out of a rendered
/// `remora-factory.yaml`, matching exactly the shape `yaml::render` writes
/// (2-space indent, one block per key, block ends at the first
/// non-indented line).
fn extract_block(yaml: &str, key: &str) -> String {
    let marker = format!("{key}: |\n");
    let start = yaml
        .find(&marker)
        .unwrap_or_else(|| panic!("{key} block not found in:\n{yaml}"))
        + marker.len();
    let mut body = String::new();
    for line in yaml[start..].lines() {
        match line.strip_prefix("  ") {
            Some(dedented) => {
                body.push_str(dedented);
                body.push('\n');
            }
            None => break,
        }
    }
    body
}

fn extract_scalar(yaml: &str, key: &str) -> String {
    yaml.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}: ")))
        .unwrap_or_else(|| panic!("{key} scalar not found in:\n{yaml}"))
        .trim_matches('"')
        .to_string()
}

#[tokio::test]
async fn provisions_a_device_and_renders_a_well_formed_yaml() {
    let output = temp_path("remora-factory.yaml");

    controller(FakeProvisioning)
        .provision(
            None,
            &DeviceSerial::Explicit {
                device_name: "e2e-serial-0001".to_string(),
                force: false,
            },
            None,
            &output,
            &OperationContext::noop(),
        )
        .await
        .expect("provisioning should succeed against the fake adapter");

    let yaml = fs::read_to_string(&output).expect("output file should have been written");
    let _ = fs::remove_file(&output);

    assert_eq!(
        extract_scalar(&yaml, "url"),
        "https://remora.access.eu2.sntns.io/access/v1"
    );
    assert_eq!(
        extract_scalar(&yaml, "key-id"),
        "test:kms:certificate:e2e-serial-0001"
    );

    let key_pem = extract_block(&yaml, "key");
    assert!(
        key_pem.starts_with("-----BEGIN EC PRIVATE KEY-----"),
        "private key must be rendered as SEC1 PEM, not PKCS#8: {key_pem}"
    );
    p256::SecretKey::from_sec1_pem(&key_pem).expect("rendered key must parse as a SEC1 EC key");

    let certificate_pem = extract_block(&yaml, "certificate");
    assert_eq!(
        pem::parse(&certificate_pem).unwrap().contents(),
        b"fake-certificate-der"
    );
    let authority_pem = extract_block(&yaml, "authority");
    assert_eq!(
        pem::parse(&authority_pem).unwrap().contents(),
        b"fake-factory-ca-der"
    );
    let server_authority_pem = extract_block(&yaml, "server-authority");
    assert_eq!(
        pem::parse(&server_authority_pem).unwrap().contents(),
        b"fake-server-ca-der"
    );
}

struct EmptyAccessUrlProvisioning;

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for EmptyAccessUrlProvisioning {
    async fn provision(
        &self,
        _context: &ResolvedContext,
        serial: &DeviceSerial,
        _csr_der: &[u8],
    ) -> provisioning::Result<ProvisionedIdentity> {
        let device_name = serial_of(serial);
        Ok(ProvisionedIdentity {
            serial_number: device_name.clone(),
            factory_device_name: format!("test:remora:factory-device:{device_name}"),
            certificate_der: b"fake-certificate-der".to_vec(),
            certificate_authority_der: b"fake-factory-ca-der".to_vec(),
            server_certificate_authority_der: b"fake-server-ca-der".to_vec(),
            key_id: format!("test:kms:certificate:{device_name}"),
            access_url: String::new(),
        })
    }
}

#[tokio::test]
async fn fails_when_the_platform_omits_an_access_url_and_no_override_was_given() {
    let output = temp_path("remora-factory.yaml");

    let result = controller(EmptyAccessUrlProvisioning)
        .provision(
            None,
            &DeviceSerial::Explicit {
                device_name: "e2e-serial-0003".to_string(),
                force: false,
            },
            None,
            &output,
            &OperationContext::noop(),
        )
        .await;

    assert!(
        result.is_err(),
        "an empty access url with no override must be a hard failure, not a silently empty field"
    );
    assert!(!output.exists());
}

#[tokio::test]
async fn honors_the_access_url_override_when_the_platform_response_is_empty() {
    let output = temp_path("remora-factory.yaml");

    controller(EmptyAccessUrlProvisioning)
        .provision(
            None,
            &DeviceSerial::Explicit {
                device_name: "e2e-serial-0004".to_string(),
                force: false,
            },
            Some("https://override.example.invalid/access/v1"),
            &output,
            &OperationContext::noop(),
        )
        .await
        .expect("the override should be used when the platform response is empty");

    let yaml = fs::read_to_string(&output).unwrap();
    let _ = fs::remove_file(&output);
    assert_eq!(
        extract_scalar(&yaml, "url"),
        "https://override.example.invalid/access/v1"
    );
}

/// Records the `DeviceSerial` the controller hands the adapter, so the
/// policy/explicit/force choice can be asserted as what reaches the wire.
struct RecordingProvisioning {
    seen: Arc<Mutex<Vec<DeviceSerial>>>,
}

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for RecordingProvisioning {
    async fn provision(
        &self,
        _context: &ResolvedContext,
        serial: &DeviceSerial,
        _csr_der: &[u8],
    ) -> provisioning::Result<ProvisionedIdentity> {
        self.seen.lock().unwrap().push(serial.clone());
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

async fn provision_recording(serial: DeviceSerial) -> (Vec<DeviceSerial>, String) {
    let output = temp_path("remora-factory.yaml");
    let seen = Arc::new(Mutex::new(Vec::new()));

    let device = controller(RecordingProvisioning { seen: seen.clone() })
        .provision(None, &serial, None, &output, &OperationContext::noop())
        .await
        .expect("provisioning should succeed against the fake adapter");

    let _ = fs::remove_file(&output);
    let seen = seen.lock().unwrap().clone();
    (seen, device.serial_number)
}

#[tokio::test]
async fn force_reaches_the_adapter_with_the_explicit_device_name() {
    let serial = DeviceSerial::Explicit {
        device_name: "e2e-serial-0005".to_string(),
        force: true,
    };
    let (seen, issued) = provision_recording(serial.clone()).await;
    assert_eq!(seen, vec![serial]);
    assert_eq!(issued, "e2e-serial-0005");
}

/// A policy-allocated serial only exists once the platform answers -- the
/// controller must hand back the one the platform issued (for the label),
/// not anything it made up itself.
#[tokio::test]
async fn a_policy_request_returns_the_platform_allocated_serial() {
    let (seen, issued) = provision_recording(DeviceSerial::FromPolicy("hubs".to_string())).await;
    assert_eq!(seen, vec![DeviceSerial::FromPolicy("hubs".to_string())]);
    assert_eq!(issued, "hubs-1H7Z");
}

#[test]
fn from_parts_rejects_what_the_platform_would() {
    use remora_factory::model::DeviceSerialError;
    let name = || Some("1H7Z".to_string());
    let policy = || Some("hubs".to_string());

    assert_eq!(
        DeviceSerial::from_parts(name(), policy(), false),
        Err(DeviceSerialError::Both)
    );
    assert_eq!(
        DeviceSerial::from_parts(None, None, false),
        Err(DeviceSerialError::Neither)
    );
    assert_eq!(
        DeviceSerial::from_parts(None, policy(), true),
        Err(DeviceSerialError::ForceWithPolicy)
    );
    assert_eq!(
        DeviceSerial::from_parts(None, policy(), false),
        Ok(DeviceSerial::FromPolicy("hubs".to_string()))
    );
    assert_eq!(
        DeviceSerial::from_parts(name(), None, true),
        Ok(DeviceSerial::Explicit {
            device_name: "1H7Z".to_string(),
            force: true
        })
    );
}

/// A device's own CSR is checked before the platform hears of it: a bad
/// one costs no serial.
#[tokio::test]
async fn provision_csr_refuses_a_bad_csr_without_calling_the_platform() {
    use remora_factory::application::Error;

    let seen = Arc::new(Mutex::new(Vec::new()));
    let report = controller(RecordingProvisioning { seen: seen.clone() })
        .provision_csr(None, &DeviceSerial::FromPolicy("hubs".into()), b"garbage")
        .await
        .unwrap_err();
    assert!(matches!(report.current_context(), Error::InvalidCsr));
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn provision_csr_returns_the_identity_for_a_device_held_key() {
    use remora_factory::adapter::key::DeviceKeyAdapter;

    // The device's key, generated "on the device": only its CSR travels.
    let key = DeviceKeyAdapterImpl.generate(None).await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let identity = controller(RecordingProvisioning { seen: seen.clone() })
        .provision_csr(None, &DeviceSerial::FromPolicy("hubs".into()), &key.csr_der)
        .await
        .unwrap();
    assert_eq!(identity.serial_number, "hubs-1H7Z");
    assert_eq!(
        identity.access_url,
        "https://remora.access.eu2.sntns.io/access/v1"
    );
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn provision_csr_has_no_override_for_an_empty_access_url() {
    use remora_factory::{adapter::key::DeviceKeyAdapter, application::Error};

    let key = DeviceKeyAdapterImpl.generate(None).await.unwrap();
    let report = controller(EmptyAccessUrlProvisioning)
        .provision_csr(None, &DeviceSerial::FromPolicy("hubs".into()), &key.csr_der)
        .await
        .unwrap_err();
    assert!(matches!(report.current_context(), Error::MissingAccessUrl));
}

/// A device keeping its own key (a hub claiming from a station): its key
/// made here, its identity issued for the CSR elsewhere, its
/// `remora-factory.yaml` written exactly as `provision` writes one.
#[tokio::test]
async fn a_device_held_key_and_an_identity_issued_elsewhere_make_its_credential() {
    let factory = controller(FakeProvisioning);
    let key = factory.generate_device_key().await.unwrap();
    let identity = factory
        .provision_csr(None, &DeviceSerial::FromPolicy("hubs".into()), &key.csr_der)
        .await
        .unwrap();
    let output = temp_path("claimed");
    factory
        .write_credential(key.private_key, identity, &output)
        .await
        .unwrap();
    let yaml = fs::read_to_string(&output).unwrap();
    let _ = fs::remove_file(&output);
    assert!(
        yaml.contains("key-id: \"test:kms:certificate:hubs-1H7Z\""),
        "{yaml}"
    );
    assert!(yaml.contains("BEGIN EC PRIVATE KEY"), "{yaml}");
}
