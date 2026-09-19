use error_stack::Report;
use p256::pkcs8::DecodePrivateKey;
use p256::SecretKey;
use remora_etcher_factory::{application::Error, model::FactoryCredential};

/// Render a `remora-factory.yaml` matching `remora-edge`'s actual parser
/// (`FactoryIdentity` in `access-application/src/configuration.rs`, which
/// carries `#[serde(rename_all = "kebab-case")]`): `url`, `key`,
/// `certificate`, `key-id`, `authority`, `server-authority` -- hyphenated,
/// not underscored, despite two real provisioned devices' files under
/// `~/provisioning` using underscores; those files predate the parser
/// gaining that rename and are themselves wrong. `key-id` has no serde
/// default (missing/misspelled fails loudly at parse time), but
/// `server-authority` is `Option<String>` with `#[serde(default)]` --
/// misspelled there silently resolves to `None`, so get this one right.
/// Hand-formatted rather than run through a YAML serializer crate, to
/// guarantee byte-for-byte the same block-scalar style as the canonical
/// generator (`remora-accessd/scripts/factory-provision.sh`).
pub(crate) fn render(
    credential: &FactoryCredential,
    access_url: &str,
) -> Result<String, Report<Error>> {
    let key_pem = sec1_pem(&credential.private_key_der)?;
    let certificate_pem = encode_certificate_pem(&credential.certificate_der);
    let authority_pem = encode_certificate_pem(&credential.certificate_authority_der);
    let server_authority_pem = encode_certificate_pem(&credential.server_certificate_authority_der);

    let mut out = String::new();
    out.push_str("url: ");
    out.push_str(access_url);
    out.push('\n');
    out.push_str("key: |\n");
    push_indented(&mut out, &key_pem);
    out.push_str("certificate: |\n");
    push_indented(&mut out, &certificate_pem);
    out.push_str("key-id: \"");
    out.push_str(&credential.key_id);
    out.push_str("\"\n");
    out.push_str("authority: |\n");
    push_indented(&mut out, &authority_pem);
    out.push_str("server-authority: |\n");
    push_indented(&mut out, &server_authority_pem);

    Ok(out)
}

/// rcgen's own DER output is PKCS#8; the known-good `remora-factory.yaml`
/// files carry the key as SEC1 (`BEGIN EC PRIVATE KEY`) instead. Confirmed
/// with the remora-edge parser: `load_static_secret_key` tries
/// `from_sec1_pem` then falls back to `from_pkcs8_pem`, deliberately
/// matching the Go reference implementation so either format loads -- so
/// this conversion is belt-and-braces (matching the known-good files), not
/// load-bearing.
fn sec1_pem(pkcs8_der: &[u8]) -> Result<String, Report<Error>> {
    let secret_key = SecretKey::from_pkcs8_der(pkcs8_der)
        .map_err(|e| Report::new(Error::Render).attach(e.to_string()))?;
    let pem = secret_key
        .to_sec1_pem(Default::default())
        .map_err(|e| Report::new(Error::Render).attach(e.to_string()))?;
    Ok(pem.to_string())
}

fn encode_certificate_pem(der: &[u8]) -> String {
    pem::encode(&pem::Pem::new("CERTIFICATE", der.to_vec()))
}

fn push_indented(out: &mut String, pem: &str) {
    for line in pem.trim_end().lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
}
