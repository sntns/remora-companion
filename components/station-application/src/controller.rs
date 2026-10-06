use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex as SyncMutex, OnceLock,
    },
    time::{Duration, Instant},
};

use error_stack::{Report, ResultExt};
use remora_context::{application::ContextService, model::ContextOverride};
use remora_factory::{
    adapter::provisioning::{self, ProvisionedIdentity},
    application::{self as factory, FactoryService},
    model::DeviceSerial,
};
use remora_station::{
    adapter::{
        hooks::{ClaimRecord, HookEvent, HookInvocation, HookRun, HookRunnerAdapterService},
        journal::{JournalAdapterService, JournalEntry, JournalEvent},
        operator::{
            Counters, HubSummary, LabelBlock, OperatorAdapterService, OperatorEvent, OperatorInput,
        },
    },
    application::{Error, Result, StationServiceInterface},
    model::{
        Ack, BoardPolicy, ClaimId, ClaimRequest, ClaimState, ClaimStatus, ConfirmMode,
        HardwareInfo, Hello, ImageInfo, LabelState, StationConfig, StationSummary,
    },
};
use tokio::sync::Mutex;

/// The station vertical's use case. Issues through the injected
/// `FactoryService` as the context `start` resolved (the CSR checked, its
/// key fingerprint naming the claim), keeps every claim in memory behind
/// one lock, journals each transition through the injected
/// `JournalAdapterService` (and rebuilds from it at `start`), runs hooks
/// through the injected `HookRunnerAdapterService` off that lock, and talks
/// to the operator through the injected `OperatorAdapterService`.
///
/// The platform call is the one slow step and is made outside the state
/// lock, so claims issue in parallel; a lock per claim id makes concurrent
/// requests for one key wait for the first one's answer instead of
/// asking twice.
pub struct StationControllerImpl(Arc<Shared>);

impl StationControllerImpl {
    pub fn new(
        contexts: ContextService,
        factory: FactoryService,
        journal: JournalAdapterService,
        hooks: HookRunnerAdapterService,
        operator: OperatorAdapterService,
    ) -> Self {
        Self(Arc::new(Shared {
            contexts,
            factory,
            journal,
            hooks,
            operator,
            running: OnceLock::new(),
            state: Mutex::new(State::default()),
            claim_locks: SyncMutex::new(HashMap::new()),
            searching: AtomicUsize::new(0),
        }))
    }
}

/// Hooks run on spawned tasks, which outlive any one call: everything they
/// touch lives here, behind an `Arc`.
struct Shared {
    contexts: ContextService,
    factory: FactoryService,
    journal: JournalAdapterService,
    hooks: HookRunnerAdapterService,
    operator: OperatorAdapterService,
    /// Set once, by `start`.
    running: OnceLock<Running>,
    state: Mutex<State>,
    claim_locks: SyncMutex<HashMap<ClaimId, Arc<Mutex<()>>>>,
    /// Claims waiting on the platform.
    searching: AtomicUsize,
}

struct Running {
    config: StationConfig,
    over: Option<ContextOverride>,
    /// The resolved context's name.
    context: String,
}

#[derive(Default)]
struct State {
    claims: HashMap<ClaimId, Claim>,
    /// Issued claims waiting for their label, oldest first; the active one
    /// is not in it.
    queue: VecDeque<ClaimId>,
    active: Option<ClaimId>,
    /// Identities issued by this run, and platform calls under way: the
    /// quota counts both, so that concurrent claims can't overshoot it.
    issued_this_run: u32,
    reserved: u32,
    /// Names each `label.d` run, so that a late result of a superseded run
    /// (reprinted, skipped meanwhile) is not taken for the current one.
    print_runs: u64,
    /// Claims the platform refused as already existing.
    refused: usize,
}

