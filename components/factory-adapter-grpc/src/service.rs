use error_stack::Report;
use remora_context::model::ResolvedContext;
use remora_factory::{
    adapter::provisioning::{Error, FactoryProvisioningAdapter, ProvisionedIdentity, Result},
    model::DeviceSerial,
};
use remora_platform_grpc::{
    connect, sntns::service::remora::v1 as pb, status_summary, Connection, GatewayChannel,
};

type Devices = pb::device_service_client::DeviceServiceClient<GatewayChannel>;

/// The remora gateway's DeviceService, as far as manufacturing goes.
pub struct FactoryGatewayAdapterImpl;

/// Exactly one of `device_name`/`serial_number_policy_name`; `force` only
/// with a name -- what the platform accepts.
fn request(serial: &DeviceSerial, csr_der: &[u8]) -> pb::DeviceServiceCreateFactoryDeviceRequest {
    let mut request = pb::DeviceServiceCreateFactoryDeviceRequest {
        certificate_signing_request: csr_der.to_vec(),
        ..Default::default()
    };
    match serial {
        DeviceSerial::FromPolicy(policy) => request.serial_number_policy_name = policy.clone(),
        DeviceSerial::Explicit { device_name, force } => {
            request.device_name = device_name.clone();
            request.force = *force;
        }
    }
    request
}

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for FactoryGatewayAdapterImpl {
    async fn provision(
        &self,
        context: &ResolvedContext,
        serial: &DeviceSerial,
        csr_der: &[u8],
    ) -> Result<ProvisionedIdentity> {
        let channel = connect(&Connection::for_resolved(context))
            .await
            .map_err(|report| report.change_context(Error::Request))?;
        let response = Devices::new(channel)
            .create_factory_device(request(serial, csr_der))
            .await
            .map_err(|status| {
                let summary = status_summary(&status);
                match (status.code(), serial) {
                    (tonic::Code::AlreadyExists, DeviceSerial::Explicit { device_name, .. }) => {
                        Report::new(Error::AlreadyExists(device_name.clone())).attach(summary)
                    }
                    (tonic::Code::Unavailable, _) => Report::new(Error::Request).attach(summary),
                    (tonic::Code::Unauthenticated, _) => {
                        Report::new(Error::Unauthenticated).attach(summary)
                    }
                    _ => Report::new(Error::Refused).attach(summary),
                }
            })?
            .into_inner();
        let key_id = response
            .certificate_reference
            .map(|reference| reference.urn)
            .filter(|urn| !urn.is_empty())
            .ok_or_else(|| {
                Report::new(Error::ParseResponse).attach("no certificate reference URN")
            })?;
        Ok(ProvisionedIdentity {
            serial_number: response.serial_number,
            factory_device_name: response.factory_device_name,
            certificate_der: response.certificate,
            certificate_authority_der: response.certificate_authority_certificate,
            server_certificate_authority_der: response.server_certificate_authority_certificate,
            key_id,
            access_url: response.access_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use remora_ota_adapter_grpc::test_gateway::TestGateway;

    use super::*;

    fn explicit(name: &str, force: bool) -> DeviceSerial {
        DeviceSerial::Explicit {
            device_name: name.into(),
            force,
        }
    }

    /// The platform refuses both names at once, and `force` alongside a
    /// policy: a policy request carries neither.
    #[test]
    fn requests_name_the_serial_one_way_only() {
        let policy = request(&DeviceSerial::FromPolicy("hubs".into()), b"csr");
        assert_eq!(
            (
                policy.serial_number_policy_name.as_str(),
                policy.device_name.as_str(),
                policy.force
            ),
            ("hubs", "", false)
        );
        let forced = request(&explicit("1H7Z", true), b"csr");
        assert_eq!(
            (
                forced.serial_number_policy_name.as_str(),
                forced.device_name.as_str(),
                forced.force
            ),
            ("", "1H7Z", true)
        );
        assert_eq!(forced.certificate_signing_request, b"csr");
    }

    #[tokio::test]
    async fn manufactures_refuses_a_duplicate_and_re_signs_with_force() {
        let (_, context) = TestGateway::serve().await;
        let adapter = FactoryGatewayAdapterImpl;

        let issued = adapter
            .provision(&context, &DeviceSerial::FromPolicy("hubs".into()), b"csr")
            .await
            .unwrap();
        assert_eq!(issued.serial_number, "hubs-1");
        assert_eq!(issued.key_id, "urn:test:certificate-hubs-1-1");
        assert_eq!(issued.access_url, "https://access.test/access/v1");

        adapter
            .provision(&context, &explicit("1H7Z", false), b"csr")
            .await
            .unwrap();
        let report = adapter
            .provision(&context, &explicit("1H7Z", false), b"csr")
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::AlreadyExists(name) if name == "1H7Z"));
        let resigned = adapter
            .provision(&context, &explicit("1H7Z", true), b"csr")
            .await
            .unwrap();
        assert_eq!(resigned.key_id, "urn:test:certificate-1H7Z-2");
    }
}
