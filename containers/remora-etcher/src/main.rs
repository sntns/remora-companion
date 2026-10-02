mod bootstrap;
mod completion;

use clap::{CommandFactory, Parser};

#[derive(Parser)]
#[command(
    name = "remora-etcher",
    version,
    about = "Standalone provisioning and flashing tool for Remora devices"
)]
struct Options {
    #[command(subcommand)]
    command: Commands,

    /// Increase verbosity (-v, -vv, -vvv). From -v on, errors are shown in
    /// full, with where each cause was raised.
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
    /// endpoint, writing its credential as a `remora-factory.yaml` to
    /// bundle into an image with `identity create`.
    #[command(subcommand)]
    Factory(remora_factory_application_transport_cli::Command),

    /// Update remora-etcher itself to its latest release.
    Update(remora_update_application_transport_cli::Args),

    /// Enable shell completion (commands, options, disks and paths with Tab).
    Completion(remora_completion::Args),
}

fn main() {
    // Shell completion: when the shell calls back with COMPLETE set, answer
    // and exit before anything else. Outside the async runtime on purpose:
    // the provider runs its own, and a runtime can't nest in another.
    remora_completion::install(completion::provide);
    clap_complete::CompleteEnv::with_factory(Options::command).complete();

    let options = Options::parse();
    init_tracing(&options);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the tokio runtime builds");
    let code = runtime.block_on(run(options));
    std::process::exit(code);
}

async fn run(options: Options) -> i32 {
    let verbose = options.verbose > 0;
    let services = bootstrap::wire().await;

    match options.command {
        Commands::Disk(cmd) => exit_on_error(
            remora_disk_application_transport_cli::run(cmd, &services.disk).await,
            verbose,
        ),
        Commands::Flash(cmd) => exit_on_error(
            remora_flash_application_transport_cli::run(cmd, &services.disk, &services.flash).await,
            verbose,
        ),
        Commands::Image(cmd) => exit_on_error(
            remora_image_application_transport_cli::run(cmd, &services.image).await,
            verbose,
        ),
        Commands::Squashfs(cmd) => exit_on_error(
            remora_squashfs_application_transport_cli::run(cmd, &services.squashfs).await,
            verbose,
        ),
        Commands::Identity(cmd) => exit_on_error(
            remora_identity_application_transport_cli::run(cmd, &services.identity).await,
            verbose,
        ),
        Commands::Config(cmd) => exit_on_error(
            remora_config_application_transport_cli::run(cmd, &services.config).await,
            verbose,
        ),
        Commands::Convert(cmd) => exit_on_error(
            remora_convert_application_transport_cli::run(cmd, &services.convert).await,
            verbose,
        ),
        Commands::Batch(cmd) => exit_on_error(
            remora_batch_application_transport_cli::run(cmd, &services.batch).await,
            verbose,
        ),
        Commands::Factory(cmd) => exit_on_error(
            remora_factory_application_transport_cli::run(cmd, &services.factory).await,
            verbose,
        ),
        Commands::Update(args) => {
            let app =
                remora_update::model::App::running("remora-etcher", env!("CARGO_PKG_VERSION"));
            exit_on_error(
                remora_update_application_transport_cli::run(args, &services.update, &app).await,
                verbose,
            )
        }
        Commands::Completion(args) => remora_completion::instructions(
            "remora-etcher",
            args,
            "Commands, options, disks and paths complete with Tab.",
        ),
    }
}

fn exit_on_error<C>(result: Result<(), error_stack::Report<C>>, verbose: bool) -> i32 {
    match result {
        Ok(()) => 0,
        Err(report) => {
            tracing::debug!("{report:?}");
            remora_tui::render_report(&report, verbose);
            1
        }
    }
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

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
