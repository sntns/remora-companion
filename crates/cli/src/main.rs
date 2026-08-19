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
}

fn main() {
    let options = Options::parse();
    init_tracing(&options);

    let result = match options.command {
        Commands::Squashfs(cmd) => commands::squashfs::run(cmd),
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
