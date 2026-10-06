use std::{
    collections::{HashMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex as SyncMutex, OnceLock,
    },
    time::{Duration, Instant},
};

use error_stack::{AttachmentKind, FrameKind, Report, ResultExt};
use remora_context::{application::ContextService, model::ContextOverride};
use remora_factory::{
    adapter::provisioning::{self, ProvisionedIdentity},
    application::{self as factory, FactoryService},
    model::DeviceSerial,
};
use remora_station::{
    adapter::{
        hooks::{ClaimRecord, HookEvent, HookInvocation, HookRun, HookRunnerAdapterService},
        journal::{self, JournalAdapterService, JournalEntry, JournalEvent},
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
use tokio::{
    sync::{mpsc, oneshot, Mutex, OwnedMutexGuard},
    task::JoinSet,
};

/// The station vertical's use case. Issues through the injected
/// `FactoryService` as the context `start` checked (the CSR verified, its
/// key fingerprint naming the claim), keeps every claim in memory behind
/// one lock, journals each transition through the injected
/// `JournalAdapterService` (and rebuilds from it at `start`), runs hooks
/// through the injected `HookRunnerAdapterService` off that lock, and talks
/// to the operator through the injected `OperatorAdapterService`.
///
/// The platform call is the one slow step and is made outside the state
/// lock, so claims issue in parallel; a lock per claim id makes concurrent
/// requests for one key wait for the first one's answer instead of
/// asking twice. Disk syncs are off that lock too: transitions hand their
/// journal entries, in order, to a single writer task, and only what must
/// be on disk before a device hears of it (`Received`, `Issued`) is waited
/// for -- by the request concerned, without the lock.
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
            commits: Mutex::new(()),
            searching: AtomicUsize::new(0),
            hook_runs: SyncMutex::new(Some(JoinSet::new())),
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
    /// Taken by `commit` from its journal write to its queueing: the queue
    /// then holds claims in the journal's order, which is the order a
    /// restart rebuilds it in. Commits wait on each other's disk write;
    /// polls, which only need `state`, don't.
    commits: Mutex<()>,
    /// Claims waiting on the platform.
    searching: AtomicUsize,
    /// The hook runs under way, so that `shutdown` can wait for them (and
    /// stop them past `hook-timeout`); `None` once it has started, when no
    /// new run starts.
    hook_runs: SyncMutex<Option<JoinSet<()>>>,
}

struct Running {
    config: StationConfig,
    over: Option<ContextOverride>,
    /// The resolved context's name.
    context: String,
    /// The journal's single writer (see `write_journal`).
    journal: mpsc::UnboundedSender<Write>,
}

#[derive(Debug)]
enum Write {
    /// An entry; its outcome goes to the sender when one waits for it,
    /// else a failure is the writer's to report.
    Append(
        Box<JournalEntry>,
        Option<oneshot::Sender<journal::Result<()>>>,
    ),
    /// Answered once every entry sent before it has been appended.
    Flush(oneshot::Sender<()>),
}

#[derive(Default)]
struct State {
    claims: HashMap<ClaimId, Claim>,
    /// Issued by the platform, but not in the journal yet: held back (the
    /// device is told to retry) until the journal records them, so that a
    /// retry is answered from here and never calls the platform again.
    pending: HashMap<ClaimId, Claim>,
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

/// Holds a claim id's lock: one issuance per key at a time. Its table
/// entry goes with the last holder, so the table only ever holds keys
/// being issued, not every key ever seen.
struct ClaimSlot<'a> {
    locks: &'a SyncMutex<HashMap<ClaimId, Arc<Mutex<()>>>>,
    id: ClaimId,
    _guard: OwnedMutexGuard<()>,
}

impl Drop for ClaimSlot<'_> {
    fn drop(&mut self) {
        let mut locks = self
            .locks
            .lock()
            .expect("the claim lock table is never poisoned");
        // The table's reference and this guard's, and no request waiting:
        // every clone is made under the table's lock, so none can appear
        // meanwhile.
        if locks
            .get(&self.id)
            .is_some_and(|lock| Arc::strong_count(lock) <= 2)
        {
            locks.remove(&self.id);
        }
    }
}

