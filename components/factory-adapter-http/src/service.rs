use error_stack::Report;
use remora_context::model::ResolvedContext;
use remora_factory::{
    adapter::provisioning::{Error, FactoryProvisioningAdapter, ProvisionedIdentity, Result},
    model::{DeviceOwner, DeviceSerial},
};
use remora_platform_http::{model, status_summary, Code, Connection, Gateway};

/// The remora gateway's DeviceService, as far as manufacturing goes.
pub struct FactoryGatewayAdapterImpl;

/// Exactly one of `device_name`/`serial_number_policy_name`; `force` only
/// with a name -- what the platform accepts.
fn request(serial: &DeviceSerial, csr_der: &[u8]) -> model::CreateFactoryDeviceRequest {
    let mut request = model::CreateFactoryDeviceRequest {
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

/// The tenant of a platform name (`<partition>:<service>:<region>:<tenant>:
/// <qualifier>:<path>`, as the platform renders a factory device's name, or
/// the same behind `urn:`): the account id it carries for its manufacturer.
fn tenant(name: &str) -> Option<&str> {
    let mut parts = name.strip_prefix("urn:").unwrap_or(name).split(':');
    let tenant = parts.nth(3)?;
    (!tenant.is_empty() && parts.next().is_some()).then_some(tenant)
}

#[async_trait::async_trait]
impl FactoryProvisioningAdapter for FactoryGatewayAdapterImpl {
    async fn provision(
        &self,
        context: &ResolvedContext,
        serial: &DeviceSerial,
        owner: &DeviceOwner,
        csr_der: &[u8],
    ) -> Result<ProvisionedIdentity> {
        let gateway = Gateway::new(&Connection::for_resolved(context))
            .await
            .map_err(|report| report.change_context(Error::Request))?;
        let response: model::CreateFactoryDeviceResponse = gateway
            .post(
                gateway.url(["remora", "v1", "factory-device"]),
                &request(serial, csr_der),
            )
            .await
            .map_err(|status| {
                let summary = status_summary(&status);
                match (status.code, serial) {
                    (Code::AlreadyExists, DeviceSerial::Explicit { device_name, .. }) => {
                        Report::new(Error::AlreadyExists(device_name.clone())).attach(summary)
                    }
                    (Code::Unavailable, _) => Report::new(Error::Request).attach(summary),
                    (Code::Unauthenticated, _) => {
                        Report::new(Error::Unauthenticated).attach(summary)
                    }
                    _ => Report::new(Error::Refused).attach(summary),
                }
            })?;
        let key_id = response
            .certificate_reference
            .map(|reference| reference.urn)
            .filter(|urn| !urn.is_empty())
            .ok_or_else(|| {
                Report::new(Error::ParseResponse).attach("no certificate reference URN")
            })?;
        // The manufacturer is the caller, whose tenant names the device.
        let owner_account = match owner {
            DeviceOwner::Account(account) => account.clone(),
            DeviceOwner::Manufacturer => tenant(&response.factory_device_name)
                .ok_or_else(|| {
                    Report::new(Error::ParseResponse).attach(format!(
                        "no tenant in factory device name {:?}",
                        response.factory_device_name
                    ))
                })?
                .to_string(),
        };
        let assigned: model::AssignFactoryDeviceResponse = gateway
            .post(
                gateway.url([
                    "remora",
                    "v1",
                    "factory-device",
                    &response.serial_number,
                    "assignment",
                ]),
                &model::AssignFactoryDeviceRequest {
                    owner_account: owner_account.clone(),
                },
            )
            .await
            .map_err(|status| {
                let error = Error::Assign {
                    serial: response.serial_number.clone(),
                    owner: owner_account.clone(),
                };
                Report::new(error).attach(status_summary(&status))
            })?;
        let owner_account = assigned
            .owner_reference
            .map(|reference| reference.urn)
            .filter(|urn| !urn.is_empty())
            .unwrap_or(owner_account);
        Ok(ProvisionedIdentity {
            serial_number: response.serial_number,
            factory_device_name: response.factory_device_name,
            certificate_der: response.certificate,
            certificate_authority_der: response.certificate_authority_certificate,
            server_certificate_authority_der: response.server_certificate_authority_certificate,
            key_id,
            access_url: response.access_url,
            owner_account,
        })
    }
}

#[cfg(test)]
mod tests {
    use remora_platform_http::test_gateway::{TestGateway, TENANT};

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
            .provision(
                &context,
                &DeviceSerial::FromPolicy("hubs".into()),
                &DeviceOwner::Manufacturer,
                b"csr",
            )
            .await
            .unwrap();
        assert_eq!(issued.serial_number, "hubs-1");
        assert_eq!(issued.key_id, "urn:test:certificate-hubs-1-1");
        assert_eq!(issued.access_url, "https://access.test/access/v1");

        adapter
            .provision(
                &context,
                &explicit("1H7Z", false),
                &DeviceOwner::Manufacturer,
                b"csr",
            )
            .await
            .unwrap();
        let report = adapter
            .provision(
                &context,
                &explicit("1H7Z", false),
                &DeviceOwner::Manufacturer,
                b"csr",
            )
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::AlreadyExists(name) if name == "1H7Z"));
        let resigned = adapter
            .provision(
                &context,
                &explicit("1H7Z", true),
                &DeviceOwner::Manufacturer,
                b"csr",
            )
            .await
            .unwrap();
        assert_eq!(resigned.key_id, "urn:test:certificate-1H7Z-2");
    }

    #[test]
    fn a_name_carries_its_tenant() {
        // What the platform returns: no `urn:` prefix.
        assert_eq!(
            tenant("sntns:remora:eu2:6f1c:factory-device:1H7Z"),
            Some("6f1c")
        );
        assert_eq!(
            tenant("urn:sntns:remora:eu2:6f1c:factory-device:1H7Z"),
            Some("6f1c")
        );
        assert_eq!(tenant("urn:test:factory-device:1H7Z"), None);
        assert_eq!(tenant("urn:sntns:remora:eu2::factory-device:1H7Z"), None);
        assert_eq!(tenant("1H7Z"), None);
    }

    /// By default the device goes to the manufacturer's own account, the
    /// tenant its factory device name carries; else to the account asked.
    #[tokio::test]
    async fn assigns_what_it_manufactures() {
        let (gateway, context) = TestGateway::serve().await;
        let adapter = FactoryGatewayAdapterImpl;

        let own = adapter
            .provision(
                &context,
                &explicit("1H7Z", false),
                &DeviceOwner::Manufacturer,
                b"csr",
            )
            .await
            .unwrap();
        assert_eq!(gateway.0.lock().unwrap().factory_owners["1H7Z"], TENANT);
        assert_eq!(
            own.owner_account,
            format!("urn:sntns:iam:test:{TENANT}:account")
        );

        let customer = "urn:sntns:iam:test:0b7e:account";
        let sold = adapter
            .provision(
                &context,
                &explicit("2K4P", false),
                &DeviceOwner::Account(customer.into()),
                b"csr",
            )
            .await
            .unwrap();
        assert_eq!(gateway.0.lock().unwrap().factory_owners["2K4P"], customer);
        assert_eq!(sold.owner_account, customer);

        let report = adapter
            .provision(
                &context,
                &explicit("3M9Q", false),
                &DeviceOwner::Account("not an account".into()),
                b"csr",
            )
            .await
            .unwrap_err();
        assert!(matches!(
            report.current_context(),
            Error::Assign { serial, .. } if serial == "3M9Q"
        ));
    }
}
