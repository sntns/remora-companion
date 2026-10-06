use std::path::PathBuf;

use error_stack::Report;
use remora_context::model::ContextOverride;
use remora_factory::{
    application::{Error, FactoryService, Result},
    model::DeviceSerial,
};
use remora_progress::OperationContext;

#[derive(clap::Subcommand)]
pub enum Command {
    /// Manufacture a device: generate a keypair + CSR, request a factory
    /// IDevID credential from sntns-platform as the selected context (its
    /// login and role choose the manufacturing account), and write it out as a
    /// `remora-factory.yaml`. That file is a plain identity input, the same
    /// as any other -- bundle it into an image with `identity create
    /// --inputs`, exactly like `config build`'s output. Also available as
    /// a `batch` step, though (unlike batch's other steps) it's a network
    /// call against a specific platform environment with per-unit output,
    /// not something a recipe can replay locally/offline.
    Provision {
        /// Let the platform allocate a fresh serial from this policy (e.g.
        /// `hubs`) -- the normal path, and the only way to be sure the
        /// serial was never used. The issued serial is printed on stdout,
        /// for the label.
        #[arg(long, required_unless_present = "device_name")]
        serial_policy: Option<String>,

        /// Durable hardware serial chosen outside the platform -- the
        /// device's identity, not a resource name scoped to whoever
        /// currently owns it. Refused if it was already manufactured,
        /// unless `--force`.
        #[arg(long, conflicts_with = "serial_policy")]
        device_name: Option<String>,

        /// Where to write the resulting `remora-factory.yaml`.
        #[arg(long)]
        output: PathBuf,

        /// Re-sign a `--device-name` that was already manufactured (a
        /// mis-flashed board, a reused test unit); the platform revokes its
        /// previous IDevID. Not allowed with `--serial-policy`, which always
        /// allocates a new serial.
        #[arg(long, requires = "device_name", conflicts_with = "serial_policy")]
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

/// The context's login (or the role it acts as) needs the
/// `remora::create-factory-device` action, and `remora::use-serial-number-
/// policy` on the policy it allocates from.
pub async fn run(
    command: Command,
    service: &FactoryService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    match command {
        Command::Provision {
            serial_policy,
            device_name,
            output,
            force,
            access_url,
        } => {
            let serial = DeviceSerial::from_parts(device_name, serial_policy, force)
                .map_err(|e| Report::new(Error::InvalidSerial(e)))?;
            let (sink, stream) = remora_progress::channel();
            let follow = remora_tui::follow(stream);
            let ctx = OperationContext::new(sink, remora_progress::cancelled_by_ctrl_c());
            let result = service
                .provision(over, &serial, access_url.as_deref(), &output, &ctx)
                .await;
            drop(ctx);
            follow.finish(&result).await;
            let device = result?;

            // The serial alone on stdout, so a label printer (or a script)
            // can consume it; the human-readable summary goes to stderr.
            remora_tui::success(format!(
                "Provisioned {} {}, credential in {}",
                remora_tui::accent(&device.serial_number),
                remora_tui::dim(format!("({})", device.factory_device_name)),
                remora_tui::accent(output.display())
            ));
            println!("{}", device.serial_number);
            Ok(())
        }
    }
}