/// What a factory failure means for the device: its CSR (it makes a new
/// key), the station's configuration or the platform's refusal (it waits
/// for someone to fix it), an existing device name, or a platform that
/// can't be reached or no longer accepts the context's credentials (it
/// retries, once someone has acted). The provisioning port's error says
/// which, a few hops down the chain.
fn platform_error(report: Report<factory::Error>) -> Report<Error> {
    let next = match report.current_context() {
        factory::Error::InvalidCsr => Error::InvalidCsr,
        factory::Error::MissingAccessUrl => Error::MissingAccessUrl,
        factory::Error::Context(_) => Error::Unavailable,
        _ => match report.downcast_ref::<provisioning::Error>() {
            Some(provisioning::Error::AlreadyExists(name)) => Error::AlreadyExists(name.clone()),
            Some(provisioning::Error::Refused) => Error::Refused,
            _ => Error::Unavailable,
        },
    };
    report.change_context(next)
}

/// Every context and printable attachment of `report`, outermost first,
/// each once: what the operator reads about a refusal.
fn describe<C>(report: &Report<C>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for frame in report.frames() {
        let part = match frame.kind() {
            FrameKind::Context(context) => context.to_string(),
            FrameKind::Attachment(AttachmentKind::Printable(printable)) => printable.to_string(),
            FrameKind::Attachment(_) => continue,
        };
        if !parts.contains(&part) {
            parts.push(part);
        }
    }
    parts.join(": ")
}

/// The journal's one writer: appends in the order entries were sent --
/// under the state lock for transitions, so the journal's order is the
/// state's -- so that no request waits on a disk sync unless it must (see
/// `record_durably`). Alerts the operator when the journal starts failing,
/// and says when it works again.
async fn write_journal(
    journal: JournalAdapterService,
    operator: OperatorAdapterService,
    path: PathBuf,
    mut writes: mpsc::UnboundedReceiver<Write>,
) {
    let mut failing = false;
    while let Some(write) = writes.recv().await {
        let (entry, reply) = match write {
            Write::Append(entry, reply) => (entry, reply),
            Write::Flush(done) => {
                let _ = done.send(());
                continue;
            }
        };
        let appended = journal.append(&path, &entry).await;
        match (&appended, failing) {
            (Err(report), false) => {
                failing = true;
                operator.show(&OperatorEvent::Alert(format!(
                    "{report} -- new claims are turned away until it can be written again; \
                     the production register is missing an entry for claim {}",
                    entry.claim_id
                )));
            }
            (Err(_), true) if reply.is_none() => {
                operator.show(&OperatorEvent::Warning(format!(
                    "the production register is missing an entry for claim {}",
                    entry.claim_id
                )));
            }
            (Ok(()), true) => {
                failing = false;
                operator.show(&OperatorEvent::Warning(format!(
                    "the journal {} can be written again",
                    path.display()
                )));
            }
            _ => {}
        }
        if let Some(reply) = reply {
            let _ = reply.send(appended);
        }
    }
}

impl Shared {
    fn running(&self) -> Result<&Running> {
        self.running
            .get()
            .ok_or_else(|| Report::new(Error::NotStarted))
    }

    async fn claim_slot(&self, id: &ClaimId) -> ClaimSlot<'_> {
        let lock = self
            .claim_locks
            .lock()
            .expect("the claim lock table is never poisoned")
            .entry(id.clone())
            .or_default()
            .clone();
        ClaimSlot {
            locks: &self.claim_locks,
            id: id.clone(),
            _guard: lock.lock_owned().await,
        }
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

    /// Journals `event` without waiting for the disk: the writer appends
    /// it after everything sent before, and reports a failure itself (once
    /// an identity exists, refusing the device would help no one).
    fn record(&self, running: &Running, claim_id: &ClaimId, event: JournalEvent) {
        let entry = JournalEntry {
            claim_id: claim_id.clone(),
            event,
        };
        // The writer lives as long as `running`.
        let _ = running.journal.send(Write::Append(Box::new(entry), None));
    }

