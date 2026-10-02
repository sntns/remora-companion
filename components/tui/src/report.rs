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

    let mut lines: Vec<String> = Vec::new();
    for frame in report.frames() {
        let line = match frame.kind() {
            FrameKind::Context(context) => context.to_string(),
            FrameKind::Attachment(AttachmentKind::Printable(printable)) => printable.to_string(),
            FrameKind::Attachment(_) => continue,
        };
        // A context and its leaf error often say the same thing (an io
        // error wrapped once); repeating it adds nothing.
        if lines.last() != Some(&line) {
            lines.push(line);
        }
    }

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
