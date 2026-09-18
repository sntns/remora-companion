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

    /// Override the tracing filter directly (e.g. "remora_etcher_flash=debug").
    #[arg(long, global = true)]
    trace: Option<String>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// List and inspect disks.
    #[command(subcommand)]
    Disk(remora_etcher_disk_application_transport_cli::Command),

    /// Flash an image to a disk, bmaptool-style.
    Flash(remora_etcher_flash_application_transport_cli::Command),

    /// Inspect and list partitions of an image or device, and inject files
    /// into (or create directories inside) their filesystems.
    #[command(subcommand)]
    Image(remora_etcher_image_application_transport_cli::Command),

    /// Build and inspect squashfs images.
    #[command(subcommand)]
    Squashfs(remora_etcher_squashfs_application_transport_cli::Command),

    /// Build identity.squashfs and/or inject it into an image's shared
    /// partition.
    #[command(subcommand)]
    Identity(remora_etcher_identity_application_transport_cli::Command),

    /// Build config.ext4 and/or upload files into an image's shared
    /// partition.
    #[command(subcommand)]
    Config(remora_etcher_config_application_transport_cli::Command),

    /// Convert a whole-disk image to/from a plain raw image, unwrapping or
    /// producing whatever container format its path names (`.qcow2`,
    /// `.gz`) -- no external tool (`qemu-img`, `gzip`) involved.
    #[command(subcommand)]
    Convert(remora_etcher_convert_application_transport_cli::Command),
}

fn main() {
    let options = Options::parse();
    init_tracing(&options);

    let runtime = tokio::runtime::Runtime::new().expect("failed to start the wiring runtime");
    let services = runtime.block_on(bootstrap::wire());
    // Wiring is the only thing that needed async; every service call below
    // is plain synchronous Rust.
    drop(runtime);

    // Print the full error-stack chain (`{:?}`), not just the top context's
    // message (`{}`) — the useful detail (e.g. *why* a flash was refused)
    // usually lives a few `change_context` hops down, and `Display` on a
    // type-erased `Box<dyn Error>` only ever shows the outermost one.
    match options.command {
        Commands::Disk(cmd) => {
            if let Err(report) =
                remora_etcher_disk_application_transport_cli::run(cmd, &services.disk)
            {
                fail(report);
            }
        }
        Commands::Flash(cmd) => {
            if let Err(report) = remora_etcher_flash_application_transport_cli::run(
                cmd,
                &services.disk,
                &services.flash,
            ) {
                fail(report);
            }
        }
        Commands::Image(cmd) => {
            if let Err(report) =
                remora_etcher_image_application_transport_cli::run(cmd, &services.image)
            {
                fail(report);
            }
        }
        Commands::Squashfs(cmd) => {
            if let Err(report) =
                remora_etcher_squashfs_application_transport_cli::run(cmd, &services.squashfs)
            {
                fail(report);
            }
        }
        Commands::Identity(cmd) => {
            if let Err(report) =
                remora_etcher_identity_application_transport_cli::run(cmd, &services.identity)
            {
                fail(report);
            }
        }
        Commands::Config(cmd) => {
            if let Err(report) =
                remora_etcher_config_application_transport_cli::run(cmd, &services.config)
            {
                fail(report);
            }
        }
        Commands::Convert(cmd) => {
            if let Err(report) =
                remora_etcher_convert_application_transport_cli::run(cmd, &services.convert)
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
