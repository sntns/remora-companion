use remora_channel::application::ChannelService;
use remora_context::model::ContextOverride;
use remora_tui as tui;
use serde_json::json;

use crate::{
    error::{channel_error, Result},
    service::Format,
};

#[derive(clap::Args)]
#[command(after_help = "Example:\n  rmra local login-code 525400C0FFEE --account root K7QM-3XRB")]
pub struct LoginCodeArgs {
    /// The device's name (its serial).
    #[arg(add = remora_completion::values(remora_completion::Kind::Device))]
    device: String,
    /// The account being logged into on the device's console.
    #[arg(long)]
    account: String,
    /// The challenge the device's console login shows, as shown: case,
    /// dashes and spaces don't matter.
    challenge: String,
}

#[derive(clap::Args)]
#[command(
    after_help = "Each code is accepted once. Indices come from one series per device, shared \
by all its accounts and operators, and a device only accepts indices up to 1024 past the \
highest it has used: issue what you will need, not more."
)]
pub struct OfflineCodesArgs {
    /// The device's name (its serial).
    #[arg(add = remora_completion::values(remora_completion::Kind::Device))]
    device: String,
    /// The account the codes log into on the device's console.
    #[arg(long)]
    account: String,
    /// How many codes to issue, 1 to 100.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u32).range(1..=100))]
    count: u32,
    /// `table` for humans, `json` for scripts.
    #[arg(long, default_value = "table")]
    format: Format,
}

/// `rmra local login-code`: the code that answers a device's console
/// challenge. The code alone goes to stdout.
pub async fn run_login_code(
    args: LoginCodeArgs,
    service: &ChannelService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let code = service
        .login_code(over, &args.device, &args.account, &args.challenge)
        .await
        .map_err(channel_error)?;
    println!("{code}");
    Ok(())
}

/// `rmra local offline-codes`: codes a device accepts without a
/// challenge, for when it can't be answered on the spot.
pub async fn run_offline_codes(
    args: OfflineCodesArgs,
    service: &ChannelService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let codes = service
        .offline_login_codes(over, &args.device, &args.account, args.count)
        .await
        .map_err(channel_error)?;
    match args.format {
        Format::Json => println!(
            "{}",
            serde_json::to_string_pretty(&json!(codes
                .iter()
                .map(|code| json!({"index": code.index, "code": code.code}))
                .collect::<Vec<_>>()))
            .expect("json values serialize")
        ),
        Format::Table => {
            let mut table = tui::Table::new(["index", "code"]);
            for code in &codes {
                table.row([code.index.to_string(), code.code.clone()], false);
            }
            table.print();
        }
    }
    Ok(())
}
