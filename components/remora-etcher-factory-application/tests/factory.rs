//! End-to-end exercise of `FactoryServiceInterface::provision`, using a fake
//! `FactoryProvisioningAdapter` (no network call -- see its doc comment) so
//! the test only depends on this crate's own logic: local keypair/CSR
//! generation and rendering `remora-factory.yaml`. Bundling the resulting
//! file into an image is `identity create`'s job, not this vertical's --
//! see `FactoryServiceInterface`'s own doc comment -- so these tests stop at
//! "is the rendered file correct", not "is it injected somewhere".

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use remora_etcher_factory::{
    adapter::{FactoryProvisioningAdapter, FactoryProvisioningAdapterService, ProvisionedIdentity},
    application::FactoryServiceInterface,
};
use remora_etcher_factory_application::FactoryControllerImpl;
use remora_etcher_progress::OperationContext;

/// Stands in for the real gateway call (see
/// `remora-etcher-factory-adapter-gateway`, gated on a platform endpoint
/// this test has no business depending on) -- returns a fixed, made-up but
/// correctly-shaped response, so this test only exercises this crate's own
/// logic (keygen/CSR/rendering the yaml).
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
            certificate_der: b"fake-certificate-der".to_vec(),
            certificate_authority_der: b"fake-factory-ca-der".to_vec(),
            server_certificate_authority_der: b"fake-server-ca-der".to_vec(),
            key_id: format!("test:kms:certificate:{device_name}"),
            access_url: "https://remora.access.eu2.sntns.io/access/v1".to_string(),
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

fn controller(provisioning: impl FactoryProvisioningAdapter + 'static) -> FactoryControllerImpl {
    FactoryControllerImpl::new(FactoryProvisioningAdapterService::new(provisioning))
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
            "e2e-serial-0001",
            "https://api.example.invalid",
            "unused-in-the-fake",
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

    // The two keys the platform-side session flagged as easy to get wrong
    // (underscore vs hyphen) -- assert the literal on-disk spelling, not
    // just that *some* value round-trips, so a regression here fails this
    // test instead of silently producing a yaml remora-edge's parser
    // either rejects outright (`key-id`, no serde default) or silently
    // drops (`server-authority`, `Option` with `#[serde(default)]`).
    assert!(
        yaml.contains("key-id: \""),
        "missing/misspelled key-id:\n{yaml}"
    );
    assert!(
        yaml.contains("server-authority: |\n"),
        "missing/misspelled server-authority:\n{yaml}"
    );
    assert!(
        !yaml.contains("key_id"),
        "key-id must be hyphenated, not underscored:\n{yaml}"
    );
    assert!(
        !yaml.contains("server_authority"),
        "server-authority must be hyphenated, not underscored:\n{yaml}"
    );
}

