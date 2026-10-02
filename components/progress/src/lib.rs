use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;

/// A single advancement notification for a long-running operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationEvent {
    /// A coarse step change, e.g. "formatting", "copying".
    Phase(String),
    /// Fine-grained advancement, when it's known (bytes, clusters, files —
    /// whatever unit the operation counts in). `total == 0` means unknown
    /// (e.g. decoding a container format that doesn't cheaply expose its
    /// decoded size up front) — `done` is still a real, monotonically
    /// growing count, just not a percentage of anything.
    Progress { done: u64, total: u64 },
    /// A free-text detail line, for a log panel rather than a progress bar.
    Log(String),
}

/// The sending half of an operation's progress channel. Cheap to clone (an
/// `mpsc::UnboundedSender` clone), so it can be handed to a spawned polling
/// task (see `remora-flash-adapter-bmap`) alongside the main
/// operation.
#[derive(Clone)]
pub struct ProgressSink(Option<mpsc::UnboundedSender<OperationEvent>>);

impl ProgressSink {
    /// A sink with nobody listening — for callers that don't care about
    /// progress (tests, a one-shot run with no live output). Every report
    /// is a no-op rather than allocating a channel nobody drains.
    pub fn noop() -> Self {
        Self(None)
    }

    pub fn phase(&self, phase: impl Into<String>) {
        self.send(OperationEvent::Phase(phase.into()));
    }

    pub fn progress(&self, done: u64, total: u64) {
        self.send(OperationEvent::Progress { done, total });
    }

    pub fn log(&self, message: impl Into<String>) {
        self.send(OperationEvent::Log(message.into()));
    }

    fn send(&self, event: OperationEvent) {
        if let Some(tx) = &self.0 {
            // A closed receiver (the listener stopped caring) is not this
            // operation's problem to report or fail on.
            let _ = tx.send(event);
        }
    }
}

/// A fresh progress channel: the `ProgressSink` half to pass into an
/// operation, and the `Stream` half to drain (print, feed a progress bar,
/// forward to a GUI) while it runs.
pub fn channel() -> (ProgressSink, UnboundedReceiverStream<OperationEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();
    (ProgressSink(Some(tx)), UnboundedReceiverStream::new(rx))
}

/// What a long-running operation needs to report advancement and notice
/// it's been asked to stop — passed by reference so call sites stay a
/// single extra parameter regardless of how many of the two a given
/// operation actually uses.
#[derive(Clone)]
pub struct OperationContext {
    pub sink: ProgressSink,
    pub cancel: CancellationToken,
}

impl OperationContext {
    pub fn new(sink: ProgressSink, cancel: CancellationToken) -> Self {
        Self { sink, cancel }
    }

    /// No progress listener, never cancelled — for tests and any caller
    /// that just wants the operation to run to completion untouched.
    pub fn noop() -> Self {
        Self {
            sink: ProgressSink::noop(),
            cancel: CancellationToken::new(),
        }
    }
}

/// What happened to a [`track_output_file_size`]-wrapped operation.
#[derive(Debug)]
pub enum TrackedOutcome<T, E> {
    /// The work finished (successfully or not) before cancellation.
    Completed(std::result::Result<T, E>),
    /// `ctx.cancel` fired first. The blocking `work` closure is **not**
    /// preemptible -- it keeps running to completion in the background
    /// (writing to `output_path` the whole time), this just stops waiting
    /// for it. Callers are responsible for treating `output_path` as
    /// unreliable/partial from this point on.
    Cancelled,
}

