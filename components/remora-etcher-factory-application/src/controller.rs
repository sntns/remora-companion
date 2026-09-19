use std::path::Path;

use error_stack::{Report, ResultExt};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use remora_etcher_factory::{
    adapter::FactoryProvisioningAdapterService,
    application::{Error, FactoryServiceInterface, Result},
    model::FactoryCredential,
};
use remora_etcher_progress::OperationContext;

use crate::yaml;

/// The factory vertical's use case: generate a device keypair + CSR
/// locally (via the injected keygen -- no adapter seam for this, unlike
/// most of this workspace's I/O: `rcgen` is pure computation, not
/// infrastructure access with a swap-worthy alternative), request a
/// factory credential from the injected `FactoryProvisioningAdapterService`,
/// and render the result as `remora-factory.yaml` at `output`. Deliberately
/// has no dependency on `ImageService` -- see `FactoryServiceInterface`'s
/// own doc comment for why bundling into an image is a separate, later
/// step (`identity create`), not this controller's job.
pub struct FactoryControllerImpl {
    provisioning: FactoryProvisioningAdapterService,
}

impl FactoryControllerImpl {
    pub fn new(provisioning: FactoryProvisioningAdapterService) -> Self {
        Self { provisioning }
    }
}

#[async_trait::async_trait]
impl FactoryServiceInterface for FactoryControllerImpl {
    async fn provision(
        &self,
        device_name: &str,
        api_url: &str,
        api_key: &str,
        access_url_override: Option<&str>,
        output: &Path,
        ctx: &OperationContext,
    ) -> Result<()> {
        if ctx.cancel.is_cancelled() {
            return Err(Report::new(Error::Cancelled));
        }

        ctx.sink.phase("generating device keypair");
        let key_pair = KeyPair::generate().change_context(Error::Keygen)?;
        let private_key_der = key_pair.serialize_der();

        // The CSR's subject is ignored server-side (identity comes from
        // `device_name` alone), but matching CN to the serial keeps the
        // artifacts readable later.
        let mut csr_params = CertificateParams::default();
        csr_params.distinguished_name = DistinguishedName::new();
        csr_params
            .distinguished_name
            .push(DnType::CommonName, device_name);
        let csr = csr_params
            .serialize_request(&key_pair)
            .change_context(Error::Csr)?;
        let csr_der = csr.der().as_ref().to_vec();

        if ctx.cancel.is_cancelled() {
            return Err(Report::new(Error::Cancelled));
        }
        ctx.sink.phase("requesting factory device credential");
        let identity = self
            .provisioning
            .provision(api_url, api_key, device_name, &csr_der)
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

        let credential = FactoryCredential {
            private_key_der,
            certificate_der: identity.certificate_der,
            certificate_authority_der: identity.certificate_authority_der,
            server_certificate_authority_der: identity.server_certificate_authority_der,
            key_id: identity.key_id,
        };

        ctx.sink.phase("rendering remora-factory.yaml");
        let rendered = yaml::render(&credential, &access_url)?;

        ctx.sink.phase("writing output");
        std::fs::write(output, rendered)
            .change_context_lazy(|| Error::WriteOutput(output.to_path_buf()))?;

        ctx.sink.phase("done");
        Ok(())
    }
}
