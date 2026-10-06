use std::{io::Write, path::Path};

use error_stack::ResultExt;
use remora_factory::{
    adapter::credential::{CredentialWriterAdapter, Error, Result},
    model::FactoryCredential,
};

use crate::yaml;

/// Renders `remora-factory.yaml` (see `yaml::render` for the format) and
/// writes it owner-only (0600 on unix: it carries the device's private
/// key, and a umask-default 0644 would hand that to every local user),
/// through a temporary file in the same directory renamed over `output`,
/// so neither a crash nor a concurrent reader ever sees a partial key.
#[derive(Debug, Default, Clone, Copy)]
pub struct CredentialWriterAdapterImpl;

#[async_trait::async_trait]
impl CredentialWriterAdapter for CredentialWriterAdapterImpl {
    async fn write(
        &self,
        credential: &FactoryCredential,
        access_url: &str,
        output: &Path,
    ) -> Result<()> {
        let rendered = yaml::render(credential, access_url)?;
        let path = output.to_path_buf();
        // fsync waits on the disk; keep it off the runtime's workers.
        tokio::task::spawn_blocking(move || write_owner_only(&path, rendered.as_bytes()))
            .await
            .change_context_lazy(|| Error::Write(output.to_path_buf()))?
            .change_context_lazy(|| Error::Write(output.to_path_buf()))
    }
}

