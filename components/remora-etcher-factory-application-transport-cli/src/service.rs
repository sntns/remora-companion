use std::path::PathBuf;

use remora_etcher_factory::application::{FactoryService, Result};
use remora_etcher_progress::OperationContext;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Manufacture a device: generate a keypair + CSR, request a factory
    /// IDevID credential from sntns-platform, and inject it into the
    /// image's shared partition so the device can self-enroll at first
    /// boot. Not a `batch` step -- this is a network call against a
    /// specific platform environment with per-unit output, unlike batch's
    /// local/reproducible steps.
    Provision {
        /// Durable hardware serial -- the device's identity, not a
        /// resource name scoped to whoever currently owns it.
        #[arg(long)]
        device_name: String,

        /// Image file or block device to write the credential into.
        #[arg(long)]
        image: PathBuf,

        /// Access-tier URL embedded into the image for the device's own
        /// later runtime use (this command never calls it itself), e.g.
        /// https://remora.access.eu2.sntns.io/access/v1 (dev: :444).
        #[arg(long)]
        access_url: String,

        /// Base URL of the platform gateway, e.g. https://api.sntns.dev
        /// (dev) or the production regional equivalent.
        #[arg(long)]
        gateway_url: String,

        /// Platform API key, sent as `Authorization: X-SNTNS-API-KEY
        /// <key>`. The identity behind it needs an IAM policy allowing
        /// the `remora::create-factory-device` action (note the double
        /// colon) with a non-empty `resources` list.
        #[arg(long, env = "REMORA_FACTORY_API_KEY")]
        api_key: String,
    },
}

pub async fn run(command: Command, service: &FactoryService) -> Result<()> {
    match command {
        Command::Provision {
            device_name,
            image,
            access_url,
            gateway_url,
            api_key,
        } => {
            let (sink, stream) = remora_etcher_progress::channel();
            let printer = remora_etcher_progress::print_to_stderr(stream);
            let ctx = OperationContext::new(sink, tokio_util::sync::CancellationToken::new());
            let result = service
                .provision(
                    &device_name,
                    &access_url,
                    &gateway_url,
                    &api_key,
                    &image,
                    &ctx,
                )
                .await;
            drop(ctx);
            let _ = printer.await;
            result?;

            println!(
                "provisioned {device_name} and wrote its factory credential into {}:/remora/factory",
                image.display()
            );
            Ok(())
        }
    }
}
