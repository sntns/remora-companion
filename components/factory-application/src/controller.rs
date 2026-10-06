use std::path::Path;

use error_stack::{Report, ResultExt};
use remora_context::{application::ContextService, model::ContextOverride};
use remora_factory::{
    adapter::{
        credential::CredentialWriterAdapterService, key::DeviceKeyAdapterService,
        provisioning::FactoryProvisioningAdapterService,
    },
    application::{Error, FactoryServiceInterface, Result},
    model::{DeviceSerial, FactoryCredential, ProvisionedDevice},
};
use remora_progress::OperationContext;

/// The factory vertical's use case: have the injected `DeviceKeyAdapterService`
/// generate a device keypair + CSR, request a factory credential from the
/// injected `FactoryProvisioningAdapterService` as the selected context, and
/// hand the result to the injected `CredentialWriterAdapterService` to land
/// as `remora-factory.yaml` at `output`. Deliberately has no dependency on
/// `ImageService` -- see `FactoryServiceInterface`'s own doc comment for why
/// bundling into an image is a separate, later step (`identity create`),
/// not this controller's job.
pub struct FactoryControllerImpl {
    contexts: ContextService,
    keys: DeviceKeyAdapterService,
    provisioning: FactoryProvisioningAdapterService,
    credentials: CredentialWriterAdapterService,
}

impl FactoryControllerImpl {
    pub fn new(
        contexts: ContextService,
        keys: DeviceKeyAdapterService,
        provisioning: FactoryProvisioningAdapterService,
        credentials: CredentialWriterAdapterService,
    ) -> Self {
        Self {
            contexts,
            keys,
            provisioning,
            credentials,
        }
    }
}

#[async_trait::async_trait]
impl FactoryServiceInterface for FactoryControllerImpl {
    async fn provision(
        &self,
        over: Option<&ContextOverride>,
        serial: &DeviceSerial,
        access_url_override: Option<&str>,
        output: &Path,
        ctx: &OperationContext,
    ) -> Result<ProvisionedDevice> {
        // First: a context that isn't logged in fails before any key exists.
        let context = self.contexts.resolve(over).await.map_err(|report| {
            let message = report.current_context().to_string();
            report.change_context(Error::Context(message))
        })?;
        if ctx.cancel.is_cancelled() {
            return Err(Report::new(Error::Cancelled));
        }
        ctx.sink.phase("generating device keypair");
        // The CSR's subject is ignored server-side (identity comes from
        // `serial` alone), but matching CN to the serial keeps the
        // artifacts readable later -- when it's known up front, that is: a
        // policy-allocated serial only exists once the platform answers.
        let common_name = match serial {
            DeviceSerial::Explicit { device_name, .. } => Some(device_name.as_str()),
            DeviceSerial::FromPolicy(_) => None,
        };
        let key = self
            .keys
            .generate(common_name)
            .await
            .change_context(Error::Keygen)?;

        if ctx.cancel.is_cancelled() {
            return Err(Report::new(Error::Cancelled));
        }
        ctx.sink.phase("requesting factory device credential");
        let identity = self
            .provisioning
            .provision(&context, serial, &key.csr_der)
            .await
            .change_context(Error::Provision)?;

        // The platform always knows which access tier a device should use;
        // an empty response means a deployment hasn't configured one yet,
        // which is the *only* case the override exists for -- see
        // `FactoryServiceInterface::provision`'s doc comment.
        let access_url = if !identity.access_url.is_empty() {
            identity.access_url.clone()
        } else if let Some(override_url) = access_url_override {
            override_url.to_string()
        } else {
            return Err(Report::new(Error::MissingAccessUrl));
        };

        let device = ProvisionedDevice {
            serial_number: identity.serial_number,
            factory_device_name: identity.factory_device_name,
        };
        let credential = FactoryCredential {
            private_key: key.private_key,
            certificate_der: identity.certificate_der,
            certificate_authority_der: identity.certificate_authority_der,
            server_certificate_authority_der: identity.server_certificate_authority_der,
            key_id: identity.key_id,
        };

        ctx.sink.phase("writing remora-factory.yaml");
        self.credentials
            .write(&credential, &access_url, output)
            .await
            .change_context_lazy(|| Error::WriteOutput(output.to_path_buf()))?;

        ctx.sink.phase("done");
        Ok(device)
    }
}