/// Runs a blocking `work` closure on its own thread while polling
/// `output_path`'s growing file size against `total_bytes` on a timer,
/// reporting [`OperationEvent::Progress`] through `ctx.sink` until it
/// finishes or `ctx.cancel` fires.
///
/// For operations with no per-iteration hook of their own to report from
/// (an opaque third-party copy routine, or codec internals not worth
/// instrumenting just for this) -- `done` is the output file's current
/// size on disk, a proxy for real progress, not an exact count.
pub async fn track_output_file_size<F, T, E>(
    ctx: &OperationContext,
    output_path: PathBuf,
    total_bytes: u64,
    work: F,
) -> TrackedOutcome<T, E>
where
    F: FnOnce() -> std::result::Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    let mut handle = tokio::task::spawn_blocking(work);

    let mut interval = tokio::time::interval(Duration::from_millis(250));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            result = &mut handle => {
                return TrackedOutcome::Completed(
                    result.expect("progress-tracked blocking task panicked")
                );
            }
            _ = ctx.cancel.cancelled() => {
                return TrackedOutcome::Cancelled;
            }
            _ = interval.tick() => {
                if let Ok(meta) = std::fs::metadata(&output_path) {
                    ctx.sink.progress(meta.len(), total_bytes);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_stream::StreamExt;

    #[tokio::test]
    async fn events_flow_from_sink_to_stream() {
        let (sink, mut stream) = channel();
        sink.phase("formatting");
        sink.progress(1, 4);
        sink.log("hello");
        drop(sink);

        assert_eq!(
            stream.next().await,
            Some(OperationEvent::Phase("formatting".to_string()))
        );
        assert_eq!(
            stream.next().await,
            Some(OperationEvent::Progress { done: 1, total: 4 })
        );
        assert_eq!(
            stream.next().await,
            Some(OperationEvent::Log("hello".to_string()))
        );
        assert_eq!(stream.next().await, None);
    }

    #[test]
    fn noop_sink_never_panics_with_nobody_listening() {
        let sink = ProgressSink::noop();
        sink.phase("ignored");
        sink.progress(0, 0);
        sink.log("ignored");
    }

    #[test]
    fn noop_context_is_not_cancelled() {
        let ctx = OperationContext::noop();
        assert!(!ctx.cancel.is_cancelled());
    }

    #[tokio::test]
    async fn track_output_file_size_reports_progress_and_completes() {
        let (sink, mut stream) = channel();
        let ctx = OperationContext::new(sink, CancellationToken::new());
        let path = std::env::temp_dir().join(format!(
            "remora-progress-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let write_path = path.clone();
        let outcome =
            track_output_file_size::<_, (), std::io::Error>(&ctx, path.clone(), 8, move || {
                std::fs::write(&write_path, b"12345678")?;
                Ok(())
            })
            .await;

        assert!(matches!(outcome, TrackedOutcome::Completed(Ok(()))));
        drop(ctx);
        // At least the final state is observable even if no interval tick
        // fired before the (near-instant) work finished.
        assert_eq!(std::fs::read(&path).unwrap(), b"12345678");
        let _ = std::fs::remove_file(&path);
        // Draining is best-effort here: this test only cares that
        // reporting never panics, not how many ticks happened to fire.
        while stream.next().await.is_some() {}
    }

    #[tokio::test(start_paused = true)]
    async fn track_output_file_size_stops_waiting_once_cancelled() {
        let (sink, _stream) = channel();
        let cancel = CancellationToken::new();
        let ctx = OperationContext::new(sink, cancel.clone());
        let path = std::env::temp_dir().join("remora-progress-test-cancel");

        let handle = tokio::spawn({
            let ctx = ctx.clone();
            async move {
                track_output_file_size::<_, (), std::io::Error>(&ctx, path, 1, move || {
                    // Long enough to still be running when we assert
                    // cancellation below; short enough that the runtime
                    // teardown waiting for this spawn_blocking task (Tokio
                    // always waits for those, cancelled or not) doesn't
                    // meaningfully slow the test suite down.
                    std::thread::sleep(Duration::from_millis(200));
                    Ok(())
                })
                .await
            }
        });

        tokio::task::yield_now().await;
        cancel.cancel();
        let outcome = handle.await.unwrap();
        assert!(matches!(outcome, TrackedOutcome::Cancelled));
    }
}
