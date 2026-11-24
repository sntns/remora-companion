use clap::{parser::ValueSource, FromArgMatches};
use error_stack::Report;
use remora_completion::Candidate;
use remora_context::{
    application::ContextService,
    model::{
        is_role_urn, ContextKind, ContextOverride, Credentials, DefinedContext, Endpoint, Secret,
        Selection, Tls,
    },
};

use crate::{
    error::{Error, Result},
    service::DEFAULT_ADDRESS,
};

/// Where the context override comes from in the environment: one variable
/// for every binary, since they share the contexts.
pub const CONTEXT_ENV: &str = "RMRA_CONTEXT";

/// A context defined whole by the environment, for an ephemeral container
/// or a CI job: an access key's token, required, and the gateway, by
/// default `DEFAULT_ADDRESS` (api.eu2.sntns.io)...
pub const ADDRESS_ENV: &str = "RMRA_ADDRESS";
pub const TOKEN_ENV: &str = "RMRA_TOKEN";
/// ...and, optionally, the role it acts as (a URN), plaintext for a local
/// development stack and the name to verify the gateway's certificate
/// against -- `context create`'s options.
pub const ASSUME_ROLE_ENV: &str = "RMRA_ASSUME_ROLE";
pub const PLAINTEXT_ENV: &str = "RMRA_PLAINTEXT";
pub const SERVER_NAME_ENV: &str = "RMRA_SERVER_NAME";

/// The name a context defined by the environment goes by: shown only, it
/// is never stored, so never collides with a stored "env".
pub const DEFINED_NAME: &str = "env";

/// The context to run against: a first-level option, given before the
/// command (`<bin> -c eu2 ssh …`), never after it -- it chooses where the
/// command runs, not how. Flatten it into the binary's top-level options,
/// without `global`.
#[derive(clap::Args, Debug)]
pub struct ContextArgs {
    /// The context to use for this command, overriding `context use` and a
    /// context defined by RMRA_TOKEN.
    #[arg(short = 'c', long = "context", env = CONTEXT_ENV, value_name = "CONTEXT")]
    #[arg(add = remora_completion::values(remora_completion::Kind::Context))]
    context: Option<String>,
}

impl ContextArgs {
    /// The override these arguments and the environment make, if any:
    /// `--context`, else a context the environment defines, else
    /// `RMRA_CONTEXT`. `matches` (the top-level ones) tell a `--context`
    /// from the environment variable. A usage error when the environment
    /// defines a context partly, or both defines one and names one.
    pub fn over(&self, matches: &clap::ArgMatches) -> Result<Option<ContextOverride>> {
        let name = self.context.clone().filter(|name| !name.is_empty());
        match (name, matches.value_source("context")) {
            (Some(name), Some(ValueSource::EnvVariable)) => choose(Some(name), env_var),
            (Some(name), _) => Ok(Some(ContextOverride::named(name, Selection::Flag))),
            (None, _) => choose(None, env_var),
        }
    }
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Between `RMRA_CONTEXT` (`named`) and a context `vars` define: never
/// both, which would leave which one a command acts as to a precedence
/// rule nobody reads -- a factory provisioning as the wrong account.
fn choose(
    named: Option<String>,
    vars: impl Fn(&str) -> Option<String>,
) -> Result<Option<ContextOverride>> {
    match (named, defined_by(vars)?) {
        (Some(_), Some(_)) => Err(Report::new(Error::Usage(format!(
            "{CONTEXT_ENV} names a context while {ADDRESS_ENV} and {TOKEN_ENV} define one: \
             unset one or the other"
        )))),
        (Some(name), None) => Ok(Some(ContextOverride::named(name, Selection::Environment))),
        (None, Some(defined)) => Ok(Some(ContextOverride::defined(defined))),
        (None, None) => Ok(None),
    }
}

/// The context `vars` define, if they do. Empty variables count as unset.
fn defined_by(vars: impl Fn(&str) -> Option<String>) -> Result<Option<DefinedContext>> {
    let Some(token) = vars(TOKEN_ENV) else {
        let rest = [ADDRESS_ENV, ASSUME_ROLE_ENV, PLAINTEXT_ENV, SERVER_NAME_ENV];
        return match rest.into_iter().find(|name| vars(name).is_some()) {
            Some(name) => Err(usage(format!(
                "{name} is set without {TOKEN_ENV}, which defines the context"
            ))),
            None => Ok(None),
        };
    };
    let address = vars(ADDRESS_ENV).unwrap_or_else(|| DEFAULT_ADDRESS.to_owned());
    let role = vars(ASSUME_ROLE_ENV);
    if let Some(role) = role.as_deref().filter(|role| !is_role_urn(role)) {
        return Err(usage(format!(
            "{ASSUME_ROLE_ENV} must be a role URN (urn:…:role:…), not {role:?}"
        )));
    }
    let disabled = match vars(PLAINTEXT_ENV).map(|value| value.to_ascii_lowercase()) {
        None => false,
        Some(value) if ["1", "true", "yes", "on"].contains(&value.as_str()) => true,
        Some(value) if ["0", "false", "no", "off"].contains(&value.as_str()) => false,
        Some(value) => {
            return Err(usage(format!(
                "{PLAINTEXT_ENV} must be true or false, not {value:?}"
            )))
        }
    };
    Ok(Some(DefinedContext {
        name: DEFINED_NAME.to_owned(),
        endpoint: Endpoint {
            address,
            tls: Tls {
                disabled,
                server_name: vars(SERVER_NAME_ENV),
            },
        },
        credentials: Credentials {
            secret: Secret::AccessKey { token },
        },
        role,
    }))
}

fn usage(message: String) -> Report<Error> {
    Report::new(Error::Usage(message))
}

/// The context the line being completed runs against: its `--context`/
/// `-c`, else the environment's, else none (the current context).
/// `command` is the binary's own: the line is parsed the way it will be
/// run, so only a first-level `-c` counts -- in `scp -c aes128-ctr …` it is
/// scp's cipher, not a context. A misconfigured environment completes as
/// none: the command itself will say what is wrong.
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
        Some((args, matches)) => args.over(&matches).ok().flatten(),
        None => choose(env_var(CONTEXT_ENV), env_var).ok().flatten(),
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
            let mut help = match &summary.context.kind {
                ContextKind::Login { endpoint } => endpoint.address.clone(),
                ContextKind::Derived(derivation) => format!(
                    "{} as {}",
                    derivation.from,
                    remora_context::model::short_role_name(&derivation.role)
                ),
            };
            if summary.current {
                help.push_str(" (current)");
            }
            Candidate::new(summary.context.name).help(help)
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

