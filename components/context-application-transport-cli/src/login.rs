use std::io::Read;

use error_stack::{Report, ResultExt};
use remora_context::{
    application::ContextService,
    model::{
        Context, ContextOverride, Credentials, Endpoint, Principal, RoleOverride, Secret, Tls,
    },
};
use remora_tui as tui;

use crate::{
    error::{context_error, prompt_error, Error, Result},
    service::{describe_selection, DEFAULT_ADDRESS},
};

#[derive(clap::Args)]
pub struct LoginArgs {
    /// Log in with a login profile: the user's URN. The password is asked
    /// for, or read with --password-stdin.
    #[arg(long, conflicts_with = "token_stdin")]
    identity: Option<String>,
    /// Read the login profile's password from stdin.
    #[arg(long, requires = "identity")]
    password_stdin: bool,
    /// Log in with an access key, reading its token from stdin.
    #[arg(long)]
    token_stdin: bool,
}

#[derive(clap::Args)]
pub struct LogoutArgs {}

#[derive(clap::Args)]
pub struct WhoamiArgs {}

/// `rmra login`: verify credentials with the selected context's platform,
/// then store them. With no flags it is a guided prompt -- including, on a
/// first run, creating the context to log in to.
pub async fn run_login(
    args: LoginArgs,
    service: &ContextService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let interactive = args.identity.is_none() && !args.token_stdin;
    if interactive && !tui::interactive() {
        return Err(Report::new(Error::Usage(
            "not a terminal: pass --token-stdin, or --identity with --password-stdin".into(),
        )));
    }
    if interactive {
        tui::intro("rmra login");
    }

    let name = match service.selected(over).await.map_err(context_error)? {
        Some((name, selection)) => {
            let context = service.inspect(&name).await.map_err(context_error)?.context;
            tui::step(format!(
                "Context {} {}",
                tui::accent(&name),
                tui::dim(format!(
                    "{} · {}",
                    context.endpoint.address,
                    describe_selection(selection)
                ))
            ));
            name
        }
        None if interactive => first_context(service).await?,
        None => {
            return Err(Report::new(Error::Context(
                remora_context::application::Error::NoContext.to_string(),
            )))
        }
    };
    // From here on, address the context by name: a first-run context was
    // just created, and an override already named it. The role travels
    // separately: logging in decides it for the context.
    let target = ContextOverride {
        name: Some(name.clone()),
        source: over.map_or(remora_context::model::Selection::Current, |o| o.source),
        role: RoleOverride::Keep,
    };

    let secret = if let Some(identity) = args.identity {
        let password = if args.password_stdin {
            read_stdin()?
        } else {
            prompt_password("Password")?
        };
        Secret::LoginProfile { identity, password }
    } else if args.token_stdin {
        Secret::AccessKey {
            token: read_stdin()?,
        }
    } else {
        prompt_secret()?
    };
    let credentials = Credentials { secret };

    // Which role the context acts as: --assume-role / --no-assume-role, else
    // asked when a human is there, else whatever the context already says.
    let role = match over.map(|o| o.role.clone()).unwrap_or_default() {
        RoleOverride::Keep if interactive => prompt_role(service, &target).await?,
        role => role,
    };

    let spinner = tui::Spinner::start("Verifying with the platform");
    match service.login(Some(&target), credentials, role).await {
        Ok((name, principal, assumed)) => {
            let acting = match &assumed {
                Some((role, account)) => format!(
                    " {} {}",
                    tui::dim("acting as"),
                    tui::accent(match account {
                        Some(account) => format!("{} @ {account}", role.display_name()),
                        None => role.display_name().to_owned(),
                    })
                ),
                None => String::new(),
            };
            spinner.done(format!(
                "Logged in as {}{acting}",
                describe_principal(&principal)
            ));
            if let Some((_, None)) = &assumed {
                tui::warning("This role may not read its account: assumed all the same.");
            }
            if interactive {
                tui::outro(format!(
                    "Credentials{} stored for context {}",
                    if assumed.is_some() { " and role" } else { "" },
                    tui::accent(&name)
                ));
            }
            Ok(())
        }
        Err(report) => {
            spinner.fail("The platform did not accept this login");
            Err(context_error(report))
        }
    }
}

/// Asks which role the context should act as: the login itself, one of the
/// context's roles, or another role by URN. Defaults to what the context
/// already does.
async fn prompt_role(service: &ContextService, target: &ContextOverride) -> Result<RoleOverride> {
    #[derive(Clone, PartialEq, Eq)]
    enum Choice {
        Myself,
        Alias(String),
        Other,
    }
    let (_, roles) = service.roles(Some(target)).await.map_err(context_error)?;
    let assumed = roles
        .iter()
        .find(|role| role.assumed)
        .map(|role| role.alias.clone());
    let mut select = tui::select("Act as")
        .item(Choice::Myself, "Myself", "the login's own account, no role")
        .initial_value(match &assumed {
            Some(alias) => Choice::Alias(alias.clone()),
            None => Choice::Myself,
        });
    for role in &roles {
        select = select.item(Choice::Alias(role.alias.clone()), &role.alias, &role.urn);
    }
    select = select.item(
        Choice::Other,
        "Another role…",
        "by its URN, e.g. a role of another tenant",
    );
    Ok(match select.interact().map_err(prompt_error)? {
        Choice::Myself => RoleOverride::Drop,
        Choice::Alias(alias) => RoleOverride::Assume(alias),
        Choice::Other => {
            let urn: String = tui::input("Role URN")
                .placeholder("urn:sntns:iam:…:role:…")
                .validate(|urn: &String| {
                    if urn.starts_with("urn:") && urn.contains(":role:") {
                        Ok(())
                    } else {
                        Err("a role URN looks like urn:…:role:…")
                    }
                })
                .interact()
                .map_err(prompt_error)?;
            RoleOverride::Assume(urn)
        }
    })
}

