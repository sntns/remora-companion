use error_stack::Report;
use remora_context::{
    application::{ContextService, Error as ContextError},
    model::{
        describe_role, short_role_name, Context, ContextKind, ContextOverride, ContextSummary,
        ContextUpdate, DefinedContext, Endpoint, Selection, Tls,
    },
};
use remora_tui as tui;
use serde::Serialize;

use crate::{
    error::{context_error, prompt_error, Error, Result},
    selection::TOKEN_ENV,
};

/// The production gateway a fresh `login` suggests.
pub const DEFAULT_ADDRESS: &str = "api.eu2.sntns.io";

#[derive(clap::Subcommand)]
pub enum Command {
    /// List contexts, each login followed by the contexts derived from it;
    /// the current one is marked with `*`.
    #[command(visible_alias = "ls")]
    List {
        /// `table` for humans, `json` for scripts.
        #[arg(long, default_value = "table")]
        format: Format,
        /// Only print context names.
        #[arg(short, long)]
        quiet: bool,
    },

    /// Create a context: a login to a gateway, which acts as itself, or,
    /// with --from, a login's role.
    #[command(after_help = "Examples:\n  \
        context create eu2 --use, then login\n  \
        context create acme --from eu2 --role urn:sntns:iam:eu2:<tenant>:role:ops")]
    Create {
        name: String,
        /// Derive it from a logged-in context: that context's login (its
        /// own, or the one it derives from) assuming --role -- e.g. one
        /// context per tenant from one login. Nothing is copied: logging
        /// the login in or out does it for both.
        #[arg(long, value_name = "CONTEXT", requires = "role")]
        #[arg(conflicts_with_all = ["address", "plaintext", "server_name"])]
        #[arg(add = remora_completion::values(remora_completion::Kind::Context))]
        from: Option<String>,
        /// With --from: the IAM role its calls assume, a URN, possibly of
        /// another tenant. Verified with the platform now.
        #[arg(long, visible_alias = "assume-role", value_name = "ROLE-URN")]
        #[arg(requires = "from")]
        role: Option<String>,
        /// The gateway, `host[:port]` or a URL (default: api.eu2.sntns.io).
        #[arg(long)]
        address: Option<String>,
        #[arg(long)]
        description: Option<String>,
        /// Talk plain HTTP (local development stacks only).
        #[arg(long)]
        plaintext: bool,
        /// Verify the gateway's certificate against this name instead of
        /// the address's host.
        #[arg(long)]
        server_name: Option<String>,
        /// Replace an existing context of that name (a login keeps its
        /// credentials and the contexts derived from it).
        #[arg(long)]
        force: bool,
        /// Also make it the current context.
        #[arg(long = "use")]
        make_current: bool,
    },

    /// Change a context's settings (default: the selected context): a
    /// login's gateway, a derived context's role.
    Update {
        #[arg(add = remora_completion::values(remora_completion::Kind::Context))]
        name: Option<String>,
        /// A new description; an empty one removes it.
        #[arg(long)]
        description: Option<String>,
        /// A derived context's new role, a URN: verified with the platform.
        #[arg(long, visible_alias = "assume-role", value_name = "ROLE-URN")]
        role: Option<String>,
        /// A login's new gateway, `host[:port]` or a URL.
        #[arg(long)]
        address: Option<String>,
        /// A login's plain HTTP (local development stacks only), on or off.
        #[arg(long, value_name = "BOOL")]
        plaintext: Option<bool>,
        /// The name to verify a login's gateway certificate against; an
        /// empty one: the address's host.
        #[arg(long)]
        server_name: Option<String>,
    },

    /// Show contexts' settings as JSON (default: the selected context).
    Inspect {
        #[arg(add = remora_completion::values(remora_completion::Kind::Context))]
        names: Vec<String>,
    },

    /// Make a context the current one.
    Use {
        #[arg(add = remora_completion::values(remora_completion::Kind::Context))]
        name: String,
    },

