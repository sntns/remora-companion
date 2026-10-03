use error_stack::{Report, ResultExt};
use remora_context::model::ResolvedContext;
use remora_device::{
    adapter::gateway::{DeviceGatewayAdapter, Error, Result},
    model::Labels,
};
use remora_platform_grpc::{
    connect, sntns::service::remora::v1 as pb, status_summary, Connection, GatewayChannel,
};

type Devices = pb::device_service_client::DeviceServiceClient<GatewayChannel>;

/// The remora gateway's DeviceService over gRPC.
pub struct DeviceGatewayAdapterImpl;

async fn client(context: &ResolvedContext) -> Result<Devices> {
    let channel = connect(&Connection::for_resolved(context))
        .await
        .change_context(Error::Unreachable)?;
    Ok(Devices::new(channel))
}

fn classify(status: tonic::Status) -> Report<Error> {
    use tonic::Code;
    let error = match status.code() {
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
        let response = client(context)
            .await?
            .list_devices(pb::DeviceServiceListDevicesRequest {
                device_labels: labels.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            })
            .await
            .map_err(classify)?
            .into_inner();
        let mut names: Vec<_> = response
            .device_summaries
            .into_iter()
            .filter_map(|summary| summary.resource.map(|resource| resource.name))
            .filter(|name| !name.is_empty())
            .collect();
        names.sort();
        Ok(names)
    }

    async fn labels(&self, context: &ResolvedContext, name: &str) -> Result<Labels> {
        let descriptor = client(context)
            .await?
            .get_device(pb::DeviceServiceGetDeviceRequest {
                device_name: name.to_owned(),
            })
            .await
            .map_err(classify)?
            .into_inner()
            .device_descriptor
            .ok_or_else(|| Report::new(Error::Call).attach("empty device descriptor"))?;
        Ok(descriptor.labels.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use remora_ota_adapter_grpc::test_gateway::TestGateway;

    use super::*;

    #[tokio::test]
    async fn lists_by_label_and_reads_labels() {
        // The OTA fake serves DeviceService too (deployments need targets).
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
        assert_eq!(
            DeviceGatewayAdapterImpl
                .labels(&context, "DEV3")
                .await
                .unwrap(),
            Labels::from([("board".into(), "hdc".into())])
        );
        let report = DeviceGatewayAdapterImpl
            .labels(&context, "NOPE")
            .await
            .unwrap_err();
        assert!(matches!(report.current_context(), Error::NotFound));
    }
}
