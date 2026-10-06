use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD, Engine};
use error_stack::{Report, ResultExt};
use remora_factory::{
    adapter::{credential::CredentialWriterAdapterService, key::DeviceKeyAdapterService},
    model::FactoryCredential,
};
use remora_station_application_transport_http::{
    self as http, AckBody, AckState, ClaimBody, ClaimStatusBody, HardwareBody, ImageBody,
    LabelBody, StateBody, StationClient,
};
use remora_tui as tui;
use tokio_util::sync::CancellationToken;

use crate::{
    error::{Error, Result},
    service::SimulateArgs,
};

/// How long to wait before retrying a station that can't be reached at
/// all (a 503 says how long itself).
const UNREACHABLE_RETRY: Duration = Duration::from_secs(2);

/// What the simulated LED shows, as the hub's would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Led {
    Queued,
    Steady,
    Off,
}

fn led(status: &ClaimStatusBody) -> Led {
    match status.label {
        LabelBody::Queued => Led::Queued,
        LabelBody::Active => Led::Steady,
        LabelBody::Labelled => Led::Off,
    }
}

fn show(status: &ClaimStatusBody) {
    match led(status) {
        Led::Queued => tui::step(format!(
            "LED: queued {}",
            tui::dim(format!(
                "(position {})",
                status
                    .queue_position
                    .map_or("?".to_string(), |at| at.to_string())
            ))
        )),
        Led::Steady => tui::info(format!(
            "LED: {} -- label me: {}",
            tui::accent("steady"),
            tui::accent(&status.identity.serial_number)
        )),
        Led::Off => tui::step("LED: off -- labelled"),
    }
}

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

/// Sleeps `duration`, or less when `cancel` fires; whether it did.
async fn cancellable_sleep(duration: Duration, cancel: &CancellationToken) -> bool {
    tokio::select! {
        () = tokio::time::sleep(duration) => false,
        () = cancel.cancelled() => true,
    }
}

pub(crate) async fn run(
    args: SimulateArgs,
    keys: &DeviceKeyAdapterService,
    credentials: &CredentialWriterAdapterService,
) -> Result<()> {
    let cancel = remora_progress::cancelled_by_ctrl_c();
    let client = StationClient::new(&args.url);
    let hello = client
        .hello()
        .await
        .change_context_lazy(|| Error::NotAStation(args.url.clone()))?;
    if hello.service != http::SERVICE || hello.protocol != http::PROTOCOL {
        return Err(
            Report::new(Error::NotAStation(args.url.clone())).attach(format!(
                "it answered {} protocol {}",
                hello.service, hello.protocol
            )),
        );
    }
    tui::step(format!(
        "Station at {} {}",
        tui::accent(client.base_url()),
        tui::dim(format!("({})", hello.environment))
    ));

    // The key first, persisted before anything is sent on a real hub: the
    // same key, the same claim, whatever is retried.
    let key = keys.generate(None).await.change_context(Error::Keygen)?;
    let claim = ClaimBody {
        csr: STANDARD.encode(&key.csr_der),
        hardware: HardwareBody {
            board: args.board.clone(),
            temp_hostname: args.temp_hostname.clone().unwrap_or_else(random_hostname),
            eth_mac: args.eth_mac.clone(),
            bsp_serial: args.bsp_serial.clone(),
            machine_id: args.machine_id.clone(),
            macs: Default::default(),
        },
        image: ImageBody {
            version: args.image_version.clone(),
            compatible: None,
        },
    };
    let mut status = loop {
        match client.claim(&claim).await {
            Ok(status) => break status,
            Err(report) => {
                let wait = match report.current_context() {
                    http::Error::Status {
                        status: 503,
                        retry_after,
                        ..
                    } => Duration::from_secs(retry_after.unwrap_or(5)),
                    http::Error::Unreachable(_) => UNREACHABLE_RETRY,
                    _ => {
                        let message = report.current_context().to_string();
                        return Err(report.change_context(Error::Claim(message)));
                    }
                };
                tui::warning(format!(
                    "{}; retrying in {}s",
                    report.current_context(),
                    wait.as_secs()
                ));
                if cancellable_sleep(wait, &cancel).await {
                    return Err(Report::new(Error::Cancelled));
                }
            }
        }
    };
    tui::success(format!(
        "Claimed {} {}",
        tui::accent(&status.identity.serial_number),
        tui::dim(format!("(claim {})", status.claim_id))
    ));

    // Poll until labelled, like the hub: the LED follows the queue.
    show(&status);
    let mut shown = led(&status);
    while status.label != LabelBody::Labelled {
        if status.state != StateBody::Issued {
            return Err(Report::new(Error::Claim(format!(
                "the station says this claim is {:?}",
                status.state
            ))));
        }
        if cancellable_sleep(args.poll_interval, &cancel).await {
            return Err(Report::new(Error::Cancelled));
        }
        match client.status(&status.claim_id).await {
            Ok(next) => status = next,
            Err(report) => tui::warning(report.current_context().to_string()),
        }
        if led(&status) != shown {
            show(&status);
            shown = led(&status);
        }
    }

    // Labelled: the commit point. Write the identity, then say so.
    let identity = status
        .identity
        .clone()
        .into_identity()
        .map_err(|reason| Report::new(Error::Claim(reason)))?;
    let serial = identity.serial_number.clone();
    let credential = FactoryCredential {
        private_key: key.private_key,
        certificate_der: identity.certificate_der,
        certificate_authority_der: identity.certificate_authority_der,
        server_certificate_authority_der: identity.server_certificate_authority_der,
        key_id: identity.key_id,
    };
    credentials
        .write(&credential, &identity.access_url, &args.output)
        .await
        .change_context_lazy(|| Error::WriteOutput(args.output.clone()))?;
    let ack = AckBody {
        state: AckState::Installed,
        reason: None,
    };
    // At best: a lost ack changes nothing for the hub.
    if let Err(report) = client.ack(&status.claim_id, &ack).await {
        tui::warning(format!("the station did not get the ack: {report}"));
    }
    tui::success(format!(
        "Installed {}, credential in {}",
        tui::accent(&serial),
        tui::accent(args.output.display())
    ));
    println!("{serial}");
    Ok(())
}
