use error_stack::{Report, ResultExt};
use remora_context::model::ResolvedContext;
use remora_device::{
    adapter::gateway::{DeviceGatewayAdapter, Error, Result},
    model::Labels,
};
use remora_platform_http::{model, status_summary, Code, Connection, Gateway, Status};

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

    async fn labels(&self, context: &ResolvedContext, name: &str) -> Result<Labels> {
        let gateway = gateway(context).await?;
        let descriptor = gateway
            .get::<model::GetDeviceResponse>(gateway.url(["remora", "v1", "device", name]))
            .await
            .map_err(classify)?
            .device_descriptor
            .ok_or_else(|| Report::new(Error::Call).attach("empty device descriptor"))?;
        Ok(descriptor.labels.into_iter().collect())
    }
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
