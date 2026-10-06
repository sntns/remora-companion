use clap::{parser::ValueSource, FromArgMatches};
use remora_completion::Candidate;
use remora_context::{
    application::ContextService,
    model::{ContextOverride, Selection},
};

/// Where the context override comes from in the environment: one variable
/// for every binary, since they share the contexts.
pub const CONTEXT_ENV: &str = "RMRA_CONTEXT";

/// The context to run against: a first-level option, given before the
/// command (`<bin> -c eu2 ssh …`), never after it -- it chooses where the
/// command runs, not how. Flatten it into the binary's top-level options,
/// without `global`.
#[derive(clap::Args, Debug)]
pub struct ContextArgs {
    /// The context to use for this command, overriding `context use`.
    #[arg(short = 'c', long = "context", env = CONTEXT_ENV, value_name = "CONTEXT")]
    #[arg(add = remora_completion::values(remora_completion::Kind::Context))]
    context: Option<String>,
}

impl ContextArgs {
    /// The override these arguments make, if any; `matches` (the top-level
    /// ones) tell a `--context` from the environment variable.
    pub fn over(&self, matches: &clap::ArgMatches) -> Option<ContextOverride> {
        let name = self.context.clone().filter(|name| !name.is_empty())?;
        let source = match matches.value_source("context") {
            Some(ValueSource::EnvVariable) => Selection::Environment,
            _ => Selection::Flag,
        };
        Some(ContextOverride { name, source })
    }
}

/// The context the line being completed runs against: its `--context`/
/// `-c`, else the environment variable, else none (the current context).
/// `command` is the binary's own: the line is parsed the way it will be
/// run, so only a first-level `-c` counts -- in `scp -c aes128-ctr …` it is
/// scp's cipher, not a context.
pub fn context_on_command_line(command: clap::Command) -> Option<ContextOverride> {
    context_in(command, remora_completion::command_line())
}

fn context_in(
    command: clap::Command,
    line: impl IntoIterator<Item = std::ffi::OsString>,
) -> Option<ContextOverride> {
    // A line being completed is unfinished by nature: parse what's there.
    let parsed = command
        .ignore_errors(true)
        .try_get_matches_from(line)
        .ok()
        .and_then(|matches| Some((ContextArgs::from_arg_matches(&matches).ok()?, matches)));
    match parsed {
        Some((args, matches)) => args.over(&matches),
        None => std::env::var(CONTEXT_ENV)
            .ok()
            .filter(|name| !name.is_empty())
            .map(|name| ContextOverride {
                name,
                source: Selection::Environment,
            }),
    }
}

/// Completion values: the contexts (local, instant).
pub async fn complete_contexts(service: &ContextService) -> Vec<Candidate> {
    service
        .list()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|summary| {
            let mut help = summary.context.endpoint.address.clone();
            if summary.current {
                help.push_str(" (current)");
            }
            Candidate::new(summary.context.name).help(help)
        })
        .collect()
}

/// Completion values: the selected context's role aliases.
pub async fn complete_roles(
    service: &ContextService,
    over: Option<&ContextOverride>,
) -> Vec<Candidate> {
    let Ok((_, roles, _)) = service.roles(over).await else {
        return Vec::new();
    };
    roles
        .into_iter()
        .map(|role| {
            let mut help = role.urn;
            if role.assumed {
                help.push_str(" (assumed)");
            }
            Candidate::new(role.alias).help(help)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::*;

    /// A binary's shape: the context first-level, a command taking its
    /// own options through.
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        context: ContextArgs,
        #[arg(short, long)]
        verbose: bool,
        #[command(subcommand)]
        command: Commands,
    }

    #[derive(clap::Subcommand)]
    enum Commands {
        Scp {
            #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
            arguments: Vec<String>,
        },
    }

    fn context(line: &str) -> Option<String> {
        let words = std::iter::once("bin")
            .chain(line.split(' '))
            .map(std::ffi::OsString::from);
        context_in(Cli::command(), words).map(|over| over.name)
    }

    #[test]
    fn only_a_first_level_context_counts() {
        assert_eq!(context("-c eu2 scp ./a ").as_deref(), Some("eu2"));
        assert_eq!(context("-v --context=eu2 scp ").as_deref(), Some("eu2"));
        assert_eq!(context("-ceu2 scp -c aes128-ctr ").as_deref(), Some("eu2"));
        if std::env::var_os(CONTEXT_ENV).is_none() {
            assert_eq!(context("scp -c aes128-ctr "), None);
            assert_eq!(context("-c "), None);
        }
    }
}
