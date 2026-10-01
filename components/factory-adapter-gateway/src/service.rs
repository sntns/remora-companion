use base64::{engine::general_purpose::STANDARD, Engine as _};
use error_stack::Report;
use remora_factory::adapter::{Error, FactoryProvisioningAdapter, ProvisionedIdentity, Result};

#[derive(Debug, Default, Clone)]
pub struct GatewayAdapterImpl {
    client: reqwest::Client,
}

#[derive(serde::Serialize)]
struct CreateFactoryDeviceRequest {
    #[serde(rename = "deviceName")]
    device_name: String,
    #[serde(rename = "certificateSigningRequest")]
    certificate_signing_request: String,
}

#[derive(serde::Deserialize)]
struct CertificateReferenceWire {
    urn: String,
}

#[derive(serde::Deserialize)]
struct CreateFactoryDeviceResponse {
    #[serde(rename = "certificateReference")]
    certificate_reference: CertificateReferenceWire,
    certificate: String,
    #[serde(rename = "certificateAuthorityCertificate")]
    certificate_authority_certificate: String,
    #[serde(rename = "serverCertificateAuthorityCertificate")]
    server_certificate_authority_certificate: String,
    /// New field, not yet present on every deployment -- `#[serde(default)]`
    /// so an old/unconfigured gateway that omits it decodes to `""` rather
    /// than failing the whole response; the application layer is what
    /// turns an empty value into a hard error.
    #[serde(rename = "accessUrl", default)]
    access_url: String,
}

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for GatewayAdapterImpl {
    async fn provision(
        &self,
        api_url: &str,
        api_key: &str,
        device_name: &str,
        csr_der: &[u8],
    ) -> Result<ProvisionedIdentity> {
        let body = CreateFactoryDeviceRequest {
            device_name: device_name.to_string(),
            certificate_signing_request: STANDARD.encode(csr_der),
        };

        let response = self
            .client
            .post(format!(
                "{}/remora/v1/factory-device",
                api_url.trim_end_matches('/')
            ))
            .header("Authorization", format!("X-SNTNS-API-KEY {api_key}"))
            .json(&body)
            .send()
            .await
            .map_err(|e| Report::new(Error::Request).attach(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Report::new(Error::Refused(format!("{status}: {text}"))));
        }

        let parsed: CreateFactoryDeviceResponse = response
            .json()
            .await
            .map_err(|e| Report::new(Error::ParseResponse).attach(e.to_string()))?;

        let decode = |field: &'static str, value: &str| {
            STANDARD
                .decode(value)
                .map_err(|e| Report::new(Error::ParseResponse).attach(format!("{field}: {e}")))
        };

        Ok(ProvisionedIdentity {
            key_id: parsed.certificate_reference.urn,
            certificate_der: decode("certificate", &parsed.certificate)?,
            certificate_authority_der: decode(
                "certificateAuthorityCertificate",
                &parsed.certificate_authority_certificate,
            )?,
            server_certificate_authority_der: decode(
                "serverCertificateAuthorityCertificate",
                &parsed.server_certificate_authority_certificate,
            )?,
            access_url: parsed.access_url,
        })
    }

    async fn delete(&self, api_url: &str, api_key: &str, device_name: &str) -> Result<()> {
        let mut url = reqwest::Url::parse(&format!("{}/", api_url.trim_end_matches('/')))
            .map_err(|e| Report::new(Error::Request).attach(e.to_string()))?;
        url.path_segments_mut()
            .map_err(|_| Report::new(Error::Request).attach("api_url cannot be a base"))?
            .extend(["remora", "v1", "factory-device", device_name]);

        let response = self
            .client
            .delete(url)
            .header("Authorization", format!("X-SNTNS-API-KEY {api_key}"))
            .send()
            .await
            .map_err(|e| Report::new(Error::Request).attach(e.to_string()))?;

        // No prior credential to delete is the expected steady state for a
        // never-before-provisioned device -- not a failure.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Report::new(Error::Refused(format!("{status}: {text}"))));
        }
        Ok(())
    }
}
