use std::path::Path;

use error_stack::{Report, ResultExt};
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
use remora_etcher_factory::{
    adapter::FactoryProvisioningAdapterService,
    application::{Error, FactoryServiceInterface, Result},
};
use remora_etcher_image::{application::ImageService, model::PartitionRole};
use remora_etcher_progress::OperationContext;

/// The factory vertical's use case: generate a device keypair + CSR
/// locally (via the injected keygen -- no adapter seam for this, unlike
/// most of this workspace's I/O: `rcgen` is pure computation, not
/// infrastructure access with a swap-worthy alternative), request a
/// factory credential from the injected `FactoryProvisioningAdapterService`,
/// and inject everything the device needs into the image's shared
/// partition via the injected `ImageService` (this vertical's
/// cross-vertical dependency, same pattern as identity/config).
pub struct FactoryControllerImpl {
    provisioning: FactoryProvisioningAdapterService,
    image: ImageService,
}

impl FactoryControllerImpl {
    pub fn new(provisioning: FactoryProvisioningAdapterService, image: ImageService) -> Self {
        Self {
            provisioning,
            image,
        }
    }
}

#[async_trait::async_trait]
impl FactoryServiceInterface for FactoryControllerImpl {
    async fn provision(
        &self,
        device_name: &str,
        access_url: &str,
        gateway_url: &str,
        api_key: &str,
        image: &Path,
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
            .provision(gateway_url, api_key, device_name, &csr_der)
            .await
            .change_context(Error::Provision)?;

        ctx.sink.phase("writing credential into the image");
        self.image
            .ensure_dir_by_role(image, PartitionRole::Shared, "/remora", 0o755)
            .await
            .change_context(Error::Image)?;
        self.image
            .ensure_dir_by_role(image, PartitionRole::Shared, "/remora/factory", 0o755)
            .await
            .change_context(Error::Image)?;

        let files: [(&str, &[u8], u16); 4] = [
            ("/remora/factory/private_key.der", &private_key_der, 0o600),
            (
                "/remora/factory/certificate.der",
                &identity.certificate_der,
                0o644,
            ),
            (
                "/remora/factory/factory_ca.der",
                &identity.certificate_authority_der,
                0o644,
            ),
            (
                "/remora/factory/server_ca.der",
                &identity.server_certificate_authority_der,
                0o644,
            ),
        ];
        for (dest_path, contents, mode) in files {
            self.image
                .inject_by_role(image, PartitionRole::Shared, dest_path, contents, mode)
                .await
                .change_context(Error::Image)?;
        }
        self.image
            .inject_by_role(
                image,
                PartitionRole::Shared,
                "/remora/factory/keyid",
                identity.certificate_reference.urn.as_bytes(),
                0o644,
            )
            .await
            .change_context(Error::Image)?;
        self.image
            .inject_by_role(
                image,
                PartitionRole::Shared,
                "/remora/factory/access_url",
                access_url.as_bytes(),
                0o644,
            )
            .await
            .change_context(Error::Image)?;

        ctx.sink.phase("done");
        Ok(())
    }
}