    /// An environment of `pairs` only.
    fn vars<'a>(pairs: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<String> + 'a {
        |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
                .filter(|value| !value.is_empty())
        }
    }

    fn usage_of(result: Result<Option<ContextOverride>>) -> String {
        match result.map(|over| over.map(|over| over.name)) {
            Err(report) => match report.current_context() {
                Error::Usage(message) => message.clone(),
                other => panic!("not a usage error: {other}"),
            },
            Ok(over) => panic!("accepted: {over:?}"),
        }
    }

    #[test]
    fn the_environment_defines_a_whole_context() {
        let role = "urn:sntns:iam:eu2:acme:role:factory";
        let env = [
            (ADDRESS_ENV, "gateway:50051"),
            (TOKEN_ENV, "s3cret"),
            (ASSUME_ROLE_ENV, role),
            (PLAINTEXT_ENV, "True"),
            (SERVER_NAME_ENV, "api.example"),
        ];
        let over = choose(None, vars(&env)).unwrap().unwrap();
        assert_eq!(
            (over.name.as_str(), over.source),
            (DEFINED_NAME, Selection::Defined)
        );
        let defined = over.defined.unwrap();
        let endpoint = &defined.endpoint;
        assert_eq!(endpoint.address, "gateway:50051");
        assert_eq!(defined.role.as_deref(), Some(role));
        assert!(endpoint.tls.disabled);
        assert_eq!(endpoint.tls.server_name.as_deref(), Some("api.example"));
        assert!(
            matches!(&defined.credentials.secret, Secret::AccessKey { token } if token == "s3cret")
        );
        // Not even a debug line shows the token.
        assert!(!format!("{defined:?}").contains("s3cret"));

        // RMRA_TOKEN alone: the default gateway.
        let token_only = choose(None, vars(&[(TOKEN_ENV, "s3cret")]))
            .unwrap()
            .unwrap();
        assert_eq!(
            token_only.defined.unwrap().endpoint.address,
            DEFAULT_ADDRESS
        );

        // Nothing set: no override; RMRA_CONTEXT alone names a stored one.
        assert!(choose(None, vars(&[])).unwrap().is_none());
        let named = choose(Some("eu2".into()), vars(&[])).unwrap().unwrap();
        assert_eq!(
            (named.source, named.defined.is_none()),
            (Selection::Environment, true)
        );
    }

    #[test]
    fn a_partial_or_contradictory_environment_is_a_usage_error() {
        let both = [(ADDRESS_ENV, "gateway:50051"), (TOKEN_ENV, "t")];
        assert!(usage_of(choose(None, vars(&both[..1]))).contains("without RMRA_TOKEN"));
        assert!(usage_of(choose(None, vars(&[(PLAINTEXT_ENV, "1")]))).contains("RMRA_PLAINTEXT"));
        assert!(usage_of(choose(Some("eu2".into()), vars(&both))).contains("RMRA_CONTEXT"));

        let with = |name, value| {
            let mut env = both.to_vec();
            env.push((name, value));
            usage_of(choose(None, vars(&env)))
        };
        assert!(with(ASSUME_ROLE_ENV, "ops").contains("role URN"));
        assert!(with(PLAINTEXT_ENV, "maybe").contains("true or false"));
    }
}