struct EmptyAccessUrlProvisioning;

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for EmptyAccessUrlProvisioning {
    async fn provision(
        &self,
        _gateway_url: &str,
        _api_key: &str,
        device_name: &str,
        _csr_der: &[u8],
    ) -> remora_etcher_factory::adapter::Result<ProvisionedIdentity> {
        Ok(ProvisionedIdentity {
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
            "e2e-serial-0003",
            "https://api.example.invalid",
            "unused-in-the-fake",
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
            "e2e-serial-0004",
            "https://api.example.invalid",
            "unused-in-the-fake",
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

/// A real credential set issued by `sntns-dev` for a throwaway test device
/// (`etcher-e2e-0002`), captured by the platform-side session that built
/// the gateway endpoint -- lets this test verify the rendering path against
/// real platform output, not just made-up fixture bytes, and separately
/// verify that output is itself a well-formed, correctly chained credential
/// (the cross-check the platform session asked for).
mod real_captured_credential {
    pub const CERTIFICATE_DER_BASE64: &str = "MIIB9zCCAZygAwIBAgIRAK1AQ7vYxYqni769sqVGSEcwCgYIKoZIzj0EAwIwIzEhMB8GA1UEAxMYUmVtb3JhIEZhY3RvcnkgQXV0aG9yaXR5MB4XDTI2MDkxOTEzNTExOVoXDTM2MDgyODEwMTQyM1owGjEYMBYGA1UEAxMPZXRjaGVyLWUyZS0wMDAyMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEmpS3cHumP+jBMiRf7/GRWiUJzCtNRDJ9uBNCRDoA+Em7AaopzRyT0pKdpeyWBnNfEnV1YUe/WvOvbwVtfNNtaaOBuTCBtjAOBgNVHQ8BAf8EBAMCB4AwEwYDVR0lBAwwCgYIKwYBBQUHAwIwDAYDVR0TAQH/BAIwADAfBgNVHSMEGDAWgBTavJCbgtXPXB5qiKEbckDfoHtTYzBgBgNVHREEWTBXhlVsb2NhbDpyZW1vcmE6dGVzdDo1NDExNDRmYS04NjJhLTQyYmUtOGMyOC1jOGZlZTRmZmYyMTE6ZmFjdG9yeS1kZXZpY2U6ZXRjaGVyLWUyZS0wMDAyMAoGCCqGSM49BAMCA0kAMEYCIQDwat42oEkoYr8UP3098PcQmoMeUSN19sFrLaKqx202RQIhAKD9lUkGdAZTeo6sal7k09ZbQXgojlSH/Wkuy+Kv2ngG";
    pub const FACTORY_CA_DER_BASE64: &str = "MIIBhjCCASygAwIBAgIQS/N1W7Oq8pLhx31cFqzhLTAKBggqhkjOPQQDAjAjMSEwHwYDVQQDExhSZW1vcmEgRmFjdG9yeSBBdXRob3JpdHkwHhcNMjYwODMxMTAxNDIzWhcNMzYwODI4MTAxNDIzWjAjMSEwHwYDVQQDExhSZW1vcmEgRmFjdG9yeSBBdXRob3JpdHkwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAASo7CxfHieOeLpwISehevK5iQlpco/JrjzBUz4Hb/nEplOy+pHtMZBwiS88SFbfJ+OFZIvP3BAvbYJVC0lCiE1Ho0IwQDAOBgNVHQ8BAf8EBAMCAQYwDwYDVR0TAQH/BAUwAwEB/zAdBgNVHQ4EFgQU2ryQm4LVz1weaoihG3JA36B7U2MwCgYIKoZIzj0EAwIDSAAwRQIhAPnhL/Yc8pshLe+CDV6wDxp+3UQ/2CxZDw3WBHeYWurtAiB9u69m6rpOJjas/w8SqFKGiNN8x0SGLe5aGzounW0TpQ==";
    pub const SERVER_CA_DER_BASE64: &str = "MIIBhDCCASqgAwIBAgIQFoFk5k3bI45MlK4+jls7/DAKBggqhkjOPQQDAjAiMSAwHgYDVQQDExdSZW1vcmEgQWNjZXNzIEF1dGhvcml0eTAeFw0yNjA4MzExMDE0MjNaFw0zNjA4MjgxMDE0MjNaMCIxIDAeBgNVBAMTF1JlbW9yYSBBY2Nlc3MgQXV0aG9yaXR5MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE9/FRLEAsRymoDtbuM7OQ7qbV/ItnHZvXnpONk097aGO2MnvVoOZlZqVPASPCKjAZ75iSePlkxxWvqSCoch5VIaNCMEAwDgYDVR0PAQH/BAQDAgEGMA8GA1UdEwEB/wQFMAMBAf8wHQYDVR0OBBYEFPVHLpKuWW18JNSTWAQHBzRBGzz4MAoGCCqGSM49BAMCA0gAMEUCIBdUmn1hw4YtONRUF7+rhpzuwA7yuQovjSeJ5ma1lL1WAiEA1sdjPosKDyNhNFLE6QU7vaYKaKymMCPA8gV5RjM8Bpw=";
    pub const KEYID: &str =
        "local:kms:test:541144fa-862a-42be-8c28-c8fee4fff211:certificate:ad4043bbd8c58aa78bbebdb2a5464847";
    pub const DEVICE_NAME: &str = "etcher-e2e-0002";
    pub const ACCESS_URL: &str = "https://remora.access.sntns.dev/access/v1";
    pub const EXPECTED_URI_SAN: &str =
        "local:remora:test:541144fa-862a-42be-8c28-c8fee4fff211:factory-device:etcher-e2e-0002";
}

struct RealCapturedProvisioning {
    certificate_der: Vec<u8>,
    certificate_authority_der: Vec<u8>,
    server_certificate_authority_der: Vec<u8>,
    keyid: String,
    access_url: String,
}

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for RealCapturedProvisioning {
    async fn provision(
        &self,
        _gateway_url: &str,
        _api_key: &str,
        _device_name: &str,
        _csr_der: &[u8],
    ) -> remora_etcher_factory::adapter::Result<ProvisionedIdentity> {
        Ok(ProvisionedIdentity {
            certificate_der: self.certificate_der.clone(),
            certificate_authority_der: self.certificate_authority_der.clone(),
            server_certificate_authority_der: self.server_certificate_authority_der.clone(),
            key_id: self.keyid.clone(),
            access_url: self.access_url.clone(),
        })
    }
}

fn assert_well_formed_idevid(certificate_der: &[u8], factory_ca_der: &[u8], server_ca_der: &[u8]) {
    let (_, cert) =
        x509_parser::parse_x509_certificate(certificate_der).expect("certificate must parse");
    let (_, factory_ca) =
        x509_parser::parse_x509_certificate(factory_ca_der).expect("factory CA must parse");
    let (_, server_ca) =
        x509_parser::parse_x509_certificate(server_ca_der).expect("server CA must parse");

    assert!(
        cert.verify_signature(Some(factory_ca.public_key())).is_ok(),
        "certificate must chain to the factory CA, not some other key"
    );
    assert_ne!(
        factory_ca.public_key().subject_public_key.data,
        server_ca.public_key().subject_public_key.data,
        "factory CA and server CA must be two different keys/authorities"
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
    assert_eq!(uri, real_captured_credential::EXPECTED_URI_SAN);
}

/// Verifies the rendering path against a *real* sntns-dev-issued credential
/// set (captured by the platform-side session), not fixture bytes -- that
/// the bytes survive the write/read round trip exactly, and that the
/// resulting yaml is itself a well-formed, correctly chained credential.
#[tokio::test]
async fn renders_a_real_platform_issued_credential_set_correctly() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use real_captured_credential as fixture;

    let decode = |s: &str| STANDARD.decode(s).unwrap();
    let certificate_der = decode(fixture::CERTIFICATE_DER_BASE64);
    let factory_ca_der = decode(fixture::FACTORY_CA_DER_BASE64);
    let server_ca_der = decode(fixture::SERVER_CA_DER_BASE64);

    // Sanity-check the captured data itself before trusting it as a test
    // oracle: the platform session's own claims about what it issued.
    assert_well_formed_idevid(&certificate_der, &factory_ca_der, &server_ca_der);

    let output = temp_path("remora-factory.yaml");
    let provisioning = RealCapturedProvisioning {
        certificate_der: certificate_der.clone(),
        certificate_authority_der: factory_ca_der.clone(),
        server_certificate_authority_der: server_ca_der.clone(),
        keyid: fixture::KEYID.to_string(),
        access_url: fixture::ACCESS_URL.to_string(),
    };

    controller(provisioning)
        .provision(
            fixture::DEVICE_NAME,
            "https://api.example.invalid",
            "unused",
            None,
            &output,
            &OperationContext::noop(),
        )
        .await
        .expect("provisioning against the real captured data should succeed");

    let yaml = fs::read_to_string(&output).unwrap();
    let _ = fs::remove_file(&output);

    assert_eq!(extract_scalar(&yaml, "url"), fixture::ACCESS_URL);
    assert_eq!(extract_scalar(&yaml, "key-id"), fixture::KEYID);

    let written_certificate_der = pem::parse(extract_block(&yaml, "certificate"))
        .unwrap()
        .into_contents();
    let written_factory_ca_der = pem::parse(extract_block(&yaml, "authority"))
        .unwrap()
        .into_contents();
    let written_server_ca_der = pem::parse(extract_block(&yaml, "server-authority"))
        .unwrap()
        .into_contents();

    assert_eq!(written_certificate_der, certificate_der);
    assert_eq!(written_factory_ca_der, factory_ca_der);
    assert_eq!(written_server_ca_der, server_ca_der);

    // Byte-identical to the input the platform issued, so every property
    // already checked above (chain, SAN, EKU, CA-ness) still holds -- but
    // re-derive it from the rendered file rather than trust that equality
    // alone, closing the loop end to end.
    assert_well_formed_idevid(
        &written_certificate_der,
        &written_factory_ca_der,
        &written_server_ca_der,
    );

    // The private key remora-etcher writes is one it generated itself
    // (never the platform's, which never sees it) -- it won't correspond
    // to this certificate's public key, but it must still be a well-formed
    // SEC1 EC key.
    let key_pem = extract_block(&yaml, "key");
    assert!(key_pem.starts_with("-----BEGIN EC PRIVATE KEY-----"));
    p256::SecretKey::from_sec1_pem(&key_pem).expect("rendered key must parse as a SEC1 EC key");
}
