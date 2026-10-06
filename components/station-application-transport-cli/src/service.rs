use std::{net::SocketAddr, path::PathBuf, time::Duration};

use remora_claim::application::ClaimService;
use remora_context::model::ContextOverride;
use remora_station::application::StationService;

use crate::{config::Confirm, error::Result, serve, simulate};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Run a provisioning station on the workshop network: every hub of a
    /// configured board that boots a cloned image and claims an identity
    /// gets one from the platform, as the selected context, then waits for
    /// its label -- one hub at a time, the one whose LED is steady; scan
    /// the label stuck on it to validate. Each issued serial is printed on
    /// stdout; the dashboard is on stderr.
    Serve(ServeArgs),

    /// Play a hub claiming its identity from a station: generate a key,
    /// claim, follow the queue (the LED, simulated on stderr), and once
    /// labelled, write the `remora-factory.yaml` the hub would, then
    /// acknowledge it. For testing a station, and hub-virtual.
    Simulate(SimulateArgs),
}

#[derive(clap::Args)]
pub struct ServeArgs {
    /// The station's settings (listen, journal, hooks, max-claims, confirm,
    /// presence-timeout, hook-timeout, boards); each option below
    /// overrides its key. Relative paths in it are from its directory.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// Where hubs reach the station [default: 0.0.0.0:8484].
    #[arg(long)]
    pub(crate) listen: Option<SocketAddr>,
    /// The journal: every transition, appended; reloaded at start
    /// [default: station.jsonl].
    #[arg(long)]
    pub(crate) journal: Option<PathBuf>,
    /// Holds the hooks' `<event>.d/` directories; must exist, even empty
    /// [default: station.d].
    #[arg(long)]
    pub(crate) hooks: Option<PathBuf>,
    /// How many identities this run may issue at most.
    #[arg(long)]
    pub(crate) max_claims: Option<u32>,
    /// How a label is validated [default: scan].
    #[arg(long, value_enum)]
    pub(crate) confirm: Option<Confirm>,
    /// Requeue the active hub after this long without a poll [default: 10s].
    #[arg(long, value_parser = humantime::parse_duration)]
    pub(crate) presence_timeout: Option<Duration>,
    /// How long each hook script may run [default: 60s].
    #[arg(long, value_parser = humantime::parse_duration)]
    pub(crate) hook_timeout: Option<Duration>,
    /// Accept BOARD, its serials allocated from POLICY (repeatable).
    #[arg(long = "serial-policy", value_name = "BOARD=POLICY")]
    pub(crate) serial_policies: Vec<String>,
    /// Accept BOARD, its serial named by TEMPLATE over the hub's hardware:
    /// {bsp_serial}, {eth_mac}, {temp_hostname}, {board} (repeatable).
    #[arg(long = "device-name", value_name = "BOARD=TEMPLATE")]
    pub(crate) device_names: Vec<String>,
}

#[derive(clap::Args)]
pub struct SimulateArgs {
    /// The station.
    #[arg(long, default_value = "http://127.0.0.1:8484")]
    pub(crate) url: String,
    /// The hub's board.
    #[arg(long)]
    pub(crate) board: String,
    /// Where to write the hub's `remora-factory.yaml`.
    #[arg(long, default_value = "remora-factory.yaml")]
    pub(crate) output: PathBuf,
    /// The hub's provisional hostname [default: a random one].
    #[arg(long)]
    pub(crate) temp_hostname: Option<String>,
    /// The hub's Ethernet MAC, as reported.
    #[arg(long)]
    pub(crate) eth_mac: Option<String>,
    /// The hub's BSP serial, as reported.
    #[arg(long)]
    pub(crate) bsp_serial: Option<String>,
    /// The hub's machine-id, as reported.
    #[arg(long)]
    pub(crate) machine_id: Option<String>,
    /// The image version reported.
    #[arg(long)]
    pub(crate) image_version: Option<String>,
    /// How often the hub polls, like a real one [default: 1s].
    #[arg(long, hide = true, value_parser = humantime::parse_duration, default_value = "1s")]
    pub(crate) poll_interval: Duration,
}

/// `serve` issues as the selected context: its login (or the role it acts
/// as) needs `remora::create-factory-device`, and
/// `remora::use-serial-number-policy` on each policy. `simulate` is a hub,
/// through the claim vertical: no context involved.
pub async fn run(
    command: Command,
    station: &StationService,
    claims: &ClaimService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    match command {
        Command::Serve(args) => serve::run(args, station, over).await,
        Command::Simulate(args) => simulate::run(args, claims).await,
    }
}
