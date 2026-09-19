use base64::{engine::general_purpose::STANDARD, Engine as _};
use error_stack::Report;
use remora_etcher_factory::{
    adapter::{Error, FactoryProvisioningAdapter, ProvisionedIdentity, Result},
    model::CertificateReference,
};

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
    id: String,
    urn: String,
    name: String,
}

#[derive(serde::Deserialize)]
struct CreateFactoryDeviceResponse {
    #[serde(rename = "factoryDeviceName")]
    factory_device_name: String,
    #[serde(rename = "certificateReference")]
    certificate_reference: CertificateReferenceWire,
    certificate: String,
    #[serde(rename = "certificateAuthorityCertificate")]
    certificate_authority_certificate: String,
    #[serde(rename = "serverCertificateAuthorityCertificate")]
    server_certificate_authority_certificate: String,
}

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for GatewayAdapterImpl {
    async fn provision(
        &self,
        gateway_url: &str,
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
                gateway_url.trim_end_matches('/')
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
            factory_device_name: parsed.factory_device_name,
            certificate_reference: CertificateReference {
                id: parsed.certificate_reference.id,
                urn: parsed.certificate_reference.urn,
                name: parsed.certificate_reference.name,
            },
            certificate_der: decode("certificate", &parsed.certificate)?,
            certificate_authority_der: decode(
                "certificateAuthorityCertificate",
                &parsed.certificate_authority_certificate,
            )?,
            server_certificate_authority_der: decode(
                "serverCertificateAuthorityCertificate",
                &parsed.server_certificate_authority_certificate,
            )?,
        })
    }
}