struct Claim {
    id: ClaimId,
    hardware: HardwareInfo,
    image: ImageInfo,
    policy: BoardPolicy,
    identity: ProvisionedIdentity,
    state: ClaimState,
    label: LabelState,
    /// How many times it was made active or reprinted: `REMORA_ATTEMPT`.
    attempt: u32,
    /// Its last poll; none for a claim reloaded from the journal that
    /// hasn't polled since.
    last_seen: Option<Instant>,
    print: Print,
    /// The operator allowed validating despite a failed `label.d`.
    forced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Print {
    Idle,
    Running(u64),
    Done,
    Failed,
}

impl Claim {
    fn new(
        id: ClaimId,
        hardware: HardwareInfo,
        image: ImageInfo,
        policy: BoardPolicy,
        identity: ProvisionedIdentity,
    ) -> Self {
        Self {
            id,
            hardware,
            image,
            policy,
            identity,
            state: ClaimState::Issued,
            label: LabelState::Queued,
            attempt: 0,
            last_seen: None,
            print: Print::Idle,
            forced: false,
        }
    }

    fn waiting_for_label(&self) -> bool {
        self.state == ClaimState::Issued && self.label != LabelState::Labelled
    }

    fn present(&self, timeout: Duration) -> bool {
        self.last_seen.is_some_and(|seen| seen.elapsed() < timeout)
    }

    fn hub(&self) -> HubSummary {
        HubSummary {
            claim_id: self.id.clone(),
            serial: Some(self.identity.serial_number.clone()),
            board: self.hardware.board.clone(),
            temp_hostname: self.hardware.temp_hostname.clone(),
            eth_mac: self.hardware.eth_mac.clone(),
        }
    }

    fn record(&self, reason: Option<String>) -> ClaimRecord {
        ClaimRecord {
            claim_id: self.id.clone(),
            hardware: self.hardware.clone(),
            image: self.image.clone(),
            policy: self.policy.clone(),
            identity: Some(self.identity.clone()),
            state: Some(self.state),
            label: Some(self.label),
            reason,
        }
    }
}

fn hub_of(record: &ClaimRecord) -> HubSummary {
    HubSummary {
        claim_id: record.claim_id.clone(),
        serial: record
            .identity
            .as_ref()
            .map(|identity| identity.serial_number.clone()),
        board: record.hardware.board.clone(),
        temp_hostname: record.hardware.temp_hostname.clone(),
        eth_mac: record.hardware.eth_mac.clone(),
    }
}

/// Counts a claim as searching for as long as it waits on the platform.
struct Searching<'a>(&'a AtomicUsize);

impl<'a> Searching<'a> {
    fn new(counter: &'a AtomicUsize) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self(counter)
    }
}

