use std::time::{SystemTime, UNIX_EPOCH};

use error_stack::ResultExt;
use remora_claim::{application::ClaimService, model::ClaimPlan};
use remora_progress::OperationContext;
use remora_station::model::{HardwareInfo, ImageInfo};
use remora_tui as tui;

use crate::{
    error::{Error, Result},
    service::SimulateArgs,
};

/// A provisional hostname, as a hub without a MAC would make up one.
fn random_hostname() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "sim{:09x}",
        (nanos ^ u128::from(std::process::id())) & 0xf_ffff_ffff
    )
}

pub(crate) async fn run(args: SimulateArgs, claims: &ClaimService) -> Result<()> {
    let plan = ClaimPlan {
        station: args.url,
        hardware: HardwareInfo {
            board: args.board,
            temp_hostname: args.temp_hostname.unwrap_or_else(random_hostname),
            eth_mac: args.eth_mac,
            bsp_serial: args.bsp_serial,
            machine_id: args.machine_id,
            macs: Default::default(),
        },
        image: ImageInfo {
            version: args.image_version,
            compatible: None,
        },
        output: args.output,
        poll_interval: args.poll_interval,
    };

    let (sink, events) = remora_progress::channel();
    let ctx = OperationContext::new(sink, remora_progress::cancelled_by_ctrl_c());
    let follow = tui::follow(events);
    let claimed = claims.claim(&plan, &ctx).await;
    drop(ctx);
    follow.finish(&claimed).await;
    let outcome = claimed.change_context(Error::Simulate)?;

    tui::success(format!(
        "Installed {}, credential in {} {}",
        tui::accent(&outcome.serial),
        tui::accent(plan.output.display()),
        tui::dim(format!(
            "({}, claim {})",
            outcome.environment, outcome.claim_id
        ))
    ));
    println!("{}", outcome.serial);
    Ok(())
}
