use remora_station::adapter::{
    hooks::HookOutcome,
    operator::{Counters, HubSummary, LabelBlock, OperatorAdapter, OperatorEvent},
};
use remora_tui as tui;

/// How many trailing lines of a hook's output the dashboard shows (the
/// journal has all of it).
const OUTPUT_LINES: usize = 3;

/// The labelling console's display on this process's terminal.
#[derive(Debug, Default, Clone, Copy)]
pub struct TuiOperatorImpl;

/// `text` without its control characters. The station checks what devices
/// report, but this is the last stop before the terminal: an escape
/// sequence from anywhere (a hook's output, a scan) could clear the
/// dashboard or forge the line saying which hub to label.
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
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
    tui::accent(clean(hub.serial.as_deref().unwrap_or(&hub.temp_hostname)))
}

fn details(hub: &HubSummary, attempt: Option<u32>) -> String {
    let mut lines = vec![
        format!("serial    {}", name(hub)),
        format!("board     {}", clean(&hub.board)),
        format!("hostname  {}", clean(&hub.temp_hostname)),
        format!("MAC       {}", clean(hub.eth_mac.as_deref().unwrap_or("-"))),
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
        .map(|line| format!("\n  {}", tui::dim(clean(line))))
        .collect()
}

impl OperatorAdapter for TuiOperatorImpl {
    fn show(&self, event: &OperatorEvent) {
        match event {
            OperatorEvent::Issued { hub } => {
                tui::success(format!(
                    "Issued {} {}",
                    name(hub),
                    tui::dim(format!("({}, {})", clean(&hub.board), clean(&hub.temp_hostname)))
                ));
                // The serial alone on stdout, one per line: the output.
                if let Some(serial) = &hub.serial {
                    println!("{}", clean(serial));
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
                "Scanned {}, but the hub to label is {}: nothing validated",
                clean(scanned),
                clean(expected)
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
                    clean(&outcome.hook),
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
                tui::alert(format!("{} failed: {}", name(hub), clean(reason)))
            }
            OperatorEvent::Rejected { hub, reason } => tui::warning(format!(
                "Refused a {} hub ({}): {}",
                clean(&hub.board),
                clean(&hub.temp_hostname),
                clean(reason)
            )),
            OperatorEvent::Warning(message) => tui::warning(clean(message)),
            OperatorEvent::Alert(message) => tui::alert(clean(message)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_what_could_redraw_the_terminal() {
        assert_eq!(clean("\x1b[2J\x1b[1;1H1H7Z\r\n"), "[2J[1;1H1H7Z");
        assert_eq!(clean("1H7Z"), "1H7Z");
    }
}
