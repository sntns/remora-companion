use std::sync::Arc;

use error_stack::{Report, ResultExt};
use remora_context::{application::ContextService, model::ContextOverride};
use remora_device::{
    adapter::gateway::{self, DeviceGatewayAdapterService},
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
        let context = self
            .contexts
            .resolve(over)
            .await
            .change_context(Error::Context)?;
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
                // The semaphore is never closed: a permit is always granted.
                let _permit = permits.acquire_owned().await.ok();
                let labels = gateway.labels(&context, &name).await;
                (index, name, labels)
            });
        }
        let mut devices = Vec::with_capacity(tasks.len());
        while let Some(joined) = tasks.join_next().await {
            let (index, name, labels) = joined.map_err(|e| {
                Report::new(Error::List).attach(format!("a label fetch failed: {e}"))
            })?;
            let labels = match labels {
                Ok(labels) => labels,
                // Deleted between the listing and now: it is no longer one
                // of the account's devices, not a reason to fail the rest.
                Err(report) if matches!(report.current_context(), gateway::Error::NotFound) => {
                    continue
                }
                Err(report) => return Err(report.change_context(Error::Get(name))),
            };
            devices.push((
                index,
                Device {
                    name,
                    labels: Some(labels),
                },
            ));
        }
        devices.sort_by_key(|(index, _)| *index);
        Ok(devices.into_iter().map(|(_, device)| device).collect())
    }
}

#[cfg(test)]
mod tests {
    use remora_context::{
        adapter::{
            credentials::CredentialStoreAdapterService,
            platform::{self, PlatformSessionAdapter, PlatformSessionAdapterService},
            store::ContextStoreAdapterService,
        },
        application::ContextServiceInterface,
        model::{Context, Credentials, Principal, ResolvedContext, RoleOverride},
    };
    use remora_context_adapter_file::{FileContextStoreImpl, FileCredentialStoreImpl};
    use remora_context_application::ContextControllerImpl;
    use remora_device::adapter::gateway::DeviceGatewayAdapter;
    use remora_device_adapter_grpc::DeviceGatewayAdapterImpl;
    use remora_ota_adapter_grpc::test_gateway::TestGateway;

    use super::*;

    struct Platform;

    #[async_trait::async_trait]
    impl PlatformSessionAdapter for Platform {
        async fn whoami(&self, _: &Context, _: &Credentials) -> platform::Result<Principal> {
            Ok(Principal {
                user_urn: "urn:user:ada".into(),
                user_name: "ada".into(),
                account_name: None,
            })
        }

        async fn acting_account(
            &self,
            _: &Context,
            _: &Credentials,
            _: &str,
        ) -> platform::Result<Option<String>> {
            Ok(None)
        }
    }

    /// The real adapter, with DEV2 deleted on the platform right after
    /// every listing: the race `rmra device ls` must ride out.
    struct DeletedAfterListing(TestGateway);

    #[async_trait::async_trait]
    impl DeviceGatewayAdapter for DeletedAfterListing {
        async fn list(
            &self,
            context: &ResolvedContext,
            labels: &Labels,
        ) -> gateway::Result<Vec<String>> {
            let names = DeviceGatewayAdapterImpl.list(context, labels).await?;
            self.0 .0.lock().unwrap().devices.remove("DEV2");
            Ok(names)
        }

        async fn labels(&self, context: &ResolvedContext, name: &str) -> gateway::Result<Labels> {
            DeviceGatewayAdapterImpl.labels(context, name).await
        }
    }

    #[tokio::test]
    async fn a_device_deleted_while_listing_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let (gateway, resolved) = TestGateway::serve().await;
        let contexts = ContextControllerImpl::new(
            ContextStoreAdapterService::new(FileContextStoreImpl::new(dir.path())),
            CredentialStoreAdapterService::new(FileCredentialStoreImpl::new(dir.path())),
            PlatformSessionAdapterService::new(Platform),
        );
        contexts
            .create(
                resolved.context,
                remora_context::model::RoleOverride::Keep,
                false,
            )
            .await
            .unwrap();
        contexts
            .login(None, resolved.credentials, RoleOverride::Keep)
            .await
            .unwrap();
        let devices = DeviceControllerImpl::new(
            ContextService::new(contexts),
            DeviceGatewayAdapterService::new(DeletedAfterListing(gateway)),
        );

        let listed = devices
            .list(None, &Labels::from([("board".into(), "rp5".into())]), true)
            .await
            .unwrap();
        let names: Vec<_> = listed.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["BROKEN", "DEV1"]);
    }
}
