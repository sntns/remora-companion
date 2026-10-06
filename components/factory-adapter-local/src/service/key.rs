use error_stack::{Report, ResultExt};
use p256::{
    ecdsa::{signature::Verifier, DerSignature, VerifyingKey},
    pkcs8::DecodePublicKey,
};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use remora_factory::{
    adapter::key::{DeviceKey, DeviceKeyAdapter, Error, Result},
    model::{PrivateKey, VerifiedCsr},
};
use sha2::{Digest, Sha256};
use x509_cert::{
    der::{oid::ObjectIdentifier, Decode, Encode},
    request::CertReq,
};

/// `ecdsa-with-SHA256` (RFC 5758): how a P-256 key signs its CSR, and the
/// only signature the platform's factory authority takes alongside it.
const ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");

/// A P-256 keypair (rcgen's default, what the platform's factory authority
/// signs) and a PKCS#10 CSR for it. Milliseconds of CPU, so done inline
/// rather than on a blocking thread.
#[derive(Debug, Default, Clone, Copy)]
pub struct DeviceKeyAdapterImpl;

#[async_trait::async_trait]
impl DeviceKeyAdapter for DeviceKeyAdapterImpl {
    async fn generate(&self, common_name: Option<&str>) -> Result<DeviceKey> {
        let key_pair = KeyPair::generate().change_context(Error::Keygen)?;

        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        if let Some(common_name) = common_name {
            params
                .distinguished_name
                .push(DnType::CommonName, common_name);
        }
        let csr = params
            .serialize_request(&key_pair)
            .change_context(Error::Csr)?;

        Ok(DeviceKey {
            csr_der: csr.der().as_ref().to_vec(),
            private_key: PrivateKey::from_pkcs8_der(key_pair.serialize_der()),
        })
    }

    async fn verify_csr(&self, csr_der: &[u8]) -> Result<VerifiedCsr> {
        verify(csr_der).map_err(|reason| Report::new(Error::InvalidCsr).attach(reason))
    }
}

/// The checks behind `verify_csr`, each failure saying which one failed.
fn verify(csr_der: &[u8]) -> std::result::Result<VerifiedCsr, String> {
    let request = CertReq::from_der(csr_der).map_err(|e| format!("not a DER PKCS#10 CSR: {e}"))?;
    let spki_der = request
        .info
        .public_key
        .to_der()
        .map_err(|e| format!("unreadable public key: {e}"))?;
    // Checks the id-ecPublicKey algorithm and the secp256r1 curve too.
    let key = p256::PublicKey::from_public_key_der(&spki_der)
        .map_err(|e| format!("not a P-256 public key: {e}"))?;
    if request.algorithm.oid != ECDSA_WITH_SHA256 {
        return Err(format!(
            "signed with {}, not ecdsa-with-SHA256",
            request.algorithm.oid
        ));
    }
    let signature = request
        .signature
        .as_bytes()
        .ok_or("the signature is not a whole number of bytes")
        .and_then(|bytes| DerSignature::try_from(bytes).map_err(|_| "malformed signature"))
        .map_err(str::to_string)?;
    let signed = request
        .info
        .to_der()
        .map_err(|e| format!("unreadable request info: {e}"))?;
    VerifyingKey::from(key)
        .verify(&signed, &signature)
        .map_err(|_| "the self-signature does not verify".to_string())?;
    Ok(VerifiedCsr {
        public_key_fingerprint: Sha256::digest(&spki_der)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use p256::pkcs8::DecodePrivateKey;
    use x509_parser::{certification_request::X509CertificationRequest, prelude::FromDer};

    use super::*;

    fn common_names(csr: &X509CertificationRequest) -> Vec<String> {
        csr.certification_request_info
            .subject
            .iter_common_name()
            .map(|cn| cn.as_str().unwrap().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn builds_a_self_signed_csr_for_a_fresh_p256_key() {
        let key = DeviceKeyAdapterImpl.generate(Some("1H7Z")).await.unwrap();

        let (_, csr) = X509CertificationRequest::from_der(&key.csr_der).unwrap();
        csr.verify_signature()
            .expect("the CSR is signed by its key");
        assert_eq!(common_names(&csr), ["1H7Z"]);

        let secret = p256::SecretKey::from_pkcs8_der(key.private_key.pkcs8_der()).unwrap();
        assert_eq!(
            csr.certification_request_info
                .subject_pki
                .subject_public_key
                .data
                .as_ref(),
            secret.public_key().to_sec1_bytes().as_ref(),
            "the CSR carries the returned key's public half"
        );
    }

    #[tokio::test]
    async fn leaves_the_subject_empty_without_a_common_name() {
        let key = DeviceKeyAdapterImpl.generate(None).await.unwrap();
        let (_, csr) = X509CertificationRequest::from_der(&key.csr_der).unwrap();
        assert!(common_names(&csr).is_empty());
    }

    fn csr_for(key_pair: &KeyPair, common_name: &str) -> Vec<u8> {
        let mut params = CertificateParams::default();
        params
            .distinguished_name
            .push(DnType::CommonName, common_name);
        params
            .serialize_request(key_pair)
            .unwrap()
            .der()
            .as_ref()
            .to_vec()
    }

    fn reason(report: &Report<Error>) -> String {
        assert!(matches!(report.current_context(), Error::InvalidCsr));
        format!("{report:?}")
    }

    #[tokio::test]
    async fn accepts_its_own_csr_and_names_the_key_not_the_request() {
        let key_pair = KeyPair::generate().unwrap();
        let first = DeviceKeyAdapterImpl
            .verify_csr(&csr_for(&key_pair, "first"))
            .await
            .unwrap();
        let second = DeviceKeyAdapterImpl
            .verify_csr(&csr_for(&key_pair, "second"))
            .await
            .unwrap();
        assert_eq!(first, second, "same key, same fingerprint");
        assert_eq!(first.public_key_fingerprint.len(), 64);

        let other = DeviceKeyAdapterImpl
            .verify_csr(&csr_for(&KeyPair::generate().unwrap(), "first"))
            .await
            .unwrap();
        assert_ne!(first, other);

        let generated = DeviceKeyAdapterImpl.generate(None).await.unwrap();
        DeviceKeyAdapterImpl
            .verify_csr(&generated.csr_der)
            .await
            .expect("generate's own CSR verifies");
    }

    #[tokio::test]
    async fn refuses_garbage_another_curve_and_a_broken_signature() {
        let report = DeviceKeyAdapterImpl
            .verify_csr(b"not a csr")
            .await
            .unwrap_err();
        assert!(reason(&report).contains("not a DER PKCS#10 CSR"));

        let p384 = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P384_SHA384).unwrap();
        let report = DeviceKeyAdapterImpl
            .verify_csr(&csr_for(&p384, "1H7Z"))
            .await
            .unwrap_err();
        assert!(reason(&report).contains("not a P-256 public key"));

        // Flip a byte of the subject: still a well-formed request, but no
        // longer the one the key signed.
        let mut tampered = csr_for(&KeyPair::generate().unwrap(), "1H7Z");
        let at = tampered
            .windows(4)
            .position(|window| window == b"1H7Z")
            .unwrap();
        tampered[at] = b'2';
        let report = DeviceKeyAdapterImpl
            .verify_csr(&tampered)
            .await
            .unwrap_err();
        assert!(reason(&report).contains("does not verify"));
    }

    #[tokio::test]
    async fn never_prints_the_private_key() {
        let key = DeviceKeyAdapterImpl.generate(None).await.unwrap();
        assert!(format!("{key:?}").contains("PrivateKey(<redacted>)"));
    }
}
