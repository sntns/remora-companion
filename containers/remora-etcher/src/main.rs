mod bootstrap;
mod completion;

use remora_context::model::ContextOverride;

use clap::{CommandFactory, FromArgMatches, Parser};

#[derive(Parser)]
#[command(
    name = "remora-etcher",
    version,
    about = "Standalone provisioning and flashing tool for Remora devices"
)]
struct Options {
    #[command(subcommand)]
    command: Commands,

    /// Before the command: the context it runs against (factory, batch).
    #[command(flatten)]
    context: remora_context_application_transport_cli::ContextArgs,

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
    /// Log in to the selected context's platform (the same contexts as rmra).
    Login(remora_context_application_transport_cli::LoginArgs),

    /// Forget the selected context's credentials.
    Logout(remora_context_application_transport_cli::LogoutArgs),

    /// Show whom the selected context is logged in as.
    Whoami(remora_context_application_transport_cli::WhoamiArgs),

    /// Manage contexts: named platform endpoints, docker-context style, and
    /// the IAM role each acts as. Shared with rmra.
    #[command(subcommand)]
    Context(remora_context_application_transport_cli::Command),

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

    /// Manufacture a device on sntns-platform as the selected context,
    /// writing its credential as a `remora-factory.yaml` to bundle into an
    /// image with `identity create`.
    #[command(subcommand)]
    Factory(remora_factory_application_transport_cli::Command),

    /// Update remora-etcher itself to its latest release.
    Update(remora_update_application_transport_cli::Args),

    /// Enable shell completion (contexts, roles, disks and paths with Tab).
    Completion(remora_completion::Args),
}

fn main() {
    // Shell completion: when the shell calls back with COMPLETE set, answer
    // and exit before anything else. Outside the async runtime on purpose:
    // the provider runs its own, and a runtime can't nest in another.
    remora_completion::install(completion::provide);
    clap_complete::CompleteEnv::with_factory(Options::command).complete();

    let matches = Options::command().get_matches();
    let options = Options::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let over = options.context.over(&matches);
    init_tracing(&options);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the tokio runtime builds");
    let code = runtime.block_on(run(options, over));
    std::process::exit(code);
}

async fn run(options: Options, over: Option<ContextOverride>) -> i32 {
    let verbose = options.verbose > 0;
    let services = bootstrap::wire().await;
    let over = over.as_ref();

    use remora_context_application_transport_cli as context;
    match options.command {
        Commands::Login(args) => exit_on_error(
            context::run_login(args, &services.context, over).await,
            verbose,
        ),
        Commands::Logout(args) => exit_on_error(
            context::run_logout(args, &services.context, over).await,
            verbose,
        ),
        Commands::Whoami(args) => exit_on_error(
            context::run_whoami(args, &services.context, over).await,
            verbose,
        ),
        Commands::Context(command) => exit_on_error(
            context::run(command, &services.context, over).await,
            verbose,
        ),
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
            remora_batch_application_transport_cli::run(cmd, &services.batch, over).await,
            verbose,
        ),
        Commands::Factory(cmd) => exit_on_error(
            remora_factory_application_transport_cli::run(cmd, &services.factory, over).await,
            verbose,
        ),
        Commands::Update(args) => exit_on_error(
            remora_update_application_transport_cli::run(
                args,
                &services.update,
                "remora-etcher",
                env!("CARGO_PKG_VERSION"),
            )
            .await,
            verbose,
        ),
        Commands::Completion(args) => remora_completion::instructions(
            "remora-etcher",
            args,
            "Contexts, roles, disks and paths complete with Tab.",
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
