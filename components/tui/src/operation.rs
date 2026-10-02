use remora_progress::OperationEvent;
use tokio_stream::{wrappers::UnboundedReceiverStream, StreamExt};

/// Draws a long-running operation's `remora-progress` events while it runs:
/// one clack line per phase -- a spinner, turned into a bar once the phase
/// reports a known total -- ticked off when the next phase starts. Plain
/// `phase...` lines when stderr isn't a terminal.
///
/// [`follow`] it before starting the operation, then [`Follow::finish`] it
/// with the operation's result once the operation's `ProgressSink` is
/// dropped: the last phase is ticked off or marked failed accordingly.
pub struct Follow(tokio::task::JoinHandle<Option<Phase>>);

/// Starts drawing `stream`'s events; see [`Follow`].
pub fn follow(mut stream: UnboundedReceiverStream<OperationEvent>) -> Follow {
    Follow(tokio::spawn(async move {
        let mut current: Option<Phase> = None;
        while let Some(event) = stream.next().await {
            match event {
                OperationEvent::Phase(name) => {
                    if let Some(phase) = current.take() {
                        phase.done();
                    }
                    // Controllers close with a "done" phase; the ticked-off
                    // last phase already says so.
                    if name != "done" {
                        current = Some(Phase::start(name));
                    }
                }
                OperationEvent::Progress { done, total } => {
                    let phase = current.get_or_insert_with(|| Phase::start("working".into()));
                    phase.progress(done, total);
                }
                OperationEvent::Log(message) => match &current {
                    Some(phase) => phase.log(&message),
                    None => crate::step(message),
                },
            }
        }
        current
    }))
}

impl Follow {
    /// Waits for every event to be drawn, then closes the last phase as
    /// `result` says.
    pub async fn finish<T, E>(self, result: &Result<T, E>) {
        if let Ok(Some(phase)) = self.0.await {
            if result.is_ok() {
                phase.done();
            } else {
                phase.fail();
            }
        }
    }
}

/// One phase on screen.
pub struct Phase {
    name: String,
    line: Option<cliclack::ProgressBar>,
    bar: bool,
    last_tenth: u64,
}

impl Phase {
    fn start(name: String) -> Self {
        let line = crate::decorated().then(|| {
            let line = cliclack::spinner();
            line.start(&name);
            line
        });
        if line.is_none() {
            eprintln!("{name}...");
        }
        Self {
            name,
            line,
            bar: false,
            last_tenth: 0,
        }
    }

    fn progress(&mut self, done: u64, total: u64) {
        if total == 0 {
            return;
        }
        let Some(line) = &self.line else {
            let tenth = done.min(total) * 10 / total;
            if tenth > self.last_tenth {
                self.last_tenth = tenth;
                eprintln!("  {}%", tenth * 10);
            }
            return;
        };
        if !self.bar {
            // A spinner can't become a bar: swap it for one. Unit-agnostic
            // (bytes for a flash, files for a copy), hence a percentage.
            line.clear();
            let bar = cliclack::progress_bar(total)
                .with_template("{msg} [{bar:30.cyan/blue}] {percent:>3}% {eta}");
            bar.start(&self.name);
            self.line = Some(bar);
            self.bar = true;
        }
        if let Some(bar) = &self.line {
            bar.set_length(total);
            bar.set_position(done);
        }
    }

    fn log(&self, message: &str) {
        match &self.line {
            Some(line) => line.set_message(format!("{} {}", self.name, crate::dim(message))),
            None => eprintln!("  {message}"),
        }
    }

    fn done(self) {
        if let Some(line) = &self.line {
            line.stop(&self.name);
        }
    }

    fn fail(self) {
        match &self.line {
            Some(line) => line.error(&self.name),
            None => eprintln!("{} failed", self.name),
        }
    }
}
