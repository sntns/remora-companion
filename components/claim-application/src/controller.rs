use std::time::Duration;

use error_stack::{Report, ResultExt};
use remora_claim::{
    adapter::station::{self, StationClientAdapterService},
    application::{ClaimServiceInterface, Error, Result},
    model::{ClaimOutcome, ClaimPlan},
};
use remora_factory::application::FactoryService;
use remora_progress::OperationContext;
use remora_station::model::{Ack, ClaimRequest, ClaimState, ClaimStatus, LabelState};

/// How long to wait before retrying a station that can't be reached at
/// all (an unavailable one says how long itself, else this).
const UNREACHABLE_RETRY: Duration = Duration::from_secs(2);
const UNAVAILABLE_RETRY: Duration = Duration::from_secs(5);

/// The claim vertical's use case: a device's key through the injected
/// `FactoryService` (which also writes its credential, exactly as
/// `factory provision` would), and the station through the injected
/// `StationClientAdapterService`. Holds the device's side of the protocol:
/// what to retry, what to give up on, when to write.
pub struct ClaimControllerImpl {
    station: StationClientAdapterService,
    factory: FactoryService,
}

impl ClaimControllerImpl {
    pub fn new(station: StationClientAdapterService, factory: FactoryService) -> Self {
        Self { station, factory }
    }
}

/// Sleeps `duration`, or less when `ctx` is cancelled; whether it was.
async fn cancellable_sleep(duration: Duration, ctx: &OperationContext) -> bool {
    tokio::select! {
        () = tokio::time::sleep(duration) => false,
        () = ctx.cancel.cancelled() => true,
    }
}

/// What the device's LED shows for `status`, as a phase.
fn led(status: &ClaimStatus) -> String {
    match status.label {
        LabelState::Queued => match status.queue_position {
            Some(position) => format!("LED: queued (position {position})"),
            None => "LED: queued".to_string(),
        },
        LabelState::Active => format!("LED: steady -- label me: {}", status.identity.serial_number),
        LabelState::Labelled => "LED: off -- labelled".to_string(),
    }
}

fn state_name(state: ClaimState) -> &'static str {
    match state {
        ClaimState::Issued => "issued",
        ClaimState::Installed => "installed",
        ClaimState::Failed => "failed",
    }
}

/// How long to wait before trying `report`'s call again, if it is worth it.
fn retry_in(report: &Report<station::Error>) -> Option<Duration> {
    match report.current_context() {
        station::Error::Unavailable { retry_after } => {
            Some(retry_after.unwrap_or(UNAVAILABLE_RETRY))
        }
        station::Error::Unreachable(_) => Some(UNREACHABLE_RETRY),
        _ => None,
    }
}

impl ClaimControllerImpl {
    /// Claims until the station answers: a claim is idempotent (the same
    /// key, the same claim), so retrying is always safe.
    async fn submit(
        &self,
        plan: &ClaimPlan,
        request: &ClaimRequest,
        ctx: &OperationContext,
    ) -> Result<ClaimStatus> {
        loop {
            let report = match self.station.claim(&plan.station, request).await {
                Ok(status) => return Ok(status),
                Err(report) => report,
            };
            let Some(wait) = retry_in(&report) else {
                return Err(report.change_context(Error::Refused));
            };
            ctx.sink.log(format!(
                "{}; retrying in {}s",
                report.current_context(),
                wait.as_secs()
            ));
            if cancellable_sleep(wait, ctx).await {
                return Err(Report::new(Error::Cancelled));
            }
        }
    }

    /// Polls until labelled, the LED following the queue.
    async fn follow(
        &self,
        plan: &ClaimPlan,
        mut status: ClaimStatus,
        ctx: &OperationContext,
    ) -> Result<ClaimStatus> {
        // The LED changes with the label, not with each step up the queue.
        let mut shown = status.label;
        ctx.sink.phase(led(&status));
        while status.label != LabelState::Labelled {
            if status.state != ClaimState::Issued {
                return Err(Report::new(Error::Abandoned(state_name(status.state))));
            }
            if cancellable_sleep(plan.poll_interval, ctx).await {
                return Err(Report::new(Error::Cancelled));
            }
            match self.station.status(&plan.station, &status.claim_id).await {
                Ok(next) => status = next,
                Err(report) if retry_in(&report).is_some() => {
                    ctx.sink.log(report.current_context().to_string())
                }
                Err(report) => return Err(report.change_context(Error::Lost)),
            }
            if status.label != shown {
                shown = status.label;
                ctx.sink.phase(led(&status));
            }
        }
        Ok(status)
    }
}

#[async_trait::async_trait]
impl ClaimServiceInterface for ClaimControllerImpl {
    async fn claim(&self, plan: &ClaimPlan, ctx: &OperationContext) -> Result<ClaimOutcome> {
        ctx.sink
            .phase(format!("reaching the station at {}", plan.station));
        let hello = self
            .station
            .hello(&plan.station)
            .await
            .change_context_lazy(|| Error::NotAStation(plan.station.clone()))?;

        // The key first -- on a hub, persisted before anything is sent:
        // the same key, the same claim, whatever is retried.
        ctx.sink.phase("generating the device key");
        let key = self
            .factory
            .generate_device_key()
            .await
            .change_context(Error::Keygen)?;
        let request = ClaimRequest {
            csr_der: key.csr_der,
            hardware: plan.hardware.clone(),
            image: plan.image.clone(),
        };
        ctx.sink.phase(format!("claiming ({})", hello.environment));
        let status = self.submit(plan, &request, ctx).await?;
        let status = self.follow(plan, status, ctx).await?;

        // Labelled: the commit point. Write the identity, then say so.
        let claim_id = status.claim_id;
        let serial = status.identity.serial_number.clone();
        ctx.sink.phase(format!("writing {}", plan.output.display()));
        self.factory
            .write_credential(key.private_key, status.identity, &plan.output)
            .await
            .change_context_lazy(|| Error::WriteOutput(plan.output.clone()))?;
        ctx.sink.phase("acknowledging");
        // At best: a lost ack changes nothing for the device.
        let acknowledged = match self
            .station
            .ack(&plan.station, &claim_id, &Ack::Installed)
            .await
        {
            Ok(_) => true,
            Err(report) => {
                ctx.sink
                    .log(format!("the station did not get the ack: {report}"));
                false
            }
        };
        ctx.sink.phase("done");
        Ok(ClaimOutcome {
            claim_id,
            serial,
            environment: hello.environment,
            acknowledged,
        })
    }
}
