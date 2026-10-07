use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

use remora_progress::{OperationEvent, Unit};
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
                OperationEvent::Progress { done, total, unit } => {
                    let phase = current.get_or_insert_with(|| Phase::start("working".into()));
                    phase.progress(done, total, unit);
                }
                OperationEvent::Transfer { done, total } => {
                    let phase = current.get_or_insert_with(|| Phase::start("working".into()));
                    phase.transfer(done, total);
                }
                OperationEvent::Log(message) => match &mut current {
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
    /// Set once the phase reports bytes: its throughput, for the line
    /// under the bar.
    rate: Option<Rate>,
    /// Set once the phase reports a transfer (a download feeding it): its
    /// throughput and total, for a third line.
    transfer: Option<(Rate, u64)>,
    /// The bytes line under a byte bar, as last drawn.
    detail: String,
    /// The phase's last log lines, under everything else.
    logs: VecDeque<String>,
    /// Off a terminal: the last line printed, its digits blanked, and
    /// when -- a counter ticking ("waiting … (14s)") isn't news each tick.
    printed: Option<(String, Instant)>,
}

/// Off a terminal, how often a line that only differs from the previous
/// one by its numbers is printed again.
const REPEAT_EVERY: Duration = Duration::from_secs(30);

/// How many of a phase's log lines stay on screen under it.
const LOG_LINES: usize = 3;

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
            rate: None,
            transfer: None,
            detail: String::new(),
            logs: VecDeque::new(),
            printed: None,
        }
    }

    fn progress(&mut self, done: u64, total: u64, unit: Unit) {
        if total == 0 {
            return;
        }
        if unit == Unit::Bytes {
            self.rate.get_or_insert_with(Rate::new).sample(done);
        }
        let Some(line) = &self.line else {
            let tenth = done.min(total) * 10 / total;
            if tenth > self.last_tenth {
                self.last_tenth = tenth;
                let detail = self.rate.as_ref().map(|rate| rate.detail(done, total));
                let transfer = self.transfer_line();
                let line = [detail, transfer].into_iter().flatten().collect::<Vec<_>>();
                match line.is_empty() {
                    true => eprintln!("  {}%", tenth * 10),
                    false => eprintln!("  {}%  {}", tenth * 10, line.join(" · ")),
                }
            }
            return;
        };
        if !self.bar {
            // A spinner can't become a bar: swap it for one. Bytes get their
            // sizes and throughput on a line of their own under the bar;
            // anything else (files for a copy) a percentage and an ETA.
            line.clear();
            let name = self.name.replace('{', "{{").replace('}', "}}");
            // `{msg}` holds the lines under the bar, each starting with its
            // own newline: none at all when there's nothing to show.
            let template = match self.rate {
                Some(_) => format!("{name} [{{bar:30.cyan/blue}}] {{percent:>3}}%{{msg}}"),
                None => {
                    format!("{name} [{{bar:30.cyan/blue}}] {{percent:>3}}% · {{eta}} left{{msg}}")
                }
            };
            let bar = cliclack::progress_bar(total).with_template(&template);
            bar.start("");
            self.line = Some(bar);
            self.bar = true;
        }
        if let Some(bar) = &self.line {
            bar.set_length(total);
            bar.set_position(done);
        }
        if let Some(rate) = &self.rate {
            self.detail = rate.detail(done, total);
        }
        self.redraw();
    }

    fn transfer(&mut self, done: u64, total: u64) {
        let (rate, known) = self.transfer.get_or_insert_with(|| (Rate::new(), total));
        rate.sample(done);
        *known = total;
        self.redraw();
    }

    /// `↓ 120.3 MiB of 548.3 MiB downloaded · 12.1 MiB/s`
    fn transfer_line(&self) -> Option<String> {
        let (rate, total) = self.transfer.as_ref()?;
        let sizes = match *total {
            0 => format!("↓ {} downloaded", crate::bytes(rate.last)),
            total => format!(
                "↓ {} of {} downloaded",
                crate::bytes(rate.last),
                crate::bytes(total)
            ),
        };
        Some(match rate.per_second() {
            Some(speed) if rate.last < *total || *total == 0 => {
                format!("{sizes} · {}/s", crate::bytes(speed as u64))
            }
            _ => sizes,
        })
    }

    /// The lines under the phase's bar or spinner: the bytes detail, the
    /// transfer feeding it, then its last log lines -- each on its own
    /// line, after a newline, so that none means nothing at all.
    fn lines(&self) -> String {
        let detail = (!self.detail.is_empty()).then(|| self.detail.clone());
        detail
            .into_iter()
            .chain(self.transfer_line())
            .chain(self.logs.iter().cloned())
            .map(|line| format!("\n{}  {}", crate::dim("│"), crate::dim(line)))
            .collect()
    }

    fn redraw(&self) {
        match &self.line {
            // A bar's name is in its template.
            Some(bar) if self.bar => bar.set_message(self.lines()),
            Some(spinner) => spinner.set_message(format!("{}{}", self.name, self.lines())),
            None => {}
        }
    }

    fn log(&mut self, message: &str) {
        if self.line.is_none() {
            let shape: String = message
                .chars()
                .map(|c| if c.is_ascii_digit() { '#' } else { c })
                .collect();
            let repeat = self
                .printed
                .as_ref()
                .is_some_and(|(last, at)| *last == shape && at.elapsed() < REPEAT_EVERY);
            if !repeat {
                eprintln!("  {message}");
                self.printed = Some((shape, Instant::now()));
            }
            return;
        }
        if self.logs.len() == LOG_LINES {
            self.logs.pop_front();
        }
        self.logs.push_back(message.to_owned());
        self.redraw();
    }

    fn done(self) {
        let downloaded = self
            .transfer
            .as_ref()
            .map(|(rate, _)| format!("{} downloaded", crate::bytes(rate.last)));
        let summary = match (self.rate.as_ref().map(Rate::summary), downloaded) {
            (Some(summary), Some(downloaded)) => Some(format!("{summary} · {downloaded}")),
            (summary, downloaded) => summary.or(downloaded),
        };
        match (&self.line, summary) {
            (Some(line), Some(summary)) => {
                line.stop(format!("{}  {}", self.name, crate::dim(summary)))
            }
            (Some(line), None) => line.stop(&self.name),
            (None, Some(summary)) => eprintln!("  {summary}"),
            (None, None) => {}
        }
    }

    fn fail(self) {
        match &self.line {
            Some(line) => line.error(&self.name),
            None => eprintln!("{} failed", self.name),
        }
    }
}

