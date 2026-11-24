use std::io::Read;

use error_stack::{Report, ResultExt};
use remora_context::{
    application::ContextService,
    model::{ContextKind, ContextOverride, Credentials, Endpoint, Principal, Secret},
};
use remora_tui as tui;

use crate::{
    error::{context_error, prompt_error, Error, Result},
    service::{describe_selection, no_context, refuse_defined, DEFAULT_ADDRESS},
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

/// `<bin> login`: verify credentials with the selected context's platform,
/// then store them -- for its login: its own, or the one it derives from.
/// With no flags it is a guided prompt -- including, on a first run,
/// creating the context to log in to. `program` is the running
/// binary's name, for the prompt's title.
pub async fn run_login(
    args: LoginArgs,
    service: &ContextService,
    over: Option<&ContextOverride>,
    program: &str,
) -> Result<()> {
    // Before any prompt: a context the environment defines has no login.
    refuse_defined(over)?;
    let interactive = args.identity.is_none() && !args.token_stdin;
    if interactive && !tui::interactive() {
        return Err(Report::new(Error::Usage(
            "not a terminal: pass --token-stdin, or --identity with --password-stdin".into(),
        )));
    }
    if interactive {
        tui::intro(format!("{program} login"));
    }

    let name = match service.selected(over).await.map_err(context_error)? {
        Some((name, selection)) => {
            let context = service.inspect(&name).await.map_err(context_error)?.context;
            let target = match &context.kind {
                ContextKind::Login { endpoint } => endpoint.address.clone(),
                ContextKind::Derived(derivation) => format!("the login of {}", derivation.from),
            };
            tui::step(format!(
                "Context {} {}",
                tui::accent(&name),
                tui::dim(format!("{target} · {}", describe_selection(selection)))
            ));
            name
        }
        None if interactive => first_context(service).await?,
        None => return Err(no_context()),
    };
    // From here on, address the context by name: a first-run context was
    // just created, and an override already named it.
    let target = ContextOverride::named(
        name.clone(),
        over.map_or(remora_context::model::Selection::Current, |o| o.source),
    );

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

    let spinner = tui::Spinner::start("Verifying with the platform");
    match service.login(Some(&target), credentials).await {
        Ok((login, principal)) => {
            spinner.done(format!("Logged in as {}", describe_principal(&principal)));
            let derived: Vec<_> = service
                .list()
                .await
                .map_err(context_error)?
                .into_iter()
                .filter(|summary| {
                    summary
                        .context
                        .derivation()
                        .is_some_and(|derivation| derivation.from == login)
                })
                .map(|summary| summary.context.name)
                .collect();
            let shared = if derived.is_empty() {
                String::new()
            } else {
                format!(", also used by {}", derived.join(", "))
            };
            if interactive {
                tui::outro(format!(
                    "Credentials stored for context {}{shared}",
                    tui::accent(&login)
                ));
            } else if login != name || !derived.is_empty() {
                tui::info(format!(
                    "Credentials stored for context {}{shared}",
                    tui::accent(&login)
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
                Some(account) => format!("{} @ {account}", role.short_name()),
                None => role.short_name().to_owned(),
            })
        ),
        _ => describe_principal(&principal),
    };
    spinner.done(headline);
    let login = if resolved.login == resolved.name {
        resolved.credentials.kind().to_string()
    } else {
        format!("{}, of {}", resolved.credentials.kind(), resolved.login)
    };
    let mut body = format!(
        "context   {} ({})\ngateway   {}\nlogin     {login}\nuser      {}",
        resolved.name,
        describe_selection(resolved.selection),
        resolved.endpoint.address,
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

/// The first-run path of `<bin> login`: no context exists, so create one.
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
        .create(&name, None, Endpoint::new(address), false)
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
