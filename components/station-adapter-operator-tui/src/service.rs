use std::io::BufRead;

use remora_station::adapter::{
    hooks::HookOutcome,
    operator::{
        Counters, HubSummary, LabelBlock, OperatorAdapter, OperatorEvent, OperatorInput, Result,
    },
};
use remora_tui as tui;
use tokio::sync::{mpsc, Mutex};

/// How many trailing lines of a hook's output the dashboard shows (the
/// journal has all of it).
const OUTPUT_LINES: usize = 3;

/// The labelling console on this process's terminal. stdin is read on a
/// thread of its own, started at the first `read`, and handed over a
/// channel: a blocking read can't be cancelled, a channel receive can.
pub struct TuiOperatorImpl {
    lines: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
}

impl TuiOperatorImpl {
    pub fn new() -> Self {
        Self {
            lines: Mutex::new(None),
        }
    }
}

impl Default for TuiOperatorImpl {
    fn default() -> Self {
        Self::new()
    }
}

/// A line of input: a command letter, Enter alone, or a scan.
pub fn parse_input(line: &str) -> OperatorInput {
    match line.trim() {
        "" => OperatorInput::Enter,
        "r" | "R" => OperatorInput::Reprint,
        "s" | "S" => OperatorInput::Skip,
        "f" | "F" => OperatorInput::Force,
        "q" | "Q" => OperatorInput::Quit,
        scanned => OperatorInput::Scan(scanned.to_string()),
    }
}

fn stdin_lines() -> mpsc::UnboundedReceiver<String> {
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

fn counters(counters: &Counters) -> String {
    tui::dim(format!(
        "searching {} · issued {} · queued {} · labelled {} · installed {} · failed {}",
        counters.searching,
        counters.issued,
        counters.queued,
        counters.labelled,
        counters.installed,
        counters.failed
    ))
}

fn name(hub: &HubSummary) -> String {
    tui::accent(hub.serial.as_deref().unwrap_or(&hub.temp_hostname))
}

fn details(hub: &HubSummary, attempt: Option<u32>) -> String {
    let mut lines = vec![
        format!("serial    {}", name(hub)),
        format!("board     {}", hub.board),
        format!("hostname  {}", hub.temp_hostname),
        format!("MAC       {}", hub.eth_mac.as_deref().unwrap_or("-")),
    ];
    if let Some(attempt) = attempt.filter(|attempt| *attempt > 1) {
        lines.push(format!("attempt   {attempt}"));
    }
    lines.join("\n")
}

fn tail(outcome: &HookOutcome) -> String {
    let output = format!("{}{}", outcome.stdout, outcome.stderr);
    let lines: Vec<&str> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(OUTPUT_LINES)..]
        .iter()
        .map(|line| format!("\n  {}", tui::dim(line)))
        .collect()
}

#[async_trait::async_trait]
impl OperatorAdapter for TuiOperatorImpl {
    fn show(&self, event: &OperatorEvent) {
        match event {
            OperatorEvent::Issued { hub } => {
                tui::success(format!(
                    "Issued {} {}",
                    name(hub),
                    tui::dim(format!("({}, {})", hub.board, hub.temp_hostname))
                ));
                // The serial alone on stdout, one per line: the output.
                if let Some(serial) = &hub.serial {
                    println!("{serial}");
                }
            }
            OperatorEvent::Active {
                hub,
                attempt,
                counters: totals,
            } => {
                tui::note(
                    "Label this hub: its LED is steady",
                    format!("{}\n{}", details(hub, Some(*attempt)), counters(totals)),
                );
            }
            OperatorEvent::Idle { counters: totals } => {
                tui::info(format!("No hub to label {}", counters(totals)))
            }
            OperatorEvent::Labelled {
                hub,
                counters: totals,
            } => tui::success(format!(
                "Labelled {}: its LED goes off {}",
                name(hub),
                counters(totals)
            )),
            OperatorEvent::Mismatch { expected, scanned } => tui::alert(format!(
                "Scanned {scanned}, but the hub to label is {expected}: nothing validated"
            )),
            OperatorEvent::NothingActive => tui::warning("No hub to label right now"),
            OperatorEvent::ScanRequired => {
                tui::warning("Scan the label stuck on the hub with the steady LED")
            }
            OperatorEvent::Blocked {
                hub,
                reason: LabelBlock::Printing,
            } => tui::warning(format!("The label of {} is still printing", name(hub))),
            OperatorEvent::Blocked {
                hub,
                reason: LabelBlock::PrintFailed,
            } => tui::alert(format!(
                "Printing the label of {} failed: r to reprint, f to validate anyway (the scan is still required)",
                name(hub)
            )),
            OperatorEvent::Forced { hub } => tui::warning(format!(
                "{} may be validated despite its failed label: scan it",
                name(hub)
            )),
            OperatorEvent::Skipped { hub } => {
                tui::info(format!("{} goes back to the end of the queue", name(hub)))
            }
            OperatorEvent::Lost { hub } => tui::warning(format!(
                "{} stopped polling: back in the queue until it does again",
                name(hub)
            )),
            OperatorEvent::Hook {
                event,
                hub,
                outcome,
            } => {
                let line = format!(
                    "{} for {}: {}{}",
                    outcome.hook,
                    name(hub),
                    match (outcome.exit, outcome.timed_out) {
                        (_, true) => "timed out".to_string(),
                        (Some(code), _) => format!("exit {code}"),
                        (None, _) => "did not run".to_string(),
                    },
                    tail(outcome)
                );
                if outcome.succeeded() {
                    tui::step(line)
                } else {
                    tui::warning(format!("{} hook failed: {line}", event.name()))
                }
            }
            OperatorEvent::Installed { hub } => {
                tui::success(format!("{} wrote its identity", name(hub)))
            }
            OperatorEvent::Failed { hub, reason } => {
                tui::alert(format!("{} failed: {reason}", name(hub)))
            }
            OperatorEvent::Rejected { hub, reason } => tui::warning(format!(
                "Refused a {} hub ({}): {reason}",
                hub.board, hub.temp_hostname
            )),
            OperatorEvent::Warning(message) => tui::warning(message),
        }
    }

    async fn read(&self) -> Result<Option<OperatorInput>> {
        let mut lines = self.lines.lock().await;
        let lines = lines.get_or_insert_with(stdin_lines);
        Ok(lines.recv().await.as_deref().map(parse_input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_a_command_letter_enter_or_a_scan() {
        assert_eq!(parse_input(""), OperatorInput::Enter);
        assert_eq!(parse_input("r"), OperatorInput::Reprint);
        assert_eq!(parse_input(" S "), OperatorInput::Skip);
        assert_eq!(parse_input("f"), OperatorInput::Force);
        assert_eq!(parse_input("q"), OperatorInput::Quit);
        assert_eq!(parse_input(" 1H7Z\r"), OperatorInput::Scan("1H7Z".into()));
    }
}
