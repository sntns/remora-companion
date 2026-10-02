mod bootstrap;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "remora-etcher",
    version,
    about = "Standalone provisioning and flashing tool for Remora devices"
)]
struct Options {
    #[command(subcommand)]
    command: Commands,

    /// Increase verbosity (-v, -vv, -vvv).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Override the tracing filter directly (e.g. "remora_flash=debug").
    #[arg(long, global = true)]
    trace: Option<String>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// List and inspect disks.
    #[command(subcommand)]
    Disk(remora_disk_application_transport_cli::Command),

    /// Flash an image to a disk, bmaptool-style.
    Flash(remora_flash_application_transport_cli::Command),

    /// Inspect and list partitions of an image or device, and inject files
    /// into (or create directories inside) their filesystems.
    #[command(subcommand)]
    Image(remora_image_application_transport_cli::Command),

    /// Build and inspect squashfs images.
    #[command(subcommand)]
    Squashfs(remora_squashfs_application_transport_cli::Command),

    /// Build identity.squashfs and/or inject it into an image's shared
    /// partition.
    #[command(subcommand)]
    Identity(remora_identity_application_transport_cli::Command),

    /// Build config.ext4 and/or upload files into an image's shared
    /// partition.
    #[command(subcommand)]
    Config(remora_config_application_transport_cli::Command),

    /// Convert a whole-disk image to/from a plain raw image, unwrapping or
    /// producing whatever container format its path names (`.qcow2`,
    /// `.gz`) -- no external tool (`qemu-img`, `gzip`) involved.
    #[command(subcommand)]
    Convert(remora_convert_application_transport_cli::Command),

    /// Run a batch recipe: several of the above operations as one call,
    /// with one unified progress stream and no cleanup/rollback if a step
    /// fails partway through.
    #[command(subcommand)]
    Batch(remora_batch_application_transport_cli::Command),

    /// Manufacture a device against sntns-platform's factory-device
    /// endpoint and inject its credential into an image. Not a `batch`
    /// step -- a network call with per-unit output, unlike batch's
    /// local/reproducible steps.
    #[command(subcommand)]
    Factory(remora_factory_application_transport_cli::Command),
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let options = Options::parse();
    init_tracing(&options);

    let services = bootstrap::wire().await;

    // Print the full error-stack chain (`{:?}`), not just the top context's
    // message (`{}`) — the useful detail (e.g. *why* a flash was refused)
    // usually lives a few `change_context` hops down, and `Display` on a
    // type-erased `Box<dyn Error>` only ever shows the outermost one.
    match options.command {
        Commands::Disk(cmd) => {
            if let Err(report) =
                remora_disk_application_transport_cli::run(cmd, &services.disk).await
            {
                fail(report);
            }
        }
        Commands::Flash(cmd) => {
            if let Err(report) =
                remora_flash_application_transport_cli::run(cmd, &services.disk, &services.flash)
                    .await
            {
                fail(report);
            }
        }
        Commands::Image(cmd) => {
            if let Err(report) =
                remora_image_application_transport_cli::run(cmd, &services.image).await
            {
                fail(report);
            }
        }
        Commands::Squashfs(cmd) => {
            if let Err(report) =
                remora_squashfs_application_transport_cli::run(cmd, &services.squashfs).await
            {
                fail(report);
            }
        }
        Commands::Identity(cmd) => {
            if let Err(report) =
                remora_identity_application_transport_cli::run(cmd, &services.identity).await
            {
                fail(report);
            }
        }
        Commands::Config(cmd) => {
            if let Err(report) =
                remora_config_application_transport_cli::run(cmd, &services.config).await
            {
                fail(report);
            }
        }
        Commands::Convert(cmd) => {
            if let Err(report) =
                remora_convert_application_transport_cli::run(cmd, &services.convert).await
            {
                fail(report);
            }
        }
        Commands::Batch(cmd) => {
            if let Err(report) =
                remora_batch_application_transport_cli::run(cmd, &services.batch).await
            {
                fail(report);
            }
        }
        Commands::Factory(cmd) => {
            if let Err(report) =
                remora_factory_application_transport_cli::run(cmd, &services.factory).await
            {
                fail(report);
            }
        }
    }
}

fn fail(report: impl std::fmt::Debug) -> ! {
    tracing::error!("{report:?}");
    eprintln!("error: {report:?}");
    std::process::exit(1);
}

fn init_tracing(options: &Options) {
    use tracing_subscriber::EnvFilter;

    let filter = if let Some(trace) = &options.trace {
        EnvFilter::new(trace)
    } else {
        let level = match options.verbose {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        };
        EnvFilter::new(format!("remora_etcher={level}"))
    };

    tracing_subscriber::fmt().with_env_filter(filter).init();
}
