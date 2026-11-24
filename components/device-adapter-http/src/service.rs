use error_stack::{Report, ResultExt};
use remora_context::model::ResolvedContext;
use remora_device::{
    adapter::gateway::{DeviceGatewayAdapter, Error, Result},
    model::{Device, FactoryDevice, Labels},
};
use remora_platform_http::{
    model::{self, time_of},
    status_summary, Code, Connection, Gateway, Status,
};

/// The remora gateway's devices, over its REST API.
pub struct DeviceGatewayAdapterImpl;

async fn gateway(context: &ResolvedContext) -> Result<Gateway> {
    Gateway::new(&Connection::for_resolved(context))
        .await
        .change_context(Error::Unreachable)
}

fn classify(status: Status) -> Report<Error> {
    let error = match status.code {
        Code::Unauthenticated => Error::Unauthenticated,
        Code::PermissionDenied => Error::PermissionDenied,
        Code::NotFound => Error::NotFound,
        Code::Unavailable => Error::Unreachable,
        _ => Error::Call,
    };
    Report::new(error).attach(status_summary(&status))
}

#[async_trait::async_trait]
impl DeviceGatewayAdapter for DeviceGatewayAdapterImpl {
    async fn list(&self, context: &ResolvedContext, labels: &Labels) -> Result<Vec<String>> {
        let gateway = gateway(context).await?;
        let mut url = gateway.url(["remora", "v1", "device"]);
        for (key, value) in labels {
            url.query_pairs_mut()
                .append_pair(&format!("deviceLabels[{key}]"), value);
        }
        let response: model::ListDevicesResponse = gateway.get(url).await.map_err(classify)?;
        let mut names: Vec<_> = response
            .device_summaries
            .into_iter()
            .filter_map(|summary| summary.resource.map(|resource| resource.name))
            .filter(|name| !name.is_empty())
            .collect();
        names.sort();
        Ok(names)
    }

    async fn get(&self, context: &ResolvedContext, name: &str) -> Result<Device> {
        let gateway = gateway(context).await?;
        let descriptor = gateway
            .get::<model::GetDeviceResponse>(gateway.url(["remora", "v1", "device", name]))
            .await
            .map_err(classify)?
            .device_descriptor
            .ok_or_else(|| Report::new(Error::Call).attach("empty device descriptor"))?;
        Ok(Device {
            name: name.to_owned(),
            labels: Some(descriptor.labels.into_iter().collect()),
            enrolled_at: time_of(descriptor.created_at.as_deref()),
        })
    }

    async fn delete(&self, context: &ResolvedContext, name: &str) -> Result<()> {
        let gateway = gateway(context).await?;
        gateway
            .delete::<model::DeviceReferenceResponse>(gateway.url(["remora", "v1", "device", name]))
            .await
            .map_err(classify)?;
        Ok(())
    }

    async fn list_factory(
        &self,
        context: &ResolvedContext,
        labels: &Labels,
    ) -> Result<Vec<FactoryDevice>> {
        let gateway = gateway(context).await?;
        let mut url = gateway.url(["remora", "v1", "factory-device"]);
        for (key, value) in labels {
            url.query_pairs_mut()
                .append_pair(&format!("factoryDeviceLabels[{key}]"), value);
        }
        let response: model::ListFactoryDevicesResponse =
            gateway.get(url).await.map_err(classify)?;
        let mut devices: Vec<_> = response
            .factory_device_summaries
            .into_iter()
            .filter_map(|summary| {
                let name = summary.resource.map(|resource| resource.name)?;
                (!name.is_empty()).then(|| FactoryDevice {
                    name,
                    owner: urn(summary.owner),
                    labels: None,
                    certificate: None,
                    created_at: time_of(summary.created_at.as_deref()),
                })
            })
            .collect();
        devices.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(devices)
    }

    async fn get_factory(&self, context: &ResolvedContext, name: &str) -> Result<FactoryDevice> {
        let gateway = gateway(context).await?;
        let descriptor = gateway
            .get::<model::GetFactoryDeviceResponse>(gateway.url([
                "remora",
                "v1",
                "factory-device",
                name,
            ]))
            .await
            .map_err(classify)?
            .factory_device
            .ok_or_else(|| Report::new(Error::Call).attach("empty factory device descriptor"))?;
        Ok(FactoryDevice {
            name: descriptor
                .resource
                .map(|resource| resource.name)
                .filter(|resource| !resource.is_empty())
                .unwrap_or_else(|| name.to_owned()),
            owner: urn(descriptor.owner),
            labels: Some(descriptor.labels.into_iter().collect()),
            certificate: urn(descriptor.certificate),
            created_at: time_of(descriptor.created_at.as_deref()),
        })
    }