    /// Remove contexts and their stored credentials.
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(required = true)]
        #[arg(add = remora_completion::values(remora_completion::Kind::Context))]
        names: Vec<String>,
        /// Don't ask for confirmation, and remove a login's derived
        /// contexts with it.
        #[arg(short, long)]
        force: bool,
    },

    /// Rename a context; its login, the contexts derived from it, and its
    /// being current follow.
    #[command(visible_alias = "mv")]
    Rename {
        #[arg(add = remora_completion::values(remora_completion::Kind::Context))]
        from: String,
        to: String,
    },

    /// Print the selected context's name.
    Show,

    /// Bring contexts stored by a previous version to the current format.
    /// Done on its own on first use and by the installers; a backup of the
    /// previous files is kept.
    Migrate {
        /// Only say what would change.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    Table,
    Json,
}

/// Runs a `context` subcommand. `program` is the running binary's name,
/// for the commands its messages suggest: several binaries mount these
/// commands, on the same contexts.
pub async fn run(
    command: Command,
    service: &ContextService,
    over: Option<&ContextOverride>,
    program: &str,
) -> Result<()> {
    // Changes to stored contexts wouldn't apply while the environment
    // defines the context: refused, unless --context names one.
    if matches!(
        command,
        Command::Create { .. }
            | Command::Update { name: None, .. }
            | Command::Use { .. }
            | Command::Remove { .. }
            | Command::Rename { .. }
    ) {
        refuse_defined(over)?;
    }
    match command {
        Command::List { format, quiet } => list(service, over, format, quiet, program).await,
        Command::Create {
            name,
            from: Some(source),
            role: Some(role),
            description,
            force,
            make_current,
            ..
        } => {
            let spinner = tui::Spinner::start(format!(
                "Assuming {} with the login of {}",
                tui::accent(short_role_name(&role)),
                tui::accent(&source)
            ));
            let (principal, role) = match service
                .derive(&source, &name, description, &role, force)
                .await
            {
                Ok(done) => done,
                Err(report) => {
                    spinner.fail(format!("Could not create {}", tui::accent(&name)));
                    return Err(context_error(report));
                }
            };
            spinner.done(format!(
                "Created context {} {}",
                tui::accent(&name),
                tui::dim(format!(
                    "· {} acting as {}",
                    describe_user(&principal),
                    role.display_name()
                ))
            ));
            if role.account.is_none() {
                tui::warning("This role may not read its account: assumed all the same.");
            }
            if make_current {
                service.use_context(&name).await.map_err(context_error)?;
                tui::info(format!("Now using {}", tui::accent(&name)));
            }
            Ok(())
        }
        Command::Create { from: Some(_), .. } | Command::Create { role: Some(_), .. } => {
            unreachable!("clap requires --from and --role together")
        }
        Command::Create {
            name,
            address,
            description,
            plaintext,
            server_name,
            force,
            make_current,
            ..
        } => {
            let endpoint = Endpoint {
                address: address.unwrap_or_else(|| DEFAULT_ADDRESS.to_owned()),
                tls: Tls {
                    disabled: plaintext,
                    server_name,
                },
            };
            let address = endpoint.address.clone();
            service
                .create(&name, description, endpoint, force)
                .await
                .map_err(context_error)?;
            if make_current {
                service.use_context(&name).await.map_err(context_error)?;
            }
            tui::success(format!(
                "Created context {} {}",
                tui::accent(&name),
                tui::dim(format!("→ {address}"))
            ));
            if make_current {
                tui::info(format!("Now using {}", tui::accent(&name)));
            }
            let logged_in = service
                .inspect(&name)
                .await
                .map_err(context_error)?
                .credentials
                .is_some();
            if !logged_in {
                tui::step(format!(
                    "Log in with {}",
                    tui::accent(login_hint(program, &name, make_current))
                ));
            }
            Ok(())
        }
        Command::Update {
            name,
            description,
            role,
            address,
            plaintext,
            server_name,
        } => {
            let name = match name {
                Some(name) => name,
                None => selected_name(service, over).await?,
            };
            let update = ContextUpdate {
                description,
                address,
                plaintext,
                server_name,
                role,
            };
            if update == ContextUpdate::default() {
                return Err(Report::new(Error::Usage(
                    "nothing to change: pass a setting to update (see --help)".into(),
                )));
            }
            let spinner = update.role.as_ref().map(|role| {
                tui::Spinner::start(format!("Assuming {}", tui::accent(short_role_name(role))))
            });
            match service.update(&name, update).await {
                Ok(role) => {
                    let message = format!("Updated context {}", tui::accent(&name));
                    match (spinner, role) {
                        (Some(spinner), Some(role)) => spinner.done(format!(
                            "{message} {}",
                            tui::dim(format!("· acting as {}", role.display_name()))
                        )),
                        (Some(spinner), None) => spinner.done(message),
                        (None, _) => tui::success(message),
                    }
                    Ok(())
                }
                Err(report) => {
                    if let Some(spinner) = spinner {
                        spinner.fail(format!("Could not update {}", tui::accent(&name)));
                    }
                    Err(context_error(report))
                }
            }
        }
        Command::Inspect { names } => {
            let names = if names.is_empty() {
                if let Some(defined) = defined(over) {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&[Inspected::from_defined(defined)])
                            .expect("contexts serialize")
                    );
                    return Ok(());
                }
                vec![selected_name(service, over).await?]
            } else {
                names
            };
            let mut inspected = Vec::new();
            for name in names {
                inspected.push(Inspected::from(
                    service.inspect(&name).await.map_err(context_error)?,
                ));
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&inspected).expect("contexts serialize")
            );
            Ok(())
        }
        Command::Use { name } => {
            service.use_context(&name).await.map_err(context_error)?;
            tui::success(format!("Now using context {}", tui::accent(&name)));
            Ok(())
        }
        Command::Remove { names, force } => remove(service, names, force).await,
        Command::Rename { from, to } => {
            service.rename(&from, &to).await.map_err(context_error)?;
            tui::success(format!(
                "Renamed context {} to {}",
                tui::accent(&from),
                tui::accent(&to)
            ));
            Ok(())
        }
        Command::Show => {
            println!("{}", selected_name(service, over).await?);
            Ok(())
        }
        Command::Migrate { dry_run } => {
            match service.migrate(dry_run).await.map_err(context_error)? {
                None => tui::info("Contexts are already in the current format"),
                Some(migration) => {
                    for change in &migration.changes {
                        tui::step(change);
                    }
                    match &migration.backup {
                        None => tui::info("Dry run: nothing changed"),
                        Some(backup) => tui::success(format!(
                            "Contexts migrated {}",
                            tui::dim(format!("· previous files kept in {}", backup.display()))
                        )),
                    }
                }
            }
            Ok(())
        }
    }
}

