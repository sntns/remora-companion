use clap::parser::ValueSource;
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

/// `--context`/`-c` as typed on the line being completed (the shell hands
/// the whole line to the binary), else the environment variable, else
/// none: the current context.
pub fn context_on_command_line() -> Option<ContextOverride> {
    let args: Vec<String> = std::env::args().collect();
    let mut found = None;
    for (index, arg) in args.iter().enumerate() {
        if let Some(value) = arg.strip_prefix("--context=") {
            found = Some(value.to_owned());
        } else if arg == "--context" || arg == "-c" {
            found = args.get(index + 1).cloned();
        } else if let Some(value) = arg.strip_prefix("-c").filter(|v| !v.is_empty()) {
            found = Some(value.to_owned());
        }
    }
    match found.filter(|name| !name.is_empty()) {
        Some(name) => Some(ContextOverride {
            name,
            source: Selection::Flag,
        }),
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
    let Ok((_, roles)) = service.roles(over).await else {
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
