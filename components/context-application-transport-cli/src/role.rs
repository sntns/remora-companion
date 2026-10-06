use remora_context::{application::ContextService, model::ContextOverride};
use remora_tui as tui;
use serde_json::json;

use crate::error::{context_error, Result};

#[derive(clap::Subcommand)]
pub enum Command {
    /// List the selected context's roles; the assumed one is marked `*`.
    #[command(visible_alias = "ls")]
    List {
        /// `table` for humans, `json` for scripts.
        #[arg(long, default_value = "table")]
        format: crate::service::Format,
    },

    /// Remember a role under an alias, e.g. a role of another tenant.
    Add {
        /// A short name for it (letters, digits, `_.+-`).
        alias: String,
        /// The role's URN, e.g. urn:sntns:iam:eu2:<tenant>:role:<name>.
        urn: String,
        /// Also assume it right away.
        #[arg(long = "assume")]
        assume: bool,
    },

    /// Forget a role alias.
    #[command(visible_alias = "rm")]
    Remove {
        #[arg(add = remora_completion::values(remora_completion::Kind::Role))]
        alias: String,
    },

    /// Act as a role for every later command of this context, once the
    /// platform confirms the login may assume it.
    Assume {
        /// An alias (`context role ls`), or a role URN.
        #[arg(add = remora_completion::values(remora_completion::Kind::Role))]
        role: String,
    },

    /// Stop assuming a role: act as the login itself again.
    Drop,
}

pub async fn run(
    command: Command,
    service: &ContextService,
    over: Option<&ContextOverride>,
    program: &str,
) -> Result<()> {
    match command {
        Command::List { format } => {
            let (context, roles, _) = service.roles(over).await.map_err(context_error)?;
            match format {
                crate::service::Format::Json => println!(
                    "{}",
                    serde_json::to_string_pretty(&json!(roles
                        .iter()
                        .map(|r| json!({"alias": r.alias, "urn": r.urn, "assumed": r.assumed}))
                        .collect::<Vec<_>>()))
                    .expect("json values serialize")
                ),
                crate::service::Format::Table if roles.is_empty() => tui::info(format!(
                    "No role in context {}. Add one with {}",
                    tui::accent(&context),
                    tui::accent(format!("{program} context role add <alias> <role-urn>"))
                )),
                crate::service::Format::Table => {
                    let mut table = tui::Table::new(["alias", "urn"]);
                    for role in &roles {
                        let alias = if role.assumed {
                            format!("{} *", role.alias)
                        } else {
                            role.alias.clone()
                        };
                        table.row([alias, role.urn.clone()], role.assumed);
                    }
                    table.print();
                }
            }
            Ok(())
        }
        Command::Add { alias, urn, assume } => {
            service
                .add_role(over, &alias, &urn)
                .await
                .map_err(context_error)?;
            tui::success(format!(
                "Added role {} {}",
                tui::accent(&alias),
                tui::dim(&urn)
            ));
            if assume {
                assume_role(service, over, &alias).await?;
            } else {
                tui::step(format!(
                    "Assume it with {}",
                    tui::accent(format!("{program} context role assume {alias}"))
                ));
            }
            Ok(())
        }
        Command::Remove { alias } => {
            service
                .remove_role(over, &alias)
                .await
                .map_err(context_error)?;
            tui::success(format!("Removed role {}", tui::accent(&alias)));
            Ok(())
        }
        Command::Assume { role } => assume_role(service, over, &role).await,
        Command::Drop => {
            let (context, dropped) = service.drop_role(over).await.map_err(context_error)?;
            match dropped {
                Some(role) => tui::success(format!(
                    "No longer assuming {} in {}: acting as the login itself",
                    tui::accent(role),
                    tui::accent(context)
                )),
                None => tui::info(format!("No role assumed in {}", tui::accent(context))),
            }
            Ok(())
        }
    }
}

async fn assume_role(
    service: &ContextService,
    over: Option<&ContextOverride>,
    role: &str,
) -> Result<()> {
    let spinner = tui::Spinner::start(format!("Assuming {}", tui::accent(role)));
    match service.assume_role(over, role).await {
        Ok((context, assumed, account)) => {
            let account = account
                .map(|account| format!(" in account {}", tui::accent(account)))
                .unwrap_or_default();
            spinner.done(format!(
                "Context {} now acts as {}{account}",
                tui::accent(context),
                tui::accent(assumed.display_name())
            ));
            Ok(())
        }
        Err(report) => {
            spinner.fail(format!("Could not assume {}", tui::accent(role)));
            Err(context_error(report))
        }
    }
}
