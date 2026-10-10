mod bootstrap;
mod completion;

use clap::{CommandFactory, FromArgMatches, Parser};

/// This binary's name, for the commands its messages suggest.
const PROGRAM: &str = "rmra";

#[derive(Parser)]
#[command(
    name = PROGRAM,
    version,
    about = "Remora operator CLI: reach your devices through sntns-platform",
    after_help = "Start with `rmra login`, then `rmra ssh <device>`, or publish an update with \
                  `rmra release create` and roll it out with `rmra deploy`."
)]
struct Options {
    #[command(subcommand)]
    command: Commands,

    /// Before the command: the context it runs against.
    #[command(flatten)]
    context: remora_context_application_transport_cli::ContextArgs,

    /// Show errors in full, with where each cause was raised, and debug
    /// detail (e.g. how `rmra ssh` set the session up).
    #[arg(short, long, global = true)]
    verbose: bool,
}

/// `rmra local`: one group for the operator on site, from two verticals.
#[derive(clap::Subcommand)]
enum LocalCommands {
    #[command(flatten)]
    Codes(remora_channel_application_transport_cli::LocalCommand),

    /// A device's serial console, minicom-style, its login challenge
    /// answered with a code the platform signs.
    Console(remora_console_application_transport_cli::ConsoleArgs),
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Log in to the selected context's platform.
    Login(remora_context_application_transport_cli::LoginArgs),

    /// Forget the selected context's credentials.
    Logout(remora_context_application_transport_cli::LogoutArgs),

    /// Show whom the selected context is logged in as.
    Whoami(remora_context_application_transport_cli::WhoamiArgs),

    /// Manage contexts: named platform endpoints, docker-context style, and
    /// the IAM role each acts as.
    #[command(subcommand)]
    Context(remora_context_application_transport_cli::Command),

    /// Open channels to devices.
    #[command(subcommand)]
    Channel(remora_channel_application_transport_cli::Command),

    /// List the account's devices.
    #[command(subcommand)]
    Device(remora_device_application_transport_cli::Command),

    /// Log into a device over ssh, through its remora channel.
    Ssh(remora_channel_application_transport_cli::SshArgs),

    /// Copy files to or from a device over scp, through its remora channel.
    Scp(remora_channel_application_transport_cli::ScpArgs),

    /// On site, at a device's local network or serial console: ssh
    /// certificates, console login codes, the console itself.
    #[command(subcommand)]
    Local(LocalCommands),

    /// Manage over-the-air releases: versions and their artifacts.
    #[command(subcommand)]
    Release(remora_ota_application_transport_cli::ReleaseCommand),

    /// Manage over-the-air deployments: a release being installed on devices.
    #[command(subcommand)]
    Deployment(remora_ota_application_transport_cli::DeploymentCommand),

    /// Deploy a release to devices (same as `rmra deployment create`).
    Deploy(remora_ota_application_transport_cli::DeployArgs),

    /// Install an OS image (a RAUC bundle, local or a release's) on a
    /// device over its channel: upload (resumable), verify, install,
    /// reboot into it and validate it.
    Install(remora_install_application_transport_cli::Args),

    /// Update rmra itself to its latest release.
    Update(remora_update_application_transport_cli::Args),

    /// Enable shell completion (contexts, devices, releases... with Tab).
    Completion(remora_completion::Args),
}

fn main() {
    // Shell completion: when the shell calls back with COMPLETE set, answer
    // and exit before anything else. Outside the async runtime on purpose:
    // providers run their own, and a runtime can't nest in another.
    remora_completion::install(completion::provide);
    clap_complete::CompleteEnv::with_factory(Options::command).complete();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the tokio runtime builds");
    runtime.block_on(run());
}

async fn run() {
    let matches = Options::command().get_matches();
    let options = Options::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    let verbose = options.verbose;
    // A usage error, like clap's own: the environment defines a context
    // partly, or both defines and names one.
    let over = options
        .context
        .over(&matches)
        .unwrap_or_else(|report| std::process::exit(fail(report, verbose, 2)));
    let over = over.as_ref();

    let services = bootstrap::wire().await;

    use remora_channel_application_transport_cli as channel;
    use remora_context_application_transport_cli as context;
    use remora_ota_application_transport_cli as ota;
    let code = match options.command {
        Commands::Login(args) => exit_on_error(
            context::run_login(args, &services.context, over, PROGRAM).await,
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
            context::run(command, &services.context, over, PROGRAM).await,
            verbose,
        ),
        // ssh's own convention: 255 when the connection itself failed.
        Commands::Channel(command) => {
            channel::run(command, &services.channel, over, PROGRAM, verbose)
                .await
                .unwrap_or_else(|report| fail(report, verbose, 255))
        }
        Commands::Ssh(args) => channel::run_ssh(args, &services.channel, over, verbose)
            .await
            .unwrap_or_else(|report| fail(report, verbose, 255)),
        // scp's own convention: 1 for any failure.
        Commands::Scp(args) => channel::run_scp(args, &services.channel, over, verbose)
            .await
            .unwrap_or_else(|report| fail(report, verbose, 1)),
        Commands::Local(LocalCommands::Codes(command)) => exit_on_error(
            channel::run_local(command, &services.channel, over).await,
            verbose,
        ),
        Commands::Local(LocalCommands::Console(args)) => exit_on_error(
            remora_console_application_transport_cli::run(args, &services.console, over).await,
            verbose,
        ),
        Commands::Completion(args) => remora_completion::instructions(
            PROGRAM,
            args,
            "Contexts, devices, releases and deployments complete with Tab.",
        ),
        Commands::Update(args) => exit_on_error(
            remora_update_application_transport_cli::run(
                args,
                &services.update,
                PROGRAM,
                env!("CARGO_PKG_VERSION"),
            )
            .await,
            verbose,
        ),
        Commands::Device(command) => exit_on_error(
            remora_device_application_transport_cli::run(command, &services.device, over).await,
            verbose,
        ),
        Commands::Release(command) => exit_on_error(
            ota::run_release(command, &services.ota, &services.context, over).await,
            verbose,
        ),
        Commands::Deployment(command) => exit_on_error(
            ota::run_deployment(command, &services.ota, over).await,
            verbose,
        ),
        Commands::Deploy(args) => {
            exit_on_error(ota::run_deploy(args, &services.ota, over).await, verbose)
        }
        Commands::Install(args) => exit_on_error(
            remora_install_application_transport_cli::run(
                args,
                &services.install,
                &services.ota,
                &services.context,
                over,
            )
            .await,
            verbose,
        ),
    };
    // Exit now rather than return: `channel open` may still have a thread
    // blocked reading stdin, which would otherwise keep the runtime alive.
    std::process::exit(code);
}

fn exit_on_error<C>(result: Result<(), error_stack::Report<C>>, verbose: bool) -> i32 {
    match result {
        Ok(()) => 0,
        Err(report) => fail(report, verbose, 1),
    }
}

/// Renders a failure -- with what to run about it, when it is a context's
/// (not logged in, none selected...), whichever command met it.
fn fail<C>(report: error_stack::Report<C>, verbose: bool, code: i32) -> i32 {
    let report = remora_context_application_transport_cli::with_hint(report, PROGRAM);
    remora_tui::render_report(&report, verbose);
    code
}