/// The temporary file lives beside `output`, not in the system temp dir:
/// a rename is only atomic within one filesystem. Dropped (so removed)
/// on any failure before the rename.
fn write_owner_only(output: &Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = match output.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix(".remora-factory.").suffix(".tmp");
    // tempfile's own default is already 0600; spelled out so the property
    // this file depends on doesn't rest on a library default.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o600));
    }
    let mut file = builder.tempfile_in(dir)?;
    file.write_all(contents)?;
    file.as_file().sync_all()?;
    file.persist(output).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use remora_factory::model::PrivateKey;

    use super::*;
    use crate::DeviceKeyAdapterImpl;

    /// Pulls the dedented body of a `key: |` block scalar out of a rendered
    /// `remora-factory.yaml`, matching exactly the shape `yaml::render`
    /// writes (2-space indent, one block per key, block ends at the first
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

    async fn private_key() -> PrivateKey {
        use remora_factory::adapter::key::DeviceKeyAdapter;
        DeviceKeyAdapterImpl
            .generate(None)
            .await
            .unwrap()
            .private_key
    }

    async fn fake_credential() -> FactoryCredential {
        FactoryCredential {
            private_key: private_key().await,
            certificate_der: b"fake-certificate-der".to_vec(),
            certificate_authority_der: b"fake-factory-ca-der".to_vec(),
            server_certificate_authority_der: b"fake-server-ca-der".to_vec(),
            key_id: "test:kms:certificate:e2e-serial-0001".into(),
        }
    }

    #[tokio::test]
    async fn renders_the_layout_remora_edge_parses() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("remora-factory.yaml");
        CredentialWriterAdapterImpl
            .write(
                &fake_credential().await,
                "https://remora.access.eu2.sntns.io/access/v1",
                &output,
            )
            .await
            .unwrap();
        let yaml = fs::read_to_string(&output).unwrap();

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
        for (key, der) in [
            ("certificate", &b"fake-certificate-der"[..]),
            ("authority", b"fake-factory-ca-der"),
            ("server-authority", b"fake-server-ca-der"),
        ] {
            assert_eq!(
                pem::parse(extract_block(&yaml, key)).unwrap().contents(),
                der
            );
        }

        // The two keys the platform-side session flagged as easy to get
        // wrong (underscore vs hyphen) -- assert the literal on-disk
        // spelling, not just that *some* value round-trips, so a regression
        // here fails this test instead of silently producing a yaml
        // remora-edge's parser either rejects outright (`key-id`, no serde
        // default) or silently drops (`server-authority`, `Option` with
        // `#[serde(default)]`).
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

    /// Even over an existing world-readable file: the rename replaces it
    /// rather than writing through its permissions, and leaves no
    /// temporary file behind.
    #[cfg(unix)]
    #[tokio::test]
    async fn writes_owner_only_and_leaves_nothing_behind() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("remora-factory.yaml");
        fs::write(&output, "stale").unwrap();
        fs::set_permissions(&output, fs::Permissions::from_mode(0o644)).unwrap();

        CredentialWriterAdapterImpl
            .write(
                &fake_credential().await,
                "https://a.test/access/v1",
                &output,
            )
            .await
            .unwrap();

        let mode = fs::metadata(&output).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
        assert!(fs::read_to_string(&output).unwrap().starts_with("url: "));
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, ["remora-factory.yaml"]);
    }

    #[tokio::test]
    async fn fails_naming_the_output_when_its_directory_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("missing").join("remora-factory.yaml");
        let report = CredentialWriterAdapterImpl
            .write(
                &fake_credential().await,
                "https://a.test/access/v1",
                &output,
            )
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::Write(path) if *path == output));
        assert!(
            !format!("{report:?}").contains("BEGIN EC PRIVATE KEY"),
            "an error never carries the key"
        );
    }

    /// A real credential set issued by `sntns-dev` for a throwaway test
    /// device (`etcher-e2e-0002`), captured by the platform-side session
    /// that built the gateway endpoint -- lets this test verify the
    /// rendering path against real platform output, not just made-up
    /// fixture bytes, and separately verify that output is itself a
    /// well-formed, correctly chained credential (the cross-check the
    /// platform session asked for).
    mod real_captured_credential {
        pub const CERTIFICATE_DER_BASE64: &str = "MIIB9zCCAZygAwIBAgIRAK1AQ7vYxYqni769sqVGSEcwCgYIKoZIzj0EAwIwIzEhMB8GA1UEAxMYUmVtb3JhIEZhY3RvcnkgQXV0aG9yaXR5MB4XDTI2MDkxOTEzNTExOVoXDTM2MDgyODEwMTQyM1owGjEYMBYGA1UEAxMPZXRjaGVyLWUyZS0wMDAyMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEmpS3cHumP+jBMiRf7/GRWiUJzCtNRDJ9uBNCRDoA+Em7AaopzRyT0pKdpeyWBnNfEnV1YUe/WvOvbwVtfNNtaaOBuTCBtjAOBgNVHQ8BAf8EBAMCB4AwEwYDVR0lBAwwCgYIKwYBBQUHAwIwDAYDVR0TAQH/BAIwADAfBgNVHSMEGDAWgBTavJCbgtXPXB5qiKEbckDfoHtTYzBgBgNVHREEWTBXhlVsb2NhbDpyZW1vcmE6dGVzdDo1NDExNDRmYS04NjJhLTQyYmUtOGMyOC1jOGZlZTRmZmYyMTE6ZmFjdG9yeS1kZXZpY2U6ZXRjaGVyLWUyZS0wMDAyMAoGCCqGSM49BAMCA0kAMEYCIQDwat42oEkoYr8UP3098PcQmoMeUSN19sFrLaKqx202RQIhAKD9lUkGdAZTeo6sal7k09ZbQXgojlSH/Wkuy+Kv2ngG";
        pub const FACTORY_CA_DER_BASE64: &str = "MIIBhjCCASygAwIBAgIQS/N1W7Oq8pLhx31cFqzhLTAKBggqhkjOPQQDAjAjMSEwHwYDVQQDExhSZW1vcmEgRmFjdG9yeSBBdXRob3JpdHkwHhcNMjYwODMxMTAxNDIzWhcNMzYwODI4MTAxNDIzWjAjMSEwHwYDVQQDExhSZW1vcmEgRmFjdG9yeSBBdXRob3JpdHkwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAASo7CxfHieOeLpwISehevK5iQlpco/JrjzBUz4Hb/nEplOy+pHtMZBwiS88SFbfJ+OFZIvP3BAvbYJVC0lCiE1Ho0IwQDAOBgNVHQ8BAf8EBAMCAQYwDwYDVR0TAQH/BAUwAwEB/zAdBgNVHQ4EFgQU2ryQm4LVz1weaoihG3JA36B7U2MwCgYIKoZIzj0EAwIDSAAwRQIhAPnhL/Yc8pshLe+CDV6wDxp+3UQ/2CxZDw3WBHeYWurtAiB9u69m6rpOJjas/w8SqFKGiNN8x0SGLe5aGzounW0TpQ==";
        pub const SERVER_CA_DER_BASE64: &str = "MIIBhDCCASqgAwIBAgIQFoFk5k3bI45MlK4+jls7/DAKBggqhkjOPQQDAjAiMSAwHgYDVQQDExdSZW1vcmEgQWNjZXNzIEF1dGhvcml0eTAeFw0yNjA4MzExMDE0MjNaFw0zNjA4MjgxMDE0MjNaMCIxIDAeBgNVBAMTF1JlbW9yYSBBY2Nlc3MgQXV0aG9yaXR5MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE9/FRLEAsRymoDtbuM7OQ7qbV/ItnHZvXnpONk097aGO2MnvVoOZlZqVPASPCKjAZ75iSePlkxxWvqSCoch5VIaNCMEAwDgYDVR0PAQH/BAQDAgEGMA8GA1UdEwEB/wQFMAMBAf8wHQYDVR0OBBYEFPVHLpKuWW18JNSTWAQHBzRBGzz4MAoGCCqGSM49BAMCA0gAMEUCIBdUmn1hw4YtONRUF7+rhpzuwA7yuQovjSeJ5ma1lL1WAiEA1sdjPosKDyNhNFLE6QU7vaYKaKymMCPA8gV5RjM8Bpw=";
        pub const KEYID: &str =
            "local:kms:test:541144fa-862a-42be-8c28-c8fee4fff211:certificate:ad4043bbd8c58aa78bbebdb2a5464847";
        pub const ACCESS_URL: &str = "https://remora.access.sntns.dev/access/v1";
        pub const EXPECTED_URI_SAN: &str =
            "local:remora:test:541144fa-862a-42be-8c28-c8fee4fff211:factory-device:etcher-e2e-0002";
    }

    fn assert_well_formed_idevid(
        certificate_der: &[u8],
        factory_ca_der: &[u8],
        server_ca_der: &[u8],
    ) {
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

    /// Verifies the rendering path against a *real* sntns-dev-issued
    /// credential set (captured by the platform-side session), not fixture
    /// bytes -- that the bytes survive the write/read round trip exactly,
    /// and that the resulting yaml is itself a well-formed, correctly
    /// chained credential.
    #[tokio::test]
    async fn renders_a_real_platform_issued_credential_set_correctly() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use real_captured_credential as fixture;

        let decode = |s: &str| STANDARD.decode(s).unwrap();
        let certificate_der = decode(fixture::CERTIFICATE_DER_BASE64);
        let factory_ca_der = decode(fixture::FACTORY_CA_DER_BASE64);
        let server_ca_der = decode(fixture::SERVER_CA_DER_BASE64);

        // Sanity-check the captured data itself before trusting it as a
        // test oracle: the platform session's own claims about what it
        // issued.
        assert_well_formed_idevid(&certificate_der, &factory_ca_der, &server_ca_der);

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("remora-factory.yaml");
        let credential = FactoryCredential {
            private_key: private_key().await,
            certificate_der: certificate_der.clone(),
            certificate_authority_der: factory_ca_der.clone(),
            server_certificate_authority_der: server_ca_der.clone(),
            key_id: fixture::KEYID.to_string(),
        };
        CredentialWriterAdapterImpl
            .write(&credential, fixture::ACCESS_URL, &output)
            .await
            .unwrap();
        let yaml = fs::read_to_string(&output).unwrap();

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

        // Byte-identical to the input the platform issued, so every
        // property already checked above (chain, SAN, EKU, CA-ness) still
        // holds -- but re-derive it from the rendered file rather than
        // trust that equality alone, closing the loop end to end.
        assert_well_formed_idevid(
            &written_certificate_der,
            &written_factory_ca_der,
            &written_server_ca_der,
        );

        // The private key remora-etcher writes is one it generated itself
        // (never the platform's, which never sees it) -- it won't
        // correspond to this certificate's public key, but it must still
        // be a well-formed SEC1 EC key.
        let key_pem = extract_block(&yaml, "key");
        assert!(key_pem.starts_with("-----BEGIN EC PRIVATE KEY-----"));
        p256::SecretKey::from_sec1_pem(&key_pem).expect("rendered key must parse as a SEC1 EC key");
    }
}
