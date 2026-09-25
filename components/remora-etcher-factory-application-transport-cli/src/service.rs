use std::path::PathBuf;

use remora_etcher_factory::application::{FactoryService, Result};
use remora_etcher_progress::OperationContext;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Manufacture a device: generate a keypair + CSR, request a factory
    /// IDevID credential from sntns-platform, and write it out as a
    /// `remora-factory.yaml`. That file is a plain identity input, the same
    /// as any other -- bundle it into an image with `identity create
    /// --inputs`, exactly like `config build`'s output. Also available as
    /// a `batch` step, though (unlike batch's other steps) it's a network
    /// call against a specific platform environment with per-unit output,
    /// not something a recipe can replay locally/offline.
    Provision {
        /// Durable hardware serial -- the device's identity, not a
        /// resource name scoped to whoever currently owns it.
        #[arg(long)]
        device_name: String,

        /// Where to write the resulting `remora-factory.yaml`.
        #[arg(long)]
        output: PathBuf,

        /// Base URL of the platform API, e.g. https://api.sntns.dev (dev)
        /// or the production regional equivalent.
        #[arg(long)]
        api_url: String,

        /// Platform API key, sent as `Authorization: X-SNTNS-API-KEY
        /// <key>`. The identity behind it needs an IAM policy allowing
        /// the `remora::create-factory-device` action (note the double
        /// colon) with a non-empty `resources` list.
        #[arg(long, env = "REMORA_FACTORY_API_KEY")]
        api_key: String,

        /// Delete any existing factory-device credential for `device_name`
        /// first (a no-op if there isn't one), instead of letting the
        /// platform reject a duplicate create. Use when re-manufacturing a
        /// serial that was already provisioned once.
        #[arg(long)]
        force: bool,

        /// Escape hatch, not the normal path: the platform's response
        /// always carries the access-tier URL a device should use. Only
        /// consulted when that response comes back empty, which means a
        /// deployment hasn't configured one yet -- treat a need for this
        /// as a platform bug to report, not a flag to reach for by habit.
        #[arg(long, hide = true)]
        access_url: Option<String>,
    },
}

pub async fn run(command: Command, service: &FactoryService) -> Result<()> {
    match command {
        Command::Provision {
            device_name,
            output,
            api_url,
            api_key,
            force,
            access_url,
        } => {
            let (sink, stream) = remora_etcher_progress::channel();
            let printer = remora_etcher_progress::print_to_stderr(stream);
            let ctx = OperationContext::new(sink, tokio_util::sync::CancellationToken::new());
            let result = service
                .provision(
                    &device_name,
                    &api_url,
                    &api_key,
                    access_url.as_deref(),
                    force,
                    &output,
                    &ctx,
                )
                .await;
            drop(ctx);
            let _ = printer.await;
            result?;

            println!(
                "provisioned {device_name} and wrote its factory credential to {}",
                output.display()
            );
            Ok(())
        }
    }
}