/// Brings contexts stored by a previous version to the current format
/// before a command runs, and says so when it did. Silent otherwise, and
/// on failure: the command then meets the problem itself.
pub async fn migrate_on_start(service: &ContextService) {
    let Ok(Some(migration)) = service.migrate(false).await else {
        return;
    };
    let backup = migration
        .backup
        .map(|backup| format!(" · previous files kept in {}", backup.display()))
        .unwrap_or_default();
    tui::info(format!(
        "Contexts moved to the current format{}",
        tui::dim(backup)
    ));
    for change in &migration.changes {
        tui::step(change);
    }
}

async fn remove(service: &ContextService, names: Vec<String>, force: bool) -> Result<()> {
    let mut gone = Vec::new();
    for name in names {
        // Gone already, as derived from a login removed before it.
        if gone.contains(&name) {
            continue;
        }
        let summary = service.inspect(&name).await.map_err(context_error)?;
        let derived: Vec<_> = service
            .list()
            .await
            .map_err(context_error)?
            .into_iter()
            .filter(|other| {
                other
                    .context
                    .derivation()
                    .is_some_and(|derivation| derivation.from == name)
            })
            .map(|other| other.context.name)
            .collect();
        let mut cascade = force;
        if !force && tui::interactive() {
            let mut question = format!("Remove context {}", tui::accent(&name));
            if summary.context.endpoint().is_some() && summary.credentials.is_some() {
                question.push_str(" and its stored credentials");
            }
            if !derived.is_empty() {
                question.push_str(&format!(
                    ", with the contexts derived from it ({})",
                    derived.join(", ")
                ));
            }
            question.push('?');
            if !tui::confirm(question)
                .initial_value(false)
                .interact()
                .map_err(prompt_error)?
            {
                tui::info(format!("Kept {}", tui::accent(&name)));
                continue;
            }
            cascade = true;
        }
        let removed = service
            .remove(&name, cascade)
            .await
            .map_err(context_error)?;
        for removed in removed {
            tui::success(format!("Removed context {}", tui::accent(&removed)));
            gone.push(removed);
        }
    }
    Ok(())
}

