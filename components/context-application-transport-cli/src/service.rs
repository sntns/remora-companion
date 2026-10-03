use std::path::PathBuf;

use error_stack::{Report, ResultExt};
use remora_context::{
    application::ContextService,
    model::{Context, ContextOverride, ContextSummary, Endpoint, Selection, Tls},
};
use remora_tui as tui;
use serde::Serialize;

use crate::error::{context_error, prompt_error, Error, Result};

/// The production gateway a fresh `rmra login` suggests.
pub const DEFAULT_ADDRESS: &str = "api.eu2.sntns.io:50051";

#[derive(clap::Subcommand)]
pub enum Command {
    /// List contexts; the current one is marked with `*`.
    #[command(visible_alias = "ls")]
    List {
        /// `table` for humans, `json` for scripts.
        #[arg(long, default_value = "table")]
        format: Format,
        /// Only print context names.
        #[arg(short, long)]
        quiet: bool,
    },

    /// Create a context: a named gateway endpoint to log in to.
    Create {
        name: String,
        /// The gateway, `host:port`.
        #[arg(long, default_value = DEFAULT_ADDRESS)]
        address: String,
        #[arg(long)]
        description: Option<String>,
        /// Talk plaintext h2c (local development stacks only).
        #[arg(long)]
        plaintext: bool,
        /// A PEM certificate authority to trust for this gateway, on top of
        /// the system's; repeatable.
        #[arg(long = "ca-file", value_name = "PEM")]
        #[arg(value_hint = clap::ValueHint::FilePath)]
        ca_files: Vec<PathBuf>,
        /// Verify the gateway's certificate against this name instead of
        /// the address's host.
        #[arg(long)]
        server_name: Option<String>,
        /// Replace an existing context of that name (its login is kept).
        #[arg(long)]
        force: bool,
        /// Also make it the current context.
        #[arg(long = "use")]
        make_current: bool,
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
        /// Don't ask for confirmation.
        #[arg(short, long)]
        force: bool,
    },

    /// Print the selected context's name.
    Show,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Format {
    Table,
    Json,
}

pub async fn run(
    command: Command,
    service: &ContextService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    match command {
        Command::List { format, quiet } => list(service, format, quiet).await,
        Command::Create {
            name,
            address,
            description,
            plaintext,
            ca_files,
            server_name,
            force,
            make_current,
        } => {
            let mut authorities = Vec::new();
            for path in ca_files {
                authorities
                    .push(std::fs::read_to_string(&path).change_context(Error::ReadFile(path))?);
            }
            let context = Context {
                name: name.clone(),
                description,
                endpoint: Endpoint {
                    address,
                    tls: Tls {
                        disabled: plaintext,
                        authorities,
                        server_name,
                    },
                },
                roles: Default::default(),
                assumed_role: None,
            };
            let address = context.endpoint.address.clone();
            service
                .create(context, force)
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
            tui::step(format!(
                "Log in with {}",
                tui::accent(login_hint(&name, make_current))
            ));
            Ok(())
        }
        Command::Inspect { names } => {
            let names = if names.is_empty() {
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
        Command::Remove { names, force } => {
            for name in names {
                let summary = service.inspect(&name).await.map_err(context_error)?;
                if !force && tui::interactive() {
                    let mut question = format!("Remove context {}", tui::accent(&name));
                    if summary.credentials.is_some() {
                        question.push_str(" and its stored credentials");
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
                }
                service.remove(&name).await.map_err(context_error)?;
                tui::success(format!("Removed context {}", tui::accent(&name)));
            }
            Ok(())
        }
        Command::Show => {
            println!("{}", selected_name(service, over).await?);
            Ok(())
        }
    }
}

async fn list(service: &ContextService, format: Format, quiet: bool) -> Result<()> {
    let contexts = service.list().await.map_err(context_error)?;
    if quiet {
        for summary in &contexts {
            println!("{}", summary.context.name);
        }
        return Ok(());
    }
    match format {
        Format::Json => {
            let inspected: Vec<_> = contexts.into_iter().map(Inspected::from).collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&inspected).expect("contexts serialize")
            );
        }
        Format::Table if contexts.is_empty() => {
            tui::info(format!(
                "No context yet. Start with {}",
                tui::accent("rmra login")
            ));
        }
        Format::Table => {
            let mut table = tui::Table::new(["name", "address", "login", "role", "description"]);
            for summary in &contexts {
                let name = if summary.current {
                    format!("{} *", summary.context.name)
                } else {
                    summary.context.name.clone()
                };
                let mut address = summary.context.endpoint.address.clone();
                if summary.context.endpoint.tls.disabled {
                    address.push_str(" (plaintext)");
                }
                table.row(
                    [
                        name,
                        address,
                        summary
                            .credentials
                            .map_or_else(|| "—".to_owned(), |kind| kind.to_string()),
                        summary
                            .context
                            .assumed_role
                            .clone()
                            .unwrap_or_else(|| "—".to_owned()),
                        summary.context.description.clone().unwrap_or_default(),
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
        None => Err(Report::new(Error::Context(
            remora_context::application::Error::NoContext.to_string(),
        ))),
    }
}

fn login_hint(name: &str, current: bool) -> String {
    if current {
        "rmra login".to_owned()
    } else {
        format!("rmra --context {name} login")
    }
}

/// How a selection reads in a sentence ("from --context").
pub(crate) fn describe_selection(selection: Selection) -> &'static str {
    match selection {
        Selection::Flag => "from --context",
        Selection::Environment => "from RMRA_CONTEXT",
        Selection::Current => "current",
        Selection::Only => "the only context",
    }
}

/// `context inspect`'s JSON: the stored context plus what isn't in it.
/// Never any secret -- only the kind of login.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Inspected {
    #[serde(flatten)]
    context: Context,
    current: bool,
    login: Option<String>,
}

impl From<ContextSummary> for Inspected {
    fn from(summary: ContextSummary) -> Self {
        Self {
            context: summary.context,
            current: summary.current,
            login: summary.credentials.map(|kind| kind.to_string()),
        }
    }
}