pub async fn run_logout(
    _args: LogoutArgs,
    service: &ContextService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let (name, removed) = service.logout(over).await.map_err(context_error)?;
    if removed {
        tui::success(format!("Logged out of {}", tui::accent(&name)));
    } else {
        tui::info(format!("Not logged in to {}", tui::accent(&name)));
    }
    Ok(())
}

pub async fn run_whoami(
    _args: WhoamiArgs,
    service: &ContextService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let spinner = tui::Spinner::start("Asking the platform");
    let (resolved, principal, acting) = match service.whoami(over).await {
        Ok(found) => found,
        Err(report) => {
            spinner.fail("Could not identify the current login");
            return Err(context_error(report));
        }
    };
    let headline = match (&resolved.role, &acting) {
        (Some(role), Some(account)) => format!(
            "{} {} {}",
            describe_principal(&principal),
            tui::dim("acting as"),
            tui::accent(match account {
                Some(account) => format!("{} @ {account}", role.display_name()),
                None => role.display_name().to_owned(),
            })
        ),
        _ => describe_principal(&principal),
    };
    spinner.done(headline);
    let mut body = format!(
        "context   {} ({})\ngateway   {}\nlogin     {}\nuser      {}",
        resolved.context.name,
        describe_selection(resolved.selection),
        resolved.context.endpoint.address,
        resolved.credentials.kind(),
        principal.user_urn,
    );
    if let Some(account) = &principal.account_name {
        body.push_str(&format!("\naccount   {account}"));
    }
    if let Some(role) = &resolved.role {
        body.push_str(&format!("\nrole      {}", role.urn));
        match &acting {
            Some(Some(account)) => body.push_str(&format!("\nacting in {account}")),
            Some(None) => body.push_str("\nacting in (this role may not read its account)"),
            None => {}
        }
    }
    tui::note("Session", body);
    Ok(())
}

/// The first-run path of `rmra login`: no context exists, so create one.
async fn first_context(service: &ContextService) -> Result<String> {
    tui::info("No context yet — let's create one.");
    let name: String = tui::input("Context name")
        .default_input("eu2")
        .interact()
        .map_err(prompt_error)?;
    let address: String = tui::input("Gateway address")
        .default_input(DEFAULT_ADDRESS)
        .interact()
        .map_err(prompt_error)?;
    service
        .create(
            Context {
                name: name.clone(),
                description: None,
                endpoint: Endpoint {
                    address,
                    tls: Tls::default(),
                },
                roles: Default::default(),
                assumed_role: None,
            },
            false,
        )
        .await
        .map_err(context_error)?;
    service.use_context(&name).await.map_err(context_error)?;
    tui::success(format!("Created context {}", tui::accent(&name)));
    Ok(name)
}

fn prompt_secret() -> Result<Secret> {
    #[derive(Clone, PartialEq, Eq)]
    enum Method {
        AccessKey,
        LoginProfile,
    }
    let method = tui::select("How do you want to log in?")
        .item(
            Method::AccessKey,
            "Access key",
            "a token, for automation or an operator key",
        )
        .item(
            Method::LoginProfile,
            "Login profile",
            "your user URN and password",
        )
        .interact()
        .map_err(prompt_error)?;
    Ok(match method {
        Method::AccessKey => Secret::AccessKey {
            token: prompt_password("Access key token")?,
        },
        Method::LoginProfile => {
            let identity: String = tui::input("User URN")
                .placeholder("urn:sntns:iam:…:user:…")
                .interact()
                .map_err(prompt_error)?;
            Secret::LoginProfile {
                identity,
                password: prompt_password("Password")?,
            }
        }
    })
}

fn prompt_password(prompt: &str) -> Result<String> {
    if !tui::interactive() {
        return Err(Report::new(Error::Usage(format!(
            "not a terminal: cannot ask for the {}",
            prompt.to_lowercase()
        ))));
    }
    tui::password(prompt)
        .mask('•')
        .interact()
        .map_err(prompt_error)
}

/// One secret from stdin, `docker login --password-stdin` style: the whole
/// input, minus the trailing newline an `echo` or a file adds.
fn read_stdin() -> Result<String> {
    let mut secret = String::new();
    std::io::stdin()
        .read_to_string(&mut secret)
        .change_context(Error::Stdin)?;
    let secret = secret.trim_end_matches(['\r', '\n']).to_owned();
    if secret.is_empty() {
        return Err(Report::new(Error::Usage("stdin was empty".into())));
    }
    Ok(secret)
}

fn describe_principal(principal: &Principal) -> String {
    let user = if principal.user_name.is_empty() {
        principal.user_urn.clone()
    } else {
        principal.user_name.clone()
    };
    match &principal.account_name {
        Some(account) => format!("{} {}", tui::accent(user), tui::dim(format!("@ {account}"))),
        None => tui::accent(user),
    }
}
