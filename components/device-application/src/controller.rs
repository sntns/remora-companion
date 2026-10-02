use std::sync::Arc;

use error_stack::ResultExt;
use remora_context::{application::ContextService, model::ContextOverride};
use remora_device::{
    adapter::gateway::DeviceGatewayAdapterService,
    application::{DeviceServiceInterface, Error, Result},
    model::{Device, Labels},
};
use tokio::{sync::Semaphore, task::JoinSet};

/// How many devices' labels are fetched at once: the gateway lists names
/// only, so labels cost one call per device.
const CONCURRENCY: usize = 16;

/// The device vertical's use case.
pub struct DeviceControllerImpl {
    contexts: ContextService,
    gateway: DeviceGatewayAdapterService,
}

impl DeviceControllerImpl {
    pub fn new(contexts: ContextService, gateway: DeviceGatewayAdapterService) -> Self {
        Self { contexts, gateway }
    }
}

#[async_trait::async_trait]
impl DeviceServiceInterface for DeviceControllerImpl {
    async fn list(
        &self,
        over: Option<&ContextOverride>,
        labels: &Labels,
        with_labels: bool,
    ) -> Result<Vec<Device>> {
        let context = self.contexts.resolve(over).await.map_err(|report| {
            let message = report.current_context().to_string();
            report.change_context(Error::Context(message))
        })?;
        let names = self
            .gateway
            .list(&context, labels)
            .await
            .change_context(Error::List)?;
        if !with_labels {
            return Ok(names
                .into_iter()
                .map(|name| Device { name, labels: None })
                .collect());
        }

        let context = Arc::new(context);
        let permits = Arc::new(Semaphore::new(CONCURRENCY));
        let mut tasks = JoinSet::new();
        for (index, name) in names.into_iter().enumerate() {
            let (gateway, context, permits) =
                (self.gateway.clone(), context.clone(), permits.clone());
            tasks.spawn(async move {
                let _permit = permits.acquire_owned().await.expect("never closed");
                let labels = gateway
                    .labels(&context, &name)
                    .await
                    .change_context_lazy(|| Error::Get(name.clone()));
                (index, name, labels)
            });
        }
        let mut devices = Vec::with_capacity(tasks.len());
        while let Some(joined) = tasks.join_next().await {
            let (index, name, labels) = joined.expect("label fetches don't panic");
            devices.push((
                index,
                Device {
                    name,
                    labels: Some(labels?),
                },
            ));
        }
        devices.sort_by_key(|(index, _)| *index);
        Ok(devices.into_iter().map(|(_, device)| device).collect())
    }
}
