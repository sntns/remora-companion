use std::sync::Arc;

use error_stack::{Report, ResultExt};
use remora_context::{application::ContextService, model::ContextOverride};
use remora_device::{
    adapter::gateway::{self, DeviceGatewayAdapterService},
    application::{DeviceServiceInterface, Error, Result},
    model::{Device, FactoryDevice, Labels},
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
                .map(|name| Device {
                    name,
                    labels: None,
                    enrolled_at: None,
                })
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
                let device = gateway.get(&context, &name).await;
                (index, name, device)
            });
        }
        let mut devices = Vec::with_capacity(tasks.len());
        while let Some(joined) = tasks.join_next().await {
            let (index, name, device) = joined.map_err(|e| {
                Report::new(Error::List).attach(format!("a device fetch failed: {e}"))
            })?;
            let device = match device {
                Ok(device) => device,
                // Deleted between the listing and now: it is no longer one
                // of the account's devices, not a reason to fail the rest.
                Err(report) if matches!(report.current_context(), gateway::Error::NotFound) => {
                    continue
                }
                Err(report) => return Err(report.change_context(Error::Get(name))),
            };
            devices.push((index, Device { name, ..device }));
        }
        devices.sort_by_key(|(index, _)| *index);
        Ok(devices.into_iter().map(|(_, device)| device).collect())
    }

    async fn delete(&self, over: Option<&ContextOverride>, name: &str) -> Result<()> {
        let context = self
            .contexts
            .resolve(over)
            .await
            .change_context(Error::Context)?;
        self.gateway
            .delete(&context, name)
            .await
            .map_err(|report| match report.current_context() {
                gateway::Error::NotFound => report.change_context(Error::NotFound(name.to_owned())),
                _ => report.change_context(Error::Delete(name.to_owned())),
            })
    }

    async fn list_factory(
        &self,
        over: Option<&ContextOverride>,
        labels: &Labels,
        without_device: bool,
    ) -> Result<Vec<FactoryDevice>> {
        let context = self
            .contexts
            .resolve(over)
            .await
            .change_context(Error::Context)?;
        let made = self
            .gateway
            .list_factory(&context, labels)
            .await
            .change_context(Error::ListFactory)?;
        if !without_device {
            return Ok(made);
        }
        // Every device, not those carrying `labels`: a factory record's
        // labels are not its device's.
        let enrolled: std::collections::HashSet<_> = self
            .gateway
            .list(&context, &Labels::new())
            .await
            .change_context(Error::List)?
            .into_iter()
            .collect();
        Ok(made
            .into_iter()
            .filter(|device| !enrolled.contains(&device.name))
            .collect())
    }

    async fn get_factory(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
    ) -> Result<FactoryDevice> {
        let context = self
            .contexts
            .resolve(over)
            .await
            .change_context(Error::Context)?;
        self.gateway
            .get_factory(&context, name)
            .await
            .map_err(|report| match report.current_context() {
                gateway::Error::NotFound => {
                    report.change_context(Error::FactoryNotFound(name.to_owned()))
                }
                _ => report.change_context(Error::GetFactory(name.to_owned())),
            })
    }

    async fn delete_factory(
        &self,
        over: Option<&ContextOverride>,
        name: &str,
    ) -> Result<Vec<String>> {
        let context = self
            .contexts
            .resolve(over)
            .await
            .change_context(Error::Context)?;
        self.gateway
            .delete_factory(&context, name)
            .await
            .map_err(|report| match report.current_context() {
                gateway::Error::NotFound => {
                    report.change_context(Error::FactoryNotFound(name.to_owned()))
                }
                _ => report.change_context(Error::DeleteFactory(name.to_owned())),
            })
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
    use remora_device_adapter_http::DeviceGatewayAdapterImpl;
    use remora_platform_http::test_gateway::TestGateway;

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

        async fn get(&self, context: &ResolvedContext, name: &str) -> gateway::Result<Device> {
            DeviceGatewayAdapterImpl.get(context, name).await
        }

        async fn delete(&self, context: &ResolvedContext, name: &str) -> gateway::Result<()> {
            DeviceGatewayAdapterImpl.delete(context, name).await
        }

        async fn list_factory(
            &self,
            context: &ResolvedContext,
            labels: &Labels,
        ) -> gateway::Result<Vec<FactoryDevice>> {
            DeviceGatewayAdapterImpl.list_factory(context, labels).await
        }

        async fn get_factory(
            &self,
            context: &ResolvedContext,
            name: &str,
        ) -> gateway::Result<FactoryDevice> {
            DeviceGatewayAdapterImpl.get_factory(context, name).await
        }

        async fn delete_factory(
            &self,
            context: &ResolvedContext,
            name: &str,
        ) -> gateway::Result<Vec<String>> {
            DeviceGatewayAdapterImpl.delete_factory(context, name).await
        }
    }

    /// The use case, logged in to `resolved`'s context through `adapter`.
    async fn wired(
        dir: &std::path::Path,
        resolved: ResolvedContext,
        adapter: impl DeviceGatewayAdapter + 'static,
    ) -> DeviceControllerImpl {
        let contexts = ContextControllerImpl::new(
            ContextStoreAdapterService::new(FileContextStoreImpl::new(dir)),
            CredentialStoreAdapterService::new(FileCredentialStoreImpl::new(dir)),
            PlatformSessionAdapterService::new(Platform),
        );
        contexts
            .create(resolved.context, RoleOverride::Keep, false)
            .await
            .unwrap();
        contexts
            .login(None, resolved.credentials, RoleOverride::Keep)
            .await
            .unwrap();
        DeviceControllerImpl::new(
            ContextService::new(contexts),
            DeviceGatewayAdapterService::new(adapter),
        )
    }

    #[tokio::test]
    async fn a_device_deleted_while_listing_is_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let (gateway, resolved) = TestGateway::serve().await;
        let devices = wired(dir.path(), resolved, DeletedAfterListing(gateway)).await;

        let listed = devices
            .list(None, &Labels::from([("board".into(), "rp5".into())]), true)
            .await
            .unwrap();
        let names: Vec<_> = listed.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["BROKEN", "DEV1"]);
    }

    #[tokio::test]
    async fn factory_devices_without_a_device_are_those_not_enrolled() {
        let dir = tempfile::tempdir().unwrap();
        let (gateway, resolved) = TestGateway::serve().await;
        {
            let mut state = gateway.0.lock().unwrap();
            // DEV3 enrolled; NEW1 and NEW2 not yet, NEW2 labelled.
            for serial in ["DEV3", "NEW1", "NEW2"] {
                state.factory_devices.insert(serial.into(), 1);
                state
                    .factory_labels
                    .insert(serial.into(), [("batch".into(), "b7".into())].into());
            }
            state.factory_labels.remove("NEW1");
        }
        let devices = wired(dir.path(), resolved, DeviceGatewayAdapterImpl).await;
        let names = |listed: Vec<FactoryDevice>| -> Vec<String> {
            listed.into_iter().map(|device| device.name).collect()
        };

        let all = devices.list_factory(None, &Labels::new(), false).await;
        assert_eq!(names(all.unwrap()), ["DEV3", "NEW1", "NEW2"]);
        let pending = devices.list_factory(None, &Labels::new(), true).await;
        assert_eq!(names(pending.unwrap()), ["NEW1", "NEW2"]);
        // DEV3's device lacks `batch`: it still counts as enrolled.
        let batch = Labels::from([("batch".into(), "b7".into())]);
        let pending = devices.list_factory(None, &batch, true).await;
        assert_eq!(names(pending.unwrap()), ["NEW2"]);

        let report = devices.get_factory(None, "NOPE").await.unwrap_err();
        assert!(matches!(
            report.current_context(),
            Error::FactoryNotFound(_)
        ));
        assert_eq!(
            devices.delete_factory(None, "NEW1").await.unwrap(),
            ["urn:test:certificate-NEW1-1"]
        );
        let pending = devices.list_factory(None, &Labels::new(), true).await;
        assert_eq!(names(pending.unwrap()), ["NEW2"]);
    }
}