/// The context the invocation defines, if it does.
fn defined(over: Option<&ContextOverride>) -> Option<&DefinedContext> {
    over.and_then(|over| over.defined.as_ref())
}

/// The order `ls` shows contexts in: each login, then the contexts derived
/// from it; any derived from a login that's gone, last.
fn tree(contexts: Vec<ContextSummary>) -> Vec<(ContextSummary, bool)> {
    let (logins, derived): (Vec<_>, Vec<_>) = contexts
        .into_iter()
        .partition(|summary| summary.context.endpoint().is_some());
    let mut derived: Vec<_> = derived.into_iter().map(Some).collect();
    let mut ordered = Vec::new();
    for login in logins {
        let name = login.context.name.clone();
        ordered.push((login, false));
        for slot in derived.iter_mut() {
            if slot
                .as_ref()
                .and_then(|summary| summary.context.derivation())
                .is_some_and(|derivation| derivation.from == name)
            {
                ordered.push((slot.take().expect("just checked"), true));
            }
        }
    }
    ordered.extend(
        derived
            .into_iter()
            .flatten()
            .map(|summary| (summary, false)),
    );
    ordered
}

fn describe_address(endpoint: &Endpoint) -> String {
    let mut address = endpoint.address.clone();
    if endpoint.tls.disabled {
        address.push_str(" (plaintext)");
    }
    address
}

async fn list(
    service: &ContextService,
    over: Option<&ContextOverride>,
    format: Format,
    quiet: bool,
    program: &str,
) -> Result<()> {
    let mut contexts = service.list().await.map_err(context_error)?;
    if quiet {
        // Names to pass to --context: a defined context has none.
        for summary in &contexts {
            println!("{}", summary.context.name);
        }
        return Ok(());
    }
    // What runs is the environment's context, first, and no stored one.
    let defined = defined(over);
    if defined.is_some() {
        for summary in &mut contexts {
            summary.current = false;
        }
    }
    match format {
        Format::Json => {
            let inspected: Vec<_> = defined
                .map(Inspected::from_defined)
                .into_iter()
                .chain(contexts.into_iter().map(Inspected::from))
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&inspected).expect("contexts serialize")
            );
        }
        Format::Table if contexts.is_empty() && defined.is_none() => {
            tui::info(format!(
                "No context yet. Start with {}",
                tui::accent(format!("{program} login"))
            ));
        }
        Format::Table => {
            let mut table = tui::Table::new(["name", "gateway", "login", "role", "description"]);
            if let Some(defined) = defined {
                table.row(
                    [
                        format!("{} *", defined.name),
                        describe_address(&defined.endpoint),
                        format!("{} (environment)", defined.credentials.kind()),
                        defined
                            .role
                            .as_deref()
                            .map_or_else(|| "—".to_owned(), |role| describe_role(role, None)),
                        format!("defined by {TOKEN_ENV}, not stored"),
                    ],
                    true,
                );
            }
            for (summary, nested) in tree(contexts) {
                let context = &summary.context;
                let mut name = if nested {
                    format!("└ {}", context.name)
                } else {
                    context.name.clone()
                };
                if summary.current {
                    name.push_str(" *");
                }
                let (gateway, login, role) = match &context.kind {
                    ContextKind::Login { endpoint } => (
                        describe_address(endpoint),
                        summary
                            .credentials
                            .map_or_else(|| "—".to_owned(), |kind| kind.to_string()),
                        "—".to_owned(),
                    ),
                    ContextKind::Derived(derivation) => (
                        String::new(),
                        match summary.credentials {
                            Some(_) => format!("via {}", derivation.from),
                            None => format!("via {} (logged out)", derivation.from),
                        },
                        describe_role(&derivation.role, derivation.account.as_deref()),
                    ),
                };
                table.row(
                    [
                        name,
                        gateway,
                        login,
                        role,
                        context.description.clone().unwrap_or_default(),
                    ],
                    summary.current,
                );
            }
            table.print();
        }
    }
    Ok(())
}

