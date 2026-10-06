//! The labelling console's input: this terminal's stdin, a line at a time
//! -- a barcode scanner types the label then Enter, and a command is its
//! letter then Enter.

use std::io::BufRead;

use remora_station::adapter::operator::OperatorInput;
use tokio::sync::mpsc;

/// One line the operator typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line {
    /// `q`: stop the station.
    Quit,
    /// Anything else is for the station: a command letter, Enter alone, or
    /// a scan.
    Input(OperatorInput),
}

pub(crate) fn parse(line: &str) -> Line {
    Line::Input(match line.trim() {
        "q" | "Q" => return Line::Quit,
        "" => OperatorInput::Enter,
        "r" | "R" => OperatorInput::Reprint,
        "s" | "S" => OperatorInput::Skip,
        "f" | "F" => OperatorInput::Force,
        scanned => OperatorInput::Scan(scanned.to_string()),
    })
}

/// stdin's lines, read on a thread of their own and handed over a channel:
/// a blocking read can't be cancelled, a channel receive can (the console
/// waits on it alongside its clock and Ctrl-C). Closed with stdin.
pub(crate) fn lines() -> mpsc::UnboundedReceiver<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_a_command_letter_enter_or_a_scan() {
        assert_eq!(parse(""), Line::Input(OperatorInput::Enter));
        assert_eq!(parse("r"), Line::Input(OperatorInput::Reprint));
        assert_eq!(parse(" S "), Line::Input(OperatorInput::Skip));
        assert_eq!(parse("f"), Line::Input(OperatorInput::Force));
        assert_eq!(parse("q"), Line::Quit);
        assert_eq!(
            parse(" 1H7Z\r"),
            Line::Input(OperatorInput::Scan("1H7Z".into()))
        );
    }
}
