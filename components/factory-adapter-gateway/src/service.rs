use base64::{engine::general_purpose::STANDARD, Engine as _};
use error_stack::Report;
use remora_factory::{
    adapter::{Error, FactoryProvisioningAdapter, ProvisionedIdentity, Result},
    model::DeviceSerial,
};

#[derive(Debug, Default, Clone)]
pub struct GatewayAdapterImpl {
    client: reqwest::Client,
}

#[derive(serde::Serialize)]
struct CreateFactoryDeviceRequest {
    /// Exactly one of `deviceName`/`serialNumberPolicyName` is sent -- the
    /// platform rejects both or neither.
    #[serde(rename = "deviceName", skip_serializing_if = "Option::is_none")]
    device_name: Option<String>,
    #[serde(
        rename = "serialNumberPolicyName",
        skip_serializing_if = "Option::is_none"
    )]
    serial_number_policy_name: Option<String>,
    /// Refused by the platform together with `serialNumberPolicyName`;
    /// omitted rather than sent as `false` so a policy request stays valid.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    force: bool,
    #[serde(rename = "certificateSigningRequest")]
    certificate_signing_request: String,
}

impl CreateFactoryDeviceRequest {
    fn new(serial: &DeviceSerial, csr_der: &[u8]) -> Self {
        let (device_name, serial_number_policy_name, force) = match serial {
            DeviceSerial::FromPolicy(policy) => (None, Some(policy.clone()), false),
            DeviceSerial::Explicit { device_name, force } => {
                (Some(device_name.clone()), None, *force)
            }
        };
        Self {
            device_name,
            serial_number_policy_name,
            force,
            certificate_signing_request: STANDARD.encode(csr_der),
        }
    }
}

#[derive(serde::Deserialize)]
struct CertificateReferenceWire {
    urn: String,
}

#[derive(serde::Deserialize)]
struct CreateFactoryDeviceResponse {
    #[serde(rename = "serialNumber")]
    serial_number: String,
    #[serde(rename = "factoryDeviceName")]
    factory_device_name: String,
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
        serial: &DeviceSerial,
        csr_der: &[u8],
    ) -> Result<ProvisionedIdentity> {
        let body = CreateFactoryDeviceRequest::new(serial, csr_der);
        let device_name = body.device_name.clone();

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

        let status = response.status();
        if status == reqwest::StatusCode::CONFLICT {
            if let Some(device_name) = device_name {
                let text = response.text().await.unwrap_or_default();
                return Err(Report::new(Error::AlreadyExists(device_name)).attach(text));
            }
        }
        if !status.is_success() {
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
            serial_number: parsed.serial_number,
            factory_device_name: parsed.factory_device_name,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(serial: &DeviceSerial) -> serde_json::Value {
        serde_json::to_value(CreateFactoryDeviceRequest::new(serial, b"csr")).unwrap()
    }

    /// The platform refuses both names at once, and `force` alongside a
    /// policy -- so a policy request must carry neither key at all.
    #[test]
    fn a_policy_request_sends_only_the_policy_name() {
        assert_eq!(
            body(&DeviceSerial::FromPolicy("hubs".to_string())),
            serde_json::json!({
                "serialNumberPolicyName": "hubs",
                "certificateSigningRequest": "Y3Ny",
            })
        );
    }

    #[test]
    fn a_forced_explicit_request_sends_the_device_name_and_force() {
        assert_eq!(
            body(&DeviceSerial::Explicit {
                device_name: "1H7Z".to_string(),
                force: true,
            }),
            serde_json::json!({
                "deviceName": "1H7Z",
                "force": true,
                "certificateSigningRequest": "Y3Ny",
            })
        );
    }
}