async fn selected_name(service: &ContextService, over: Option<&ContextOverride>) -> Result<String> {
    match service.selected(over).await.map_err(context_error)? {
        Some((name, _)) => Ok(name),
        None => Err(no_context()),
    }
}

fn describe_user(principal: &remora_context::model::Principal) -> String {
    if principal.user_name.is_empty() {
        principal.user_urn.clone()
    } else {
        principal.user_name.clone()
    }
}

/// The service's own refusal to change a context the invocation defines
/// (`over`), for the commands that change stored contexts without
/// selecting one (`context create`, `use`...): while the environment
/// defines the context, a change to the stored ones would not apply.
pub(crate) fn refuse_defined(over: Option<&ContextOverride>) -> Result<()> {
    match over {
        Some(over) if over.defined.is_some() => Err(context_error(Report::new(
            ContextError::Defined(over.name.clone()),
        ))),
        _ => Ok(()),
    }
}

/// The service's own "no context" failure, for when the transport is the
/// one finding there's none: it reads, and is hinted, alike.
pub(crate) fn no_context() -> Report<Error> {
    context_error(Report::new(ContextError::NoContext))
}

fn login_hint(program: &str, name: &str, current: bool) -> String {
    if current {
        format!("{program} login")
    } else {
        format!("{program} --context {name} login")
    }
}

/// How a selection reads in a sentence ("from --context").
pub(crate) fn describe_selection(selection: Selection) -> &'static str {
    match selection {
        Selection::Flag => "from --context",
        Selection::Environment => "from RMRA_CONTEXT",
        Selection::Current => "current",
        Selection::Only => "the only context",
        Selection::Defined => "from RMRA_TOKEN",
    }
}

/// `context inspect`'s JSON: the stored context plus what isn't in it.
/// Never any secret -- only the kind of its login's credentials. A context
/// the environment defines reads as a login, marked `environment`, with
/// the role it assumes.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Inspected {
    #[serde(flatten)]
    context: Context,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    current: bool,
    credentials: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    environment: bool,
}

impl From<ContextSummary> for Inspected {
    fn from(summary: ContextSummary) -> Self {
        Self {
            context: summary.context,
            role: None,
            current: summary.current,
            credentials: summary.credentials.map(|kind| kind.to_string()),
            environment: false,
        }
    }
}

impl Inspected {
    fn from_defined(defined: &DefinedContext) -> Self {
        Self {
            context: Context::login(defined.name.clone(), defined.endpoint.clone()),
            role: defined.role.clone(),
            current: true,
            credentials: Some(defined.credentials.kind().to_string()),
            environment: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use remora_context::model::{CredentialKind, Derivation};

    use super::*;

    fn summary(context: Context) -> ContextSummary {
        ContextSummary {
            context,
            current: false,
            credentials: Some(CredentialKind::AccessKey),
        }
    }

    fn derived(name: &str, from: &str) -> Context {
        Context {
            name: name.into(),
            description: None,
            kind: ContextKind::Derived(Derivation {
                from: from.into(),
                role: "urn:sntns:iam:eu2:t:role:ops".into(),
                account: None,
            }),
        }
    }

    #[test]
    fn inspected_shows_a_derived_context_without_an_endpoint() {
        let inspected = Inspected::from(summary(derived("acme", "eu2")));
        let value: serde_json::Value =
            serde_json::to_value(&inspected).expect("contexts serialize");
        assert_eq!(value["name"], "acme");
        assert_eq!(value["from"], "eu2");
        assert_eq!(value["credentials"], "access key");
        assert!(value.get("endpoint").is_none(), "{value}");
    }

    #[test]
    fn ls_nests_derived_contexts_under_their_login() {
        let order: Vec<_> = tree(vec![
            summary(derived("acme", "eu2")),
            summary(Context::login("dev", Endpoint::new("dev"))),
            summary(derived("orphan", "gone")),
            summary(Context::login("eu2", Endpoint::new("eu2"))),
            summary(derived("zeta", "eu2")),
        ])
        .into_iter()
        .map(|(summary, nested)| (summary.context.name, nested))
        .collect();
        assert_eq!(
            order,
            [
                ("dev".into(), false),
                ("eu2".into(), false),
                ("acme".into(), true),
                ("zeta".into(), true),
                ("orphan".into(), false),
            ]
        );
    }
}
