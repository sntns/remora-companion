mod commands;
mod output;

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

    /// Override the tracing filter directly (e.g. "remora_etcher_core=debug").
    #[arg(long, global = true)]
    trace: Option<String>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// Build and inspect squashfs images.
    #[command(subcommand)]
    Squashfs(commands::squashfs::Command),

    /// List, inspect, and flash disks.
    #[command(subcommand)]
    Disk(commands::disk::Command),

    /// Inspect and list partitions of an image or device.
    #[command(subcommand)]
    Image(commands::image::Command),

    /// Build identity.squashfs and/or inject it into an image's shared
    /// partition.
    #[command(subcommand)]
    Identity(commands::identity::Command),

    /// Build config.ext4 and/or upload files into an image's shared
    /// partition.
    #[command(subcommand)]
    Config(commands::config::Command),
}

fn main() {
    let options = Options::parse();
    init_tracing(&options);

    let result: Result<(), Box<dyn std::error::Error>> = match options.command {
        Commands::Squashfs(cmd) => commands::squashfs::run(cmd).map_err(Into::into),
        Commands::Disk(cmd) => commands::disk::run(cmd),
        Commands::Image(cmd) => commands::image::run(cmd),
        Commands::Identity(cmd) => commands::identity::run(cmd).map_err(Into::into),
        Commands::Config(cmd) => commands::config::run(cmd),
    };

    if let Err(err) = result {
        tracing::error!("{err}");
        eprintln!("error: {err}");
        std::process::exit(1);
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
        EnvFilter::new(format!("remora_etcher={level},remora_etcher_core={level}"))
    };

    tracing_subscriber::fmt().with_env_filter(filter).init();
}
