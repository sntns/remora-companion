use error_stack::Report;
use p256::pkcs8::DecodePrivateKey;
use p256::SecretKey;
use remora_etcher_factory::{application::Error, model::FactoryCredential};

/// Render a `remora-factory.yaml` matching the schema read from two real
/// provisioned devices: `url`, `key`, `certificate`, `key_id`, `authority`,
/// `server_authority`, each certificate/key a PEM block-scalar and `key_id`
/// quoted. Hand-formatted rather than run through a YAML serializer crate,
/// to guarantee byte-for-byte the same block-scalar style as those known
/// good files rather than whatever a generic serializer happens to choose.
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
    out.push_str("key_id: \"");
    out.push_str(&credential.key_id);
    out.push_str("\"\n");
    out.push_str("authority: |\n");
    push_indented(&mut out, &authority_pem);
    out.push_str("server_authority: |\n");
    push_indented(&mut out, &server_authority_pem);

    Ok(out)
}

/// rcgen's own DER output is PKCS#8; the known-good `remora-factory.yaml`
/// files carry the key as SEC1 (`BEGIN EC PRIVATE KEY`) instead, so this
/// conversion is load-bearing, not cosmetic.
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