impl Drop for Searching<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What a factory failure means for the device: its CSR (it makes a new
/// key), the station's configuration or the platform's refusal (it waits
/// for someone to fix it), an existing device name, or an unreachable
/// platform (it retries). The provisioning port's error says which, a few
/// hops down the chain.
fn platform_error(report: Report<factory::Error>) -> Report<Error> {
    let next = match report.current_context() {
        factory::Error::InvalidCsr => Error::InvalidCsr,
        factory::Error::MissingAccessUrl => Error::Refused,
        _ => match report.downcast_ref::<provisioning::Error>() {
            Some(provisioning::Error::AlreadyExists(name)) => Error::AlreadyExists(name.clone()),
            Some(provisioning::Error::Refused) => Error::Refused,
            _ => Error::Unavailable,
        },
    };
    report.change_context(next)
}

impl Shared {
    fn running(&self) -> Result<&Running> {
        self.running
            .get()
            .ok_or_else(|| Report::new(Error::NotStarted))
    }

    fn claim_lock(&self, id: &ClaimId) -> Arc<Mutex<()>> {
        self.claim_locks
            .lock()
            .expect("the claim lock table is never poisoned")
            .entry(id.clone())
            .or_default()
            .clone()
    }

    fn counters(&self, state: &State) -> Counters {
        let count = |keep: fn(&Claim) -> bool| state.claims.values().filter(|c| keep(c)).count();
        Counters {
            searching: self.searching.load(Ordering::Relaxed),
            issued: state.claims.len(),
            queued: state.queue.len() + usize::from(state.active.is_some()),
            labelled: count(|c| c.state == ClaimState::Issued && c.label == LabelState::Labelled),
            installed: count(|c| c.state == ClaimState::Installed),
            failed: count(|c| c.state == ClaimState::Failed) + state.refused,
        }
    }

    fn status(&self, state: &State, id: &ClaimId) -> Option<ClaimStatus> {
        let claim = state.claims.get(id)?;
        let queue_position = if state.active.as_ref() == Some(id) {
            Some(0)
        } else if claim.waiting_for_label() {
            state
                .queue
                .iter()
                .position(|queued| queued == id)
                .map(|at| at + 1)
        } else {
            None
        };
        Some(ClaimStatus {
            claim_id: id.clone(),
            state: claim.state,
            label: claim.label,
            queue_position,
            identity: claim.identity.clone(),
        })
    }

    /// Journals `event`. A journal that can't be written doesn't stop the
    /// flow once an identity exists (refusing the device helps no one);
    /// the operator is told every time.
    async fn record(&self, running: &Running, claim_id: &ClaimId, event: JournalEvent) {
        let entry = JournalEntry {
            claim_id: claim_id.clone(),
            event,
        };
        if let Err(report) = self.journal.append(&running.config.journal, &entry).await {
            self.operator.show(&OperatorEvent::Warning(format!(
                "{report} -- the production register is missing an entry for claim {claim_id}"
            )));
        }
    }

    fn hook_run(
        &self,
        running: &Running,
        event: HookEvent,
        claim: ClaimRecord,
        attempt: u32,
    ) -> HookRun {
        HookRun {
            dir: running.config.hooks.clone(),
            timeout: running.config.hook_timeout,
            invocation: HookInvocation {
                event,
                claim,
                context: running.context.clone(),
                attempt: attempt.max(1),
                journal: running.config.journal.clone(),
            },
        }
    }

    /// Runs `run` on its own task: hooks take their time (a printer, a
    /// network registry), and nothing waits on them -- except validating a
    /// label, which reads the outcome of `print`'s run from the claim.
    fn spawn_hooks(self: &Arc<Self>, run: HookRun, print: Option<u64>) {
        let shared = Arc::clone(self);
        tokio::spawn(async move {
            let outcomes = shared.hooks.run(&run).await;
            let Ok(running) = shared.running() else {
                return;
            };
            let invocation = &run.invocation;
            let hub = hub_of(&invocation.claim);
            let mut state = shared.state.lock().await;
            let succeeded = match outcomes {
                Ok(outcomes) => {
                    for outcome in &outcomes {
                        shared
                            .record(
                                running,
                                &invocation.claim.claim_id,
                                JournalEvent::Hook {
                                    hook: outcome.hook.clone(),
                                    exit: outcome.exit,
                                    timed_out: outcome.timed_out,
                                    attempt: invocation.attempt,
                                    stdout: outcome.stdout.clone(),
                                    stderr: outcome.stderr.clone(),
                                },
                            )
                            .await;
                        shared.operator.show(&OperatorEvent::Hook {
                            event: invocation.event,
                            hub: hub.clone(),
                            outcome: outcome.clone(),
                        });
                    }
                    outcomes.iter().all(|outcome| outcome.succeeded())
                }
                Err(report) => {
                    shared.operator.show(&OperatorEvent::Warning(format!(
                        "{} hooks did not run: {report}",
                        invocation.event.name()
                    )));
                    false
                }
            };
            let Some(print) = print else {
                return;
            };
            let id = &invocation.claim.claim_id;
            let active = state.active.as_ref() == Some(id);
            let Some(claim) = state.claims.get_mut(id) else {
                return;
            };
            if claim.print != Print::Running(print) {
                return;
            }
            claim.print = if succeeded {
                Print::Done
            } else {
                Print::Failed
            };
            if !succeeded && active {
                shared.operator.show(&OperatorEvent::Blocked {
                    hub: claim.hub(),
                    reason: LabelBlock::PrintFailed,
                });
            }
        });
    }

    /// Runs `label.d` for claim `id` (again).
    fn print(self: &Arc<Self>, running: &Running, state: &mut State, id: &ClaimId) {
        state.print_runs += 1;
        let run_id = state.print_runs;
        let claim = state.claims.get_mut(id).expect("printing a known claim");
        claim.print = Print::Running(run_id);
        let run = self.hook_run(running, HookEvent::Label, claim.record(None), claim.attempt);
        self.spawn_hooks(run, Some(run_id));
    }

    /// Makes the first queued claim whose device is still polling the
    /// active one, unless one already is. Claims of devices gone silent
    /// keep their place, and are activated once they poll again.
    async fn promote(self: &Arc<Self>, running: &Running, state: &mut State) -> bool {
        if state.active.is_some() {
            return true;
        }
        let timeout = running.config.presence_timeout;
        let Some(at) = state
            .queue
            .iter()
            .position(|id| state.claims[id].present(timeout))
        else {
            return false;
        };
        let id = state
            .queue
            .remove(at)
            .expect("a position found in the queue");
        let claim = state.claims.get_mut(&id).expect("queued claims are known");
        claim.label = LabelState::Active;
        claim.attempt += 1;
        claim.forced = false;
        let attempt = claim.attempt;
        let hub = claim.hub();
        state.active = Some(id.clone());
        self.record(running, &id, JournalEvent::LabelActive { attempt })
            .await;
        self.operator.show(&OperatorEvent::Active {
            hub,
            attempt,
            counters: self.counters(state),
        });
        self.print(running, state, &id);
        true
    }

    /// After the active claim left: the next one, or say there's none.
    async fn advance(self: &Arc<Self>, running: &Running, state: &mut State) {
        if !self.promote(running, state).await {
            self.operator.show(&OperatorEvent::Idle {
                counters: self.counters(state),
            });
        }
    }

    /// The status of a claim already issued, counting this as a poll.
    async fn known(self: &Arc<Self>, running: &Running, id: &ClaimId) -> Option<ClaimStatus> {
        let mut state = self.state.lock().await;
        state.claims.get_mut(id)?.last_seen = Some(Instant::now());
        self.promote(running, &mut state).await;
        self.status(&state, id)
    }

    fn reject(&self, hardware: &HardwareInfo, id: &ClaimId, report: &Report<Error>) {
        self.operator.show(&OperatorEvent::Rejected {
            hub: HubSummary {
                claim_id: id.clone(),
                serial: None,
                board: hardware.board.clone(),
                temp_hostname: hardware.temp_hostname.clone(),
                eth_mac: hardware.eth_mac.clone(),
            },
            reason: report.current_context().to_string(),
        });
    }

    async fn submit(self: &Arc<Self>, request: ClaimRequest) -> Result<ClaimStatus> {
        let running = self.running()?;
        let verified = self
            .factory
            .verify_csr(&request.csr_der)
            .await
            .map_err(platform_error)?;
        let id = ClaimId::new(verified.public_key_fingerprint);
        if let Some(status) = self.known(running, &id).await {
            return Ok(status);
        }

        let board = &request.hardware.board;
        let policy = running.config.boards.get(board).cloned().ok_or_else(|| {
            let report = Report::new(Error::UnknownBoard(board.clone()));
            self.reject(&request.hardware, &id, &report);
            report
        })?;
        let serial = policy.serial_for(&request.hardware).map_err(|error| {
            let report = Report::new(Error::InvalidHardware(error.to_string()));
            self.reject(&request.hardware, &id, &report);
            report
        })?;

        let lock = self.claim_lock(&id);
        let _claim = lock.lock().await;
        // A concurrent request for this key may have been issued while
        // this one waited.
        if let Some(status) = self.known(running, &id).await {
            return Ok(status);
        }
        let _searching = Searching::new(&self.searching);
        {
            let mut state = self.state.lock().await;
            if let Some(max) = running.config.max_claims {
                if state.issued_this_run + state.reserved >= max {
                    let report = Report::new(Error::QuotaReached(max));
                    self.reject(&request.hardware, &id, &report);
                    return Err(report);
                }
            }
            state.reserved += 1;
        }
        let result = self.issue(running, &id, &request, policy, &serial).await;
        if let Err(report) = &result {
            self.reject(&request.hardware, &id, report);
        }
        result
    }

    /// Journals the request, asks the platform, and queues what it issued.
    /// Holds a quota reservation, released here whatever happens.
    async fn issue(
        self: &Arc<Self>,
        running: &Running,
        id: &ClaimId,
        request: &ClaimRequest,
        policy: BoardPolicy,
        serial: &DeviceSerial,
    ) -> Result<ClaimStatus> {
        // Nothing is issued that the register doesn't know was asked for.
        let received = JournalEntry {
            claim_id: id.clone(),
            event: JournalEvent::Received {
                hardware: request.hardware.clone(),
                image: request.image.clone(),
            },
        };
        let journaled = self
            .journal
            .append(&running.config.journal, &received)
            .await
            .change_context_lazy(|| Error::Journal(running.config.journal.clone()));
        let outcome = match journaled {
            Ok(()) => self
                .factory
                .provision_csr(running.over.as_ref(), serial, &request.csr_der)
                .await
                .map_err(platform_error),
            Err(report) => Err(report),
        };

        let mut state = self.state.lock().await;
        state.reserved -= 1;
        let identity = match outcome {
            Ok(identity) => identity,
            Err(report) => {
                if let Error::AlreadyExists(_) = report.current_context() {
                    state.refused += 1;
                    let reason = report.current_context().to_string();
                    self.record(
                        running,
                        id,
                        JournalEvent::Failed {
                            reason: reason.clone(),
                        },
                    )
                    .await;
                    let record = ClaimRecord {
                        claim_id: id.clone(),
                        hardware: request.hardware.clone(),
                        image: request.image.clone(),
                        policy,
                        identity: None,
                        state: None,
                        label: None,
                        reason: Some(reason),
                    };
                    self.spawn_hooks(self.hook_run(running, HookEvent::Failed, record, 1), None);
                }
                return Err(report);
            }
        };

        state.issued_this_run += 1;
        let mut claim = Claim::new(
            id.clone(),
            request.hardware.clone(),
            request.image.clone(),
            policy.clone(),
            identity.clone(),
        );
        claim.last_seen = Some(Instant::now());
        let hub = claim.hub();
        let run = self.hook_run(running, HookEvent::Issued, claim.record(None), 1);
        state.claims.insert(id.clone(), claim);
        state.queue.push_back(id.clone());
        self.record(
            running,
            id,
            JournalEvent::Issued {
                identity,
                context: running.context.clone(),
                policy,
            },
        )
        .await;
        self.operator.show(&OperatorEvent::Issued { hub });
        self.spawn_hooks(run, None);
        self.promote(running, &mut state).await;
        Ok(self
            .status(&state, id)
            .expect("the claim was just inserted"))
    }

    async fn poll(self: &Arc<Self>, id: &ClaimId) -> Result<ClaimStatus> {
        let running = self.running()?;
        self.known(running, id)
            .await
            .ok_or_else(|| Report::new(Error::UnknownClaim(id.to_string())))
    }

    async fn ack(self: &Arc<Self>, id: &ClaimId, ack: Ack) -> Result<ClaimStatus> {
        let running = self.running()?;
        let mut state = self.state.lock().await;
        let claim = state
            .claims
            .get_mut(id)
            .ok_or_else(|| Report::new(Error::UnknownClaim(id.to_string())))?;
        claim.last_seen = Some(Instant::now());
        let (next, event, reason) = match ack {
            Ack::Installed => (ClaimState::Installed, HookEvent::Installed, None),
            Ack::Failed { reason } => (ClaimState::Failed, HookEvent::Failed, Some(reason)),
        };
        // A lost response makes a device ack twice.
        if claim.state == next {
            return Ok(self.status(&state, id).expect("a known claim"));
        }
        claim.state = next;
        let hub = claim.hub();
        let run = self.hook_run(running, event, claim.record(reason.clone()), claim.attempt);
        let was_active = state.active.as_ref() == Some(id);
        if was_active {
            state.active = None;
        }
        state.queue.retain(|queued| queued != id);
        match reason {
            None => {
                self.record(running, id, JournalEvent::Installed).await;
                self.operator.show(&OperatorEvent::Installed { hub });
            }
            Some(reason) => {
                self.record(
                    running,
                    id,
                    JournalEvent::Failed {
                        reason: reason.clone(),
                    },
                )
                .await;
                self.operator.show(&OperatorEvent::Failed { hub, reason });
            }
        }
        self.spawn_hooks(run, None);
        if was_active {
            self.advance(running, &mut state).await;
        }
        Ok(self.status(&state, id).expect("a known claim"))
    }

    async fn handle(self: &Arc<Self>, running: &Running, input: OperatorInput) {
        let mut state = self.state.lock().await;
        let Some(id) = state.active.clone() else {
            self.operator.show(&OperatorEvent::NothingActive);
            return;
        };
        match input {
            OperatorInput::Scan(scanned) => {
                self.confirm(running, &mut state, &id, Some(scanned.trim().to_string()))
                    .await
            }
            OperatorInput::Enter => match running.config.confirm {
                ConfirmMode::Key => self.confirm(running, &mut state, &id, None).await,
                ConfirmMode::Scan => self.operator.show(&OperatorEvent::ScanRequired),
            },
            OperatorInput::Reprint => {
                let claim = state
                    .claims
                    .get_mut(&id)
                    .expect("the active claim is known");
                if let Print::Running(_) = claim.print {
                    self.operator.show(&OperatorEvent::Blocked {
                        hub: claim.hub(),
                        reason: LabelBlock::Printing,
                    });
                    return;
                }
                claim.attempt += 1;
                let attempt = claim.attempt;
                self.record(running, &id, JournalEvent::Reprint { attempt })
                    .await;
                self.print(running, &mut state, &id);
            }
            OperatorInput::Skip => {
                let claim = state
                    .claims
                    .get_mut(&id)
                    .expect("the active claim is known");
                claim.label = LabelState::Queued;
                let hub = claim.hub();
                state.active = None;
                state.queue.push_back(id.clone());
                self.record(running, &id, JournalEvent::LabelSkipped).await;
                self.operator.show(&OperatorEvent::Skipped { hub });
                self.advance(running, &mut state).await;
            }
            OperatorInput::Force => {
                let claim = state
                    .claims
                    .get_mut(&id)
                    .expect("the active claim is known");
                claim.forced = true;
                let hub = claim.hub();
                self.record(running, &id, JournalEvent::LabelForced).await;
                self.operator.show(&OperatorEvent::Forced { hub });
            }
            // `operate` stops on it before getting here.
            OperatorInput::Quit => {}
        }
    }

    /// Validates the active claim's label: `scanned` must be its serial
    /// (`None`: Enter alone, in `confirm: key` mode), and `label.d` must
    /// have succeeded unless the operator forced it.
    async fn confirm(
        self: &Arc<Self>,
        running: &Running,
        state: &mut State,
        id: &ClaimId,
        scanned: Option<String>,
    ) {
        let claim = state.claims.get_mut(id).expect("the active claim is known");
        let expected = &claim.identity.serial_number;
        if let Some(scanned) = &scanned {
            if scanned != expected {
                let expected = expected.clone();
                self.record(
                    running,
                    id,
                    JournalEvent::LabelMismatch {
                        scanned: scanned.clone(),
                    },
                )
                .await;
                self.operator.show(&OperatorEvent::Mismatch {
                    expected,
                    scanned: scanned.clone(),
                });
                return;
            }
        }
        let block = match claim.print {
            Print::Running(_) => Some(LabelBlock::Printing),
            Print::Failed if !claim.forced => Some(LabelBlock::PrintFailed),
            _ => None,
        };
        if let Some(reason) = block {
            self.operator.show(&OperatorEvent::Blocked {
                hub: claim.hub(),
                reason,
            });
            return;
        }
        claim.label = LabelState::Labelled;
        let hub = claim.hub();
        let run = self.hook_run(
            running,
            HookEvent::Labelled,
            claim.record(None),
            claim.attempt,
        );
        state.active = None;
        self.record(running, id, JournalEvent::Labelled { scanned })
            .await;
        self.operator.show(&OperatorEvent::Labelled {
            hub,
            counters: self.counters(state),
        });
        self.spawn_hooks(run, None);
        self.advance(running, state).await;
    }

    /// Requeues the active claim once its device stopped polling.
    async fn check_presence(self: &Arc<Self>, running: &Running) {
        let mut state = self.state.lock().await;
        let timeout = running.config.presence_timeout;
        let lost = state
            .active
            .clone()
            .filter(|id| !state.claims[id].present(timeout));
        let Some(id) = lost else {
            self.promote(running, &mut state).await;
            return;
        };
        let claim = state
            .claims
            .get_mut(&id)
            .expect("the active claim is known");
        claim.label = LabelState::Queued;
        let hub = claim.hub();
        state.active = None;
        state.queue.push_back(id.clone());
        self.record(running, &id, JournalEvent::LabelLost).await;
        self.operator.show(&OperatorEvent::Lost { hub });
        self.advance(running, &mut state).await;
    }
}

/// Rebuilds the claims from the journal: issued ones with their identity
/// (a retry gets it back without asking the platform), labelled ones
/// labelled, and every claim still waiting for its label back in the
/// queue, in the order it was issued.
fn replay(state: &mut State, entries: Vec<JournalEntry>) {
    let mut received: HashMap<ClaimId, (HardwareInfo, ImageInfo)> = HashMap::new();
    let mut order = Vec::new();
    for JournalEntry { claim_id, event } in entries {
        match event {
            JournalEvent::Received { hardware, image } => {
                received.insert(claim_id, (hardware, image));
            }
            JournalEvent::Issued {
                identity, policy, ..
            } => {
                let (hardware, image) = received.get(&claim_id).cloned().unwrap_or_default();
                if !state.claims.contains_key(&claim_id) {
                    order.push(claim_id.clone());
                }
                let claim = Claim::new(claim_id.clone(), hardware, image, policy, identity);
                state.claims.insert(claim_id, claim);
            }
            event => {
                let Some(claim) = state.claims.get_mut(&claim_id) else {
                    continue;
                };
                match event {
                    JournalEvent::LabelActive { attempt } | JournalEvent::Reprint { attempt } => {
                        claim.attempt = claim.attempt.max(attempt)
                    }
                    JournalEvent::Labelled { .. } => claim.label = LabelState::Labelled,
                    JournalEvent::Installed => claim.state = ClaimState::Installed,
                    JournalEvent::Failed { .. } => claim.state = ClaimState::Failed,
                    _ => {}
                }
            }
        }
    }
    state.queue = order
        .into_iter()
        .filter(|id| state.claims[id].waiting_for_label())
        .collect();
}

#[async_trait::async_trait]
impl StationServiceInterface for StationControllerImpl {
    async fn start(
        &self,
        config: StationConfig,
        over: Option<ContextOverride>,
    ) -> Result<StationSummary> {
        let shared = &self.0;
        if shared.running.get().is_some() {
            return Err(Report::new(Error::AlreadyStarted));
        }
        let context = shared
            .contexts
            .resolve(over.as_ref())
            .await
            .map_err(|report| {
                let message = report.current_context().to_string();
                report.change_context(Error::Context(message))
            })?;
        shared
            .hooks
            .check(&config.hooks)
            .await
            .change_context_lazy(|| Error::Hooks(config.hooks.clone()))?;
        let entries = shared
            .journal
            .load(&config.journal)
            .await
            .change_context_lazy(|| Error::LoadJournal(config.journal.clone()))?;

        let mut state = shared.state.lock().await;
        replay(&mut state, entries);
        let summary = StationSummary {
            context: context.context.name.clone(),
            restored: state.claims.len(),
            awaiting_label: state.queue.len(),
        };
        shared
            .running
            .set(Running {
                config,
                over,
                context: context.context.name,
            })
            .map_err(|_| Report::new(Error::AlreadyStarted))?;
        Ok(summary)
    }

    async fn hello(&self) -> Result<Hello> {
        Ok(Hello {
            environment: self.0.running()?.context.clone(),
        })
    }

    async fn submit(&self, request: ClaimRequest) -> Result<ClaimStatus> {
        self.0.submit(request).await
    }

    async fn status(&self, claim: &ClaimId) -> Result<ClaimStatus> {
        self.0.poll(claim).await
    }

    async fn ack(&self, claim: &ClaimId, ack: Ack) -> Result<ClaimStatus> {
        self.0.ack(claim, ack).await
    }

    async fn operate(&self) -> Result<()> {
        let shared = &self.0;
        let running = shared.running()?;
        let tick = (running.config.presence_timeout / 4)
            .clamp(Duration::from_millis(50), Duration::from_secs(1));
        let mut ticker = tokio::time::interval(tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut reading = true;
        loop {
            tokio::select! {
                input = shared.operator.read(), if reading => {
                    match input.change_context(Error::Operator)? {
                        None => reading = false,
                        Some(OperatorInput::Quit) => return Ok(()),
                        Some(input) => shared.handle(running, input).await,
                    }
                }
                _ = ticker.tick() => shared.check_presence(running).await,
            }
        }
    }
}
