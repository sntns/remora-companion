use error_stack::ResultExt;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use remora_factory::{
    adapter::key::{DeviceKey, DeviceKeyAdapter, Error, Result},
    model::PrivateKey,
};

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

    #[tokio::test]
    async fn never_prints_the_private_key() {
        let key = DeviceKeyAdapterImpl.generate(None).await.unwrap();
        assert!(format!("{key:?}").contains("PrivateKey(<redacted>)"));
    }
}