    /// Journals `event` and waits until it is on disk: for what the
    /// register must hold before a device hears of it.
    async fn record_durably(
        &self,
        running: &Running,
        claim_id: &ClaimId,
        event: JournalEvent,
    ) -> Result<()> {
        let failed = || Error::Journal(running.config.journal.clone());
        let (reply, appended) = oneshot::channel();
        let entry = JournalEntry {
            claim_id: claim_id.clone(),
            event,
        };
        running
            .journal
            .send(Write::Append(Box::new(entry), Some(reply)))
            .change_context_lazy(failed)?;
        appended
            .await
            .change_context_lazy(failed)?
            .change_context_lazy(failed)
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
    /// label, which reads the outcome of `print`'s run from the claim, and
    /// `shutdown`, which waits for them all.
    fn spawn_hooks(self: &Arc<Self>, run: HookRun, print: Option<u64>) {
        let mut runs = self
            .hook_runs
            .lock()
            .expect("the hook runs are never poisoned");
        let Some(runs) = runs.as_mut() else {
            self.operator.show(&OperatorEvent::Warning(format!(
                "{} hooks not run for claim {}: the station is stopping",
                run.invocation.event.name(),
                run.invocation.claim.claim_id
            )));
            return;
        };
        // Reaps the runs already over: the set only holds those under way.
        while runs.try_join_next().is_some() {}
        let shared = Arc::clone(self);
        runs.spawn(async move { shared.run_hooks(run, print).await });
    }

    async fn run_hooks(&self, run: HookRun, print: Option<u64>) {
        let outcomes = self.hooks.run(&run).await;
        let Ok(running) = self.running() else {
            return;
        };
        let invocation = &run.invocation;
        let hub = hub_of(&invocation.claim);
        let succeeded = match outcomes {
            Ok(outcomes) => {
                for outcome in &outcomes {
                    self.record(
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
                    );
                    self.operator.show(&OperatorEvent::Hook {
                        event: invocation.event,
                        hub: hub.clone(),
                        outcome: outcome.clone(),
                    });
                }
                outcomes.iter().all(|outcome| outcome.succeeded())
            }
            Err(report) => {
                self.operator.show(&OperatorEvent::Warning(format!(
                    "{} hooks did not run: {report}",
                    invocation.event.name()
                )));
                false
            }
        };
        // Only a label print's outcome changes the claim.
        let Some(print) = print else {
            return;
        };
        let mut state = self.state.lock().await;
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
            self.operator.show(&OperatorEvent::Blocked {
                hub: claim.hub(),
                reason: LabelBlock::PrintFailed,
            });
        }
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
    fn promote(self: &Arc<Self>, running: &Running, state: &mut State) -> bool {
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
        self.record(running, &id, JournalEvent::LabelActive { attempt });
        self.operator.show(&OperatorEvent::Active {
            hub,
            attempt,
            counters: self.counters(state),
        });
        self.print(running, state, &id);
        true
    }

    /// After the active claim left: the next one, or say there's none.
    fn advance(self: &Arc<Self>, running: &Running, state: &mut State) {
        if !self.promote(running, state) {
            self.operator.show(&OperatorEvent::Idle {
                counters: self.counters(state),
            });
        }
    }

    /// The status of a claim already issued, counting this as a poll.
    async fn known(self: &Arc<Self>, running: &Running, id: &ClaimId) -> Option<ClaimStatus> {
        let mut state = self.state.lock().await;
        state.claims.get_mut(id)?.last_seen = Some(Instant::now());
        self.promote(running, &mut state);
        self.status(&state, id)
    }

    /// Tells the operator about a claim turned away: an alert when it's
    /// for them to fix (the platform, the context's credentials), nothing
    /// more for the journal (its writer already alerted).
    fn reject(&self, hardware: &HardwareInfo, id: &ClaimId, report: &Report<Error>) {
        let hub = HubSummary {
            claim_id: id.clone(),
            serial: None,
            board: hardware.board.clone(),
            temp_hostname: hardware.temp_hostname.clone(),
            eth_mac: hardware.eth_mac.clone(),
        };
        match report.current_context() {
            Error::Journal(_) => {}
            Error::Unavailable => self.operator.show(&OperatorEvent::Alert(format!(
                "Turned away a {} hub ({}) until the platform answers: {}",
                hub.board,
                hub.temp_hostname,
                describe(report)
            ))),
            _ => self.operator.show(&OperatorEvent::Rejected {
                hub,
                reason: describe(report),
            }),
        }
    }

    async fn submit(self: &Arc<Self>, request: ClaimRequest) -> Result<ClaimStatus> {
        let running = self.running()?;
        // Before anything of it is shown, journaled or templated.
        request.validate().map_err(|error| {
            self.operator
                .show(&OperatorEvent::Warning(format!("Refused a claim: {error}")));
            Report::new(error).change_context(Error::InvalidHardware)
        })?;
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
            let report = Report::new(error).change_context(Error::InvalidHardware);
            self.reject(&request.hardware, &id, &report);
            report
        })?;

