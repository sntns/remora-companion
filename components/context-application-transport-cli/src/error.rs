use error_stack::Report;
use remora_context::application::Error as ContextError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Context(String),
    #[error("cancelled")]
    Cancelled,
    #[error("failed to read the terminal")]
    Prompt,
    #[error("failed to read the secret from stdin")]
    Stdin,
    #[error("failed to read {0}")]
    ReadFile(std::path::PathBuf),
    #[error("{0}")]
    Usage(String),
}

pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;

/// A prompt's io error: Ctrl-C/Esc in cliclack is `Interrupted`, which is
/// the operator changing their mind, not a failure worth a stack.
pub(crate) fn prompt_error(error: std::io::Error) -> error_stack::Report<Error> {
    if error.kind() == std::io::ErrorKind::Interrupted {
        error_stack::Report::new(Error::Cancelled)
    } else {
        error_stack::Report::new(error).change_context(Error::Prompt)
    }
}

/// Lifts a context-service failure into this transport's error, keeping the
/// service's message as the headline (it is already the useful one, e.g.
/// "not logged in to context \"eu2\""). What to run about it is [`hint`]'s.
pub(crate) fn context_error(report: Report<ContextError>) -> Report<Error> {
    let message = report.current_context().to_string();
    report.change_context(Error::Context(message))
}

/// What to run about a context failure anywhere in `report`'s chain -- a
/// context command's own, or one another command met resolving its context
/// (`ssh` while logged out) -- naming `program`'s commands: the domain says
/// what is wrong, binary-neutral, and only the binary knows its own name.
pub fn hint<C>(report: &Report<C>, program: &str) -> Option<String> {
    Some(match report.downcast_ref::<ContextError>()? {
        ContextError::NoContext => format!(
            "create one with `{program} context create <name>`, or log in with `{program} login`"
        ),
        ContextError::Ambiguous => format!(
            "pick one with `{program} context use <name>`, or `{program} --context <name> …`"
        ),
        ContextError::NotLoggedIn(name) => {
            format!("log in with `{program} --context {name} login`")
        }
        ContextError::UnknownRole(alias) => format!(
            "add it with `{program} context role add {alias} <role-urn>`, or give the role's URN"
        ),
        _ => return None,
    })
}

/// `report` with its [`hint`] attached, which `remora_tui::render_report`
/// prints right below the headline. For a binary's top level, so every
/// command's failure gets it.
pub fn with_hint<C>(report: Report<C>, program: &str) -> Report<C> {
    match hint(&report, program) {
        Some(hint) => report.attach(hint),
        None => report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("failed to open the channel")]
    struct Elsewhere;

    #[test]
    fn hints_name_the_running_binary_wherever_the_failure_is() {
        let report = context_error(Report::new(ContextError::NotLoggedIn("eu2".into())));
        assert_eq!(
            hint(&report, "remora-etcher").as_deref(),
            Some("log in with `remora-etcher --context eu2 login`")
        );
        // Met by another vertical's command, a few hops down.
        let report = report.change_context(Elsewhere);
        let report = with_hint(report, "rmra");
        let printed = format!("{report:?}");
        assert!(printed.contains("`rmra --context eu2 login`"), "{printed}");

        let report = Report::new(ContextError::Verify);
        assert_eq!(hint(&report, "rmra"), None);
    }
}
