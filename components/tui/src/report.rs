use error_stack::{AttachmentKind, FrameKind, Report};

/// Prints an error for a human on stderr: the outermost message, then each
/// cause and attachment below it, outermost first -- what the platform or
/// the device actually said is usually a few hops down. `verbose` prints
/// error-stack's full debug rendering instead (with file:line per hop).
pub fn render_report<C>(report: &Report<C>, verbose: bool) {
    if verbose {
        eprintln!("{report:?}");
        return;
    }

    let lines = distinct_lines(report);
    print_lines(&lines);
}

/// An error on one plain line, causes joined by `: ` -- for output that
/// can't be decorated or span lines, like a ssh ProxyCommand's (`verbose`
/// still gives the full chain, on as many lines as it takes).
pub fn one_line<C>(report: &Report<C>, verbose: bool) -> String {
    if verbose {
        return format!("{report:?}").replace('\n', "\r\n");
    }
    distinct_lines(report).join(": ")
}

/// Each context once, outermost first, each followed by its own printable
/// attachments (error-stack lists an attachment above the context it was
/// attached to, which would otherwise print a detail before its cause).
fn distinct_lines<C>(report: &Report<C>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let push = |lines: &mut Vec<String>, line: String| {
        // Layers often say the same thing (a context and the leaf it
        // wraps, two layers both "failed"); repeating it adds nothing.
        if !lines.contains(&line) {
            lines.push(line);
        }
    };
    for frame in report.frames() {
        match frame.kind() {
            FrameKind::Context(context) => {
                push(&mut lines, context.to_string());
                for attachment in pending.drain(..) {
                    push(&mut lines, attachment);
                }
            }
            FrameKind::Attachment(AttachmentKind::Printable(printable)) => {
                pending.push(printable.to_string())
            }
            FrameKind::Attachment(_) => {}
        }
    }
    for attachment in pending {
        push(&mut lines, attachment);
    }
    lines
}

fn print_lines(lines: &[String]) {
    let Some((headline, causes)) = lines.split_first() else {
        return;
    };
    let mut body = String::new();
    for cause in causes {
        body.push_str(&format!("{} {cause}\n", console::style("↳").dim()));
    }
    if crate::decorated() {
        let message = if body.is_empty() {
            console::style(headline).red().bold().to_string()
        } else {
            format!(
                "{}\n{}",
                console::style(headline).red().bold(),
                body.trim_end()
            )
        };
        let _ = cliclack::log::error(message);
    } else {
        eprintln!("error: {headline}");
        eprint!("{body}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("{0}")]
    struct Message(&'static str);

    #[test]
    fn one_line_joins_each_cause_once() {
        let report = Report::new(Message("the channel failed"))
            .attach("Unavailable: device disconnected")
            .change_context(Message("the connection to DEV was lost"))
            .change_context(Message("the channel failed"));
        assert_eq!(
            one_line(&report, false),
            "the channel failed: the connection to DEV was lost: Unavailable: device disconnected"
        );
    }
}