/// How long the throughput is averaged over: long enough not to swing
/// with a link's bursts, short enough to follow a disk slowing down as its
/// cache fills.
const RATE_WINDOW: Duration = Duration::from_secs(15);

/// How much of it it takes before a throughput (and a time left) is shown:
/// the first seconds of a transfer mostly measure buffers filling.
const RATE_SETTLE: Duration = Duration::from_secs(3);

/// A byte count's throughput, over the last [`RATE_WINDOW`].
struct Rate {
    started: Instant,
    samples: VecDeque<(Instant, u64)>,
    /// Where the count started: a resumed transfer's bytes before this run
    /// aren't its own.
    first: Option<u64>,
    last: u64,
}

impl Rate {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            samples: VecDeque::new(),
            first: None,
            last: 0,
        }
    }

    fn sample(&mut self, done: u64) {
        self.first.get_or_insert(done);
        let now = Instant::now();
        self.samples.push_back((now, done));
        self.last = done;
        while self
            .samples
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > RATE_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// Bytes per second, once there's enough to tell.
    fn per_second(&self) -> Option<f64> {
        let (first, last) = (self.samples.front()?, self.samples.back()?);
        if self.started.elapsed() < RATE_SETTLE {
            return None;
        }
        let elapsed = last.0.duration_since(first.0).as_secs_f64();
        (elapsed >= 0.5).then(|| last.1.saturating_sub(first.1) as f64 / elapsed)
    }

    /// `120.3 MiB of 634.1 MiB · 31.2 MiB/s · 17s left`
    fn detail(&self, done: u64, total: u64) -> String {
        let sizes = format!("{} of {}", crate::bytes(done), crate::bytes(total));
        match self.per_second() {
            Some(speed) if done >= total => format!("{sizes} · {}/s", crate::bytes(speed as u64)),
            Some(speed) if speed >= 1.0 => format!(
                "{sizes} · {}/s · {} left",
                crate::bytes(speed as u64),
                duration(Duration::from_secs_f64(
                    total.saturating_sub(done) as f64 / speed
                ))
            ),
            _ => sizes,
        }
    }

    /// `634.1 MiB in 21s, 30.2 MiB/s`: what this run moved, not what a
    /// resumed transfer started from.
    fn summary(&self) -> String {
        let elapsed = self.started.elapsed();
        let moved = self.last.saturating_sub(self.first.unwrap_or(0));
        let speed = moved as f64 / elapsed.as_secs_f64().max(0.001);
        format!(
            "{} in {}, {}/s",
            crate::bytes(moved),
            duration(elapsed),
            crate::bytes(speed as u64)
        )
    }
}

/// `5s`, `2m 05s`, `1h 02m`.
fn duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(duration(Duration::from_secs(5)), "5s");
        assert_eq!(duration(Duration::from_secs(125)), "2m 05s");
        assert_eq!(duration(Duration::from_secs(3720)), "1h 02m");
    }

    #[test]
    fn a_rate_shows_sizes_until_it_can_tell_a_speed() {
        let mut rate = Rate::new();
        rate.sample(0);
        assert_eq!(rate.detail(0, 1024 * 1024), "0 B of 1.0 MiB");

        // Settled: some seconds in.
        rate.started = Instant::now() - Duration::from_secs(5);
        let start = Instant::now() - Duration::from_secs(2);
        rate.samples = VecDeque::from([(start, 0), (start + Duration::from_secs(2), 2 << 20)]);
        assert_eq!(
            rate.detail(2 << 20, 6 << 20),
            "2.0 MiB of 6.0 MiB · 1.0 MiB/s · 4s left"
        );
    }

    #[test]
    fn a_resumed_transfer_sums_up_what_this_run_moved() {
        let mut rate = Rate::new();
        rate.sample(43 << 20);
        rate.sample(177 << 20);
        assert!(
            rate.summary().starts_with("134.0 MiB in "),
            "{}",
            rate.summary()
        );
    }
}