        let _slot = self.claim_slot(&id).await;
        // A concurrent request for this key may have been issued while
        // this one waited -- or issued, but not journaled yet.
        if let Some(status) = self.known(running, &id).await {
            return Ok(status);
        }
        if self.state.lock().await.pending.contains_key(&id) {
            return self.commit(running, &id).await;
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

    /// Journals the request, asks the platform, and commits what it
    /// issued. Holds a quota reservation, released here whatever happens.
    async fn issue(
        self: &Arc<Self>,
        running: &Running,
        id: &ClaimId,
        request: &ClaimRequest,
        policy: BoardPolicy,
        serial: &DeviceSerial,
    ) -> Result<ClaimStatus> {
        // Nothing is issued that the register doesn't know was asked for:
        // a journal that can't be written turns new claims away here.
        let received = JournalEvent::Received {
            hardware: request.hardware.clone(),
            image: request.image.clone(),
        };
        let outcome = match self.record_durably(running, id, received).await {
            Ok(()) => self
                .factory
                .provision_csr(running.over.as_ref(), serial, &request.csr_der)
                .await
                .map_err(platform_error),
            Err(report) => Err(report),
        };

        {
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
                        );
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
                        self.spawn_hooks(
                            self.hook_run(running, HookEvent::Failed, record, 1),
                            None,
                        );
                    }
                    return Err(report);
                }
            };
            state.issued_this_run += 1;
            let claim = Claim::new(
                id.clone(),
                request.hardware.clone(),
                request.image.clone(),
                policy,
                identity,
            );
            state.pending.insert(id.clone(), claim);
        }
        self.commit(running, id).await
    }

    /// Journals the identity issued for pending claim `id`, then hands it
    /// out: queued, announced, `issued.d` started. Until the journal has
    /// it, the claim stays pending and the device is told to retry --
    /// answered from here, never by a second platform call: a serial the
    /// register doesn't know would be lost to the next restart.
    async fn commit(self: &Arc<Self>, running: &Running, id: &ClaimId) -> Result<ClaimStatus> {
        let _order = self.commits.lock().await;
        let (identity, policy) = {
            let state = self.state.lock().await;
            let claim = state.pending.get(id).expect("committing a pending claim");
            (claim.identity.clone(), claim.policy.clone())
        };
        let serial = identity.serial_number.clone();
        let issued = JournalEvent::Issued {
            identity,
            context: running.context.clone(),
            policy,
        };
        if let Err(report) = self.record_durably(running, id, issued).await {
            self.operator.show(&OperatorEvent::Alert(format!(
                "Serial {serial} was issued for claim {id}, but the journal can't record it: \
                 held back until the journal can be written and its hub retries"
            )));
            return Err(report);
        }

        let mut state = self.state.lock().await;
        let mut claim = state.pending.remove(id).expect("a pending claim");
        claim.last_seen = Some(Instant::now());
        let hub = claim.hub();
        let run = self.hook_run(running, HookEvent::Issued, claim.record(None), 1);
        state.claims.insert(id.clone(), claim);
        state.queue.push_back(id.clone());
        self.operator.show(&OperatorEvent::Issued { hub });
        self.spawn_hooks(run, None);
        self.promote(running, &mut state);
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

    /// A device's ack. `installed` only once its label is validated -- the
    /// commit point: until then nobody on the workshop network can mark a
    /// device installed or fire `installed.d` with its claim id. `failed`
    /// at any time: a device checks its identity as soon as it gets it,
    /// and one it rejects must not be labelled -- it leaves the queue (the
    /// next device becomes active if it was), keeping its label state
    /// otherwise. A `failed` device may still ack `installed` once
    /// labelled (it wrote its identity after all); repeating an ack
    /// changes nothing.
    async fn ack(self: &Arc<Self>, id: &ClaimId, ack: Ack) -> Result<ClaimStatus> {
        let running = self.running()?;
        let ack = ack.sanitized();
        let mut state = self.state.lock().await;
        let claim = state
            .claims
            .get_mut(id)
            .ok_or_else(|| Report::new(Error::UnknownClaim(id.to_string())))?;
        claim.last_seen = Some(Instant::now());
        let (next, event, reason) = match (ack, claim.state) {
            // A lost response makes a device ack twice: the same answer,
            // and no second hook run.
            (Ack::Installed, ClaimState::Installed) | (Ack::Failed { .. }, ClaimState::Failed) => {
                return Ok(self.status(&state, id).expect("a known claim"))
            }
            (Ack::Installed, _) if claim.label != LabelState::Labelled => {
                return Err(Report::new(Error::NotLabelled(id.to_string())))
            }
            (Ack::Installed, _) => (ClaimState::Installed, HookEvent::Installed, None),
            (Ack::Failed { reason }, _) => (ClaimState::Failed, HookEvent::Failed, Some(reason)),
        };
        claim.state = next;
        // Out of the labelling queue: a failed claim no longer waits.
        if claim.label == LabelState::Active {
            claim.label = LabelState::Queued;
        }
        let hub = claim.hub();
        let run = self.hook_run(running, event, claim.record(reason.clone()), claim.attempt);
        let was_active = state.active.as_ref() == Some(id);
        if was_active {
            state.active = None;
        }
        state.queue.retain(|queued| queued != id);
        match reason {
            None => {
                self.record(running, id, JournalEvent::Installed);
                self.operator.show(&OperatorEvent::Installed { hub });
            }
            Some(reason) => {
                self.record(
                    running,
                    id,
                    JournalEvent::Failed {
                        reason: reason.clone(),
                    },
                );
                self.operator.show(&OperatorEvent::Failed { hub, reason });
            }
        }
        self.spawn_hooks(run, None);
        if was_active {
            self.advance(running, &mut state);
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
            }
            OperatorInput::Enter => match running.config.confirm {
                ConfirmMode::Key => self.confirm(running, &mut state, &id, None),
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
                self.record(running, &id, JournalEvent::Reprint { attempt });
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
                self.record(running, &id, JournalEvent::LabelSkipped);
                self.operator.show(&OperatorEvent::Skipped { hub });
                self.advance(running, &mut state);
            }
            OperatorInput::Force => {
                let claim = state
                    .claims
                    .get_mut(&id)
                    .expect("the active claim is known");
                claim.forced = true;
                let hub = claim.hub();
                self.record(running, &id, JournalEvent::LabelForced);
                self.operator.show(&OperatorEvent::Forced { hub });
            }
        }
    }

    /// Validates the active claim's label: `scanned` must be its serial
    /// (`None`: Enter alone, in `confirm: key` mode), and `label.d` must
    /// have succeeded unless the operator forced it.
    fn confirm(
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
                );
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
        self.record(running, id, JournalEvent::Labelled { scanned });
        self.operator.show(&OperatorEvent::Labelled {
            hub,
            counters: self.counters(state),
        });
        self.spawn_hooks(run, None);
        self.advance(running, state);
    }

    /// Requeues the active claim once its device stopped polling, and
    /// activates the next one still polling.
    async fn tick(self: &Arc<Self>, running: &Running) {
        let mut state = self.state.lock().await;
        let timeout = running.config.presence_timeout;
        let lost = state
            .active
            .clone()
            .filter(|id| !state.claims[id].present(timeout));
        let Some(id) = lost else {
            self.promote(running, &mut state);
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
        self.record(running, &id, JournalEvent::LabelLost);
        self.operator.show(&OperatorEvent::Lost { hub });
        self.advance(running, &mut state);
    }

    /// Lets the hook runs under way finish within `hook-timeout` (their
    /// outcomes journaled), stops those still going past it -- killing
    /// their scripts, rather than leaving them orphaned when the process
    /// exits -- then waits until the journal has every entry.
    async fn shutdown(&self, running: &Running) {
        let runs = self
            .hook_runs
            .lock()
            .expect("the hook runs are never poisoned")
            .take();
        if let Some(mut runs) = runs {
            let drained = tokio::time::timeout(running.config.hook_timeout, async {
                while runs.join_next().await.is_some() {}
            })
            .await;
            if drained.is_err() {
                let stopped = runs.len();
                runs.shutdown().await;
                self.operator.show(&OperatorEvent::Warning(format!(
                    "stopped {stopped} hook run(s) still going after {}s: their outcome is not \
                     in the journal",
                    running.config.hook_timeout.as_secs_f32()
                )));
            }
        }
        let (flush, flushed) = oneshot::channel();
        if running.journal.send(Write::Flush(flush)).is_ok() {
            let _ = flushed.await;
        }
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
        // Asks the platform, not just the context store: a station whose
        // credentials were revoked would otherwise turn every hub away.
        let (resolved, _, _) = shared
            .contexts
            .whoami(over.as_ref())
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
            context: resolved.context.name.clone(),
            restored: state.claims.len(),
            awaiting_label: state.queue.len(),
            tick: (config.presence_timeout / 4)
                .clamp(Duration::from_millis(50), Duration::from_secs(1)),
        };
        let (journal, writes) = mpsc::unbounded_channel();
        let running = Running {
            over,
            context: resolved.context.name,
            journal,
            config,
        };
        let path = running.config.journal.clone();
        if shared.running.set(running).is_err() {
            return Err(Report::new(Error::AlreadyStarted));
        }
        tokio::spawn(write_journal(
            shared.journal.clone(),
            shared.operator.clone(),
            path,
            writes,
        ));
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

    async fn handle(&self, input: OperatorInput) -> Result<()> {
        let running = self.0.running()?;
        self.0.handle(running, input).await;
        Ok(())
    }

    async fn tick(&self) -> Result<()> {
        let running = self.0.running()?;
        self.0.tick(running).await;
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        let running = self.0.running()?;
        self.0.shutdown(running).await;
        Ok(())
    }
}
