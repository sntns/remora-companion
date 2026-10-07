use clap::{parser::ValueSource, FromArgMatches};
use error_stack::{Report, ResultExt};
use remora_completion::Candidate;
use remora_context::{
    application::ContextService,
    model::{
        is_role_urn, Context, ContextOverride, Credentials, DefinedContext, Endpoint, Secret,
        Selection, Tls,
    },
};

use crate::error::{Error, Result};

/// Where the context override comes from in the environment: one variable
/// for every binary, since they share the contexts.
pub const CONTEXT_ENV: &str = "RMRA_CONTEXT";

/// A context defined whole by the environment, for an ephemeral container
/// or a CI job: the gateway and an access key's token, both required...
pub const ADDRESS_ENV: &str = "RMRA_ADDRESS";
pub const TOKEN_ENV: &str = "RMRA_TOKEN";
/// ...and, optionally, the role it acts as (a URN), plaintext for a local
/// development stack, a PEM certificate authority file and the name to
/// verify the gateway's certificate against -- `context create`'s options.
pub const ASSUME_ROLE_ENV: &str = "RMRA_ASSUME_ROLE";
pub const PLAINTEXT_ENV: &str = "RMRA_PLAINTEXT";
pub const CA_FILE_ENV: &str = "RMRA_CA_FILE";
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
    /// context defined by RMRA_ADDRESS and RMRA_TOKEN.
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
    let (address, token) = match (vars(ADDRESS_ENV), vars(TOKEN_ENV)) {
        (Some(address), Some(token)) => (address, token),
        (None, None) => {
            let rest = [ASSUME_ROLE_ENV, PLAINTEXT_ENV, CA_FILE_ENV, SERVER_NAME_ENV];
            return match rest.into_iter().find(|name| vars(name).is_some()) {
                Some(name) => Err(usage(format!(
                    "{name} is set without {ADDRESS_ENV} and {TOKEN_ENV}, which define the context"
                ))),
                None => Ok(None),
            };
        }
        (Some(_), None) => {
            return Err(usage(format!(
                "{ADDRESS_ENV} is set without {TOKEN_ENV}: set both"
            )))
        }
        (None, Some(_)) => {
            return Err(usage(format!(
                "{TOKEN_ENV} is set without {ADDRESS_ENV}: set both"
            )))
        }
    };
    // Its role can only be a URN: an alias needs a stored context's roles.
    let assumed_role = vars(ASSUME_ROLE_ENV);
    if let Some(role) = assumed_role.as_deref().filter(|role| !is_role_urn(role)) {
        return Err(usage(format!(
            "{ASSUME_ROLE_ENV} must be a role URN (urn:…:role:…), not {role:?}: \
             a context defined by the environment has no role aliases"
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
    // Stored contexts keep a CA's PEM itself, so does this one.
    let mut authorities = Vec::new();
    if let Some(path) = vars(CA_FILE_ENV).map(std::path::PathBuf::from) {
        authorities.push(std::fs::read_to_string(&path).change_context(Error::ReadFile(path))?);
    }
    Ok(Some(DefinedContext {
        context: Context {
            name: DEFINED_NAME.to_owned(),
            description: None,
            endpoint: Endpoint {
                address,
                tls: Tls {
                    disabled,
                    authorities,
                    server_name: vars(SERVER_NAME_ENV),
                },
            },
            roles: Default::default(),
            assumed_role,
            login: None,
        },
        credentials: Credentials {
            secret: Secret::AccessKey { token },
        },
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
        let ca = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(ca.path(), "-----BEGIN CERTIFICATE-----\n").unwrap();
        let path = ca.path().to_str().unwrap();
        let role = "urn:sntns:iam:eu2:acme:role:factory";
        let env = [
            (ADDRESS_ENV, "gateway:50051"),
            (TOKEN_ENV, "s3cret"),
            (ASSUME_ROLE_ENV, role),
            (PLAINTEXT_ENV, "True"),
            (CA_FILE_ENV, path),
            (SERVER_NAME_ENV, "api.example"),
        ];
        let over = choose(None, vars(&env)).unwrap().unwrap();
        assert_eq!(
            (over.name.as_str(), over.source),
            (DEFINED_NAME, Selection::Defined)
        );
        let defined = over.defined.unwrap();
        let context = &defined.context;
        assert_eq!(context.endpoint.address, "gateway:50051");
        assert_eq!(context.assumed_role.as_deref(), Some(role));
        assert!(context.endpoint.tls.disabled);
        assert_eq!(
            context.endpoint.tls.authorities,
            ["-----BEGIN CERTIFICATE-----\n"]
        );
        assert_eq!(
            context.endpoint.tls.server_name.as_deref(),
            Some("api.example")
        );
        assert!(
            matches!(&defined.credentials.secret, Secret::AccessKey { token } if token == "s3cret")
        );
        // Not even a debug line shows the token.
        assert!(!format!("{defined:?}").contains("s3cret"));

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
        assert!(usage_of(choose(None, vars(&both[1..]))).contains("without RMRA_ADDRESS"));
        assert!(usage_of(choose(None, vars(&[(PLAINTEXT_ENV, "1")]))).contains("RMRA_PLAINTEXT"));
        assert!(usage_of(choose(Some("eu2".into()), vars(&both))).contains("RMRA_CONTEXT"));

        let with = |name, value| {
            let mut env = both.to_vec();
            env.push((name, value));
            usage_of(choose(None, vars(&env)))
        };
        assert!(with(ASSUME_ROLE_ENV, "ops").contains("no role aliases"));
        assert!(with(PLAINTEXT_ENV, "maybe").contains("true or false"));
        let missing = choose(
            None,
            vars(&[
                (ADDRESS_ENV, "a:1"),
                (TOKEN_ENV, "t"),
                (CA_FILE_ENV, "/nonexistent/ca.pem"),
            ]),
        );
        assert!(matches!(
            missing.unwrap_err().current_context(),
            Error::ReadFile(_)
        ));
    }
}