    async fn delete_factory(&self, context: &ResolvedContext, name: &str) -> Result<Vec<String>> {
        let gateway = gateway(context).await?;
        let response: model::DeleteFactoryDeviceResponse = gateway
            .delete(gateway.url(["remora", "v1", "factory-device", name]))
            .await
            .map_err(classify)?;
        Ok(response
            .revoked_certificate_references
            .into_iter()
            .filter_map(|reference| urn(Some(reference)))
            .collect())
    }
}

/// A reference's URN, when it has one.
fn urn(reference: Option<model::ResourceReference>) -> Option<String> {
    reference
        .map(|reference| reference.urn)
        .filter(|urn| !urn.is_empty())
}

#[cfg(test)]
mod tests {
    use remora_platform_http::test_gateway::TestGateway;

    use super::*;

    #[tokio::test]
    async fn lists_by_label_and_reads_labels() {
        let (_, context) = TestGateway::serve().await;
        let all = DeviceGatewayAdapterImpl
            .list(&context, &Labels::new())
            .await
            .unwrap();
        assert_eq!(all, ["BROKEN", "DEV1", "DEV2", "DEV3"]);
        let hdc = DeviceGatewayAdapterImpl
            .list(&context, &Labels::from([("board".into(), "hdc".into())]))
            .await
            .unwrap();
        assert_eq!(hdc, ["DEV3"]);
        let dev3 = DeviceGatewayAdapterImpl
            .get(&context, "DEV3")
            .await
            .unwrap();
        assert_eq!(
            dev3.labels,
            Some(Labels::from([("board".into(), "hdc".into())]))
        );
        assert_eq!(dev3.enrolled_at, None);
        let report = DeviceGatewayAdapterImpl
            .get(&context, "NOPE")
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NotFound));
    }

    #[tokio::test]
    async fn lists_reads_and_deletes_factory_devices() {
        let (gateway, context) = TestGateway::serve().await;
        {
            let mut state = gateway.0.lock().unwrap();
            for serial in ["F2", "F1", "F3"] {
                state.factory_devices.insert(serial.into(), 1);
            }
            state.factory_owners.insert("F1".into(), "acme".into());
            state
                .factory_created_at
                .insert("F1".into(), "2026-11-20T14:30:00Z".into());
            state
                .factory_labels
                .insert("F3".into(), [("batch".into(), "b7".into())].into());
        }
        let all = DeviceGatewayAdapterImpl
            .list_factory(&context, &Labels::new())
            .await
            .unwrap();
        let names: Vec<_> = all.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["F1", "F2", "F3"]);
        assert_eq!(
            all[0].owner.as_deref(),
            Some("urn:sntns:iam:test:acme:account")
        );
        assert_eq!(all[1].owner, None);
        assert_eq!(all[0].created_at, time_of(Some("2026-11-20T14:30:00Z")));
        assert!(all[0].created_at.is_some());
        assert_eq!(all[1].created_at, None);
        let batch = DeviceGatewayAdapterImpl
            .list_factory(&context, &Labels::from([("batch".into(), "b7".into())]))
            .await
            .unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].name, "F3");

        let f3 = DeviceGatewayAdapterImpl
            .get_factory(&context, "F3")
            .await
            .unwrap();
        assert_eq!(
            f3.labels,
            Some(Labels::from([("batch".into(), "b7".into())]))
        );
        assert_eq!(f3.certificate.as_deref(), Some("urn:test:certificate-F3-1"));

        let revoked = DeviceGatewayAdapterImpl
            .delete_factory(&context, "F3")
            .await
            .unwrap();
        assert_eq!(revoked, ["urn:test:certificate-F3-1"]);
        for report in [
            DeviceGatewayAdapterImpl
                .get_factory(&context, "F3")
                .await
                .unwrap_err(),
            DeviceGatewayAdapterImpl
                .delete_factory(&context, "F3")
                .await
                .unwrap_err(),
        ] {
            assert!(matches!(report.current_context(), Error::NotFound));
        }
    }

    #[tokio::test]
    async fn deletes_a_device() {
        let (gateway, context) = TestGateway::serve().await;
        DeviceGatewayAdapterImpl
            .delete(&context, "DEV3")
            .await
            .unwrap();
        assert!(!gateway.0.lock().unwrap().devices.contains_key("DEV3"));
        let report = DeviceGatewayAdapterImpl
            .delete(&context, "DEV3")
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NotFound));
    }
}
