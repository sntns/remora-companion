use std::{path::PathBuf, sync::Arc};

use error_stack::{Report, ResultExt};
use remora_channel::{
    application::ChannelService,
    model::{
        ChannelKind, PreparedSsh, ProxyCommandBuilder, ScpRequest, SshRequest, SshRole, SSH_PROFILE,
    },
};
use remora_context::model::ContextOverride;
use remora_tui as tui;

use crate::{
    error::{channel_error, Error, Result},
    relay,
};

#[derive(clap::Subcommand)]
pub enum Command {
    /// Open a channel on a device and join it to stdin/stdout -- what ssh
    /// runs as its ProxyCommand. Nothing but the channel's bytes is ever
    /// written to stdout.
    Open {
        /// The device's name (its serial).
        #[arg(add = remora_completion::values(remora_completion::Kind::Device))]
        device: String,
        /// What to open: the profile a policy grants, never a host and port.
        #[arg(long, default_value = SSH_PROFILE)]
        profile: String,
        /// Say nothing once the channel is open (ssh passes a
        /// ProxyCommand's stderr through to the terminal).
        #[arg(short, long)]
        quiet: bool,
    },

    /// Log into a device's sshd through its ssh channel (same as `rmra ssh`).
    Ssh(SshArgs),

    /// Copy files to or from a device through its ssh channel (same as
    /// `rmra scp`).
    Scp(ScpArgs),
}

#[derive(clap::Args)]
pub struct SshArgs {
    /// The device's name (its serial).
    #[arg(add = remora_completion::values(remora_completion::Kind::Device))]
    device: String,
    /// The role to be on the device; each is its own IAM action.
    #[arg(long, value_enum, default_value = "user")]
    role: Role,
    /// The device account to log into, when not the one the device maps
    /// the role to (the certificate still names only the role).
    #[arg(long)]
    user: Option<String>,
    /// An ssh -o option, e.g. "LocalForward 127.0.0.1:8080 127.0.0.1:80";
    /// repeatable.
    #[arg(long = "ssh-option", value_name = "OPTION")]
    ssh_options: Vec<String>,
    /// The ssh client to run.
    #[arg(long = "ssh", default_value = "ssh", value_name = "PATH")]
    #[arg(value_hint = clap::ValueHint::ExecutablePath)]
    binary: PathBuf,
    /// ssh's own arguments: options (-t, -L ...), then the remote command.
    /// `-v` is rmra's own --verbose; for ssh's, pass it after `--`. -J, -W,
    /// -F and -o ProxyCommand/ProxyJump are refused: they would bypass the
    /// channel or the host pinning.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "SSH ARGS"
    )]
    arguments: Vec<String>,
}

#[derive(clap::Args)]
#[command(after_help = "Examples:\n  \
    rmra scp ./bundle.raucb 525400C0FFEE:/data/\n  \
    rmra scp -r root@525400C0FFEE:/var/log ./logs")]
pub struct ScpArgs {
    /// The role to be on the device; each is its own IAM action.
    #[arg(long, value_enum, default_value = "user")]
    role: Role,
    /// The device account, when neither a `user@` operand nor the role's
    /// default should decide it.
    #[arg(long)]
    user: Option<String>,
    /// An ssh -o option; repeatable.
    #[arg(long = "ssh-option", value_name = "OPTION")]
    ssh_options: Vec<String>,
    /// The scp client to run.
    #[arg(long = "scp", default_value = "scp", value_name = "PATH")]
    #[arg(value_hint = clap::ValueHint::ExecutablePath)]
    binary: PathBuf,
    /// scp's own arguments: options (-r, -p, -l ...), then the files, the
    /// device side written [user@]DEVICE:path. One device per copy. `-c` and
    /// `-v` are rmra's own (--context, --verbose); for scp's, pass them after
    /// `--`. -J, -F, -S and
    /// -o ProxyCommand/ProxyJump are refused: they would bypass the channel
    /// or the host pinning.
    #[arg(
        required = true,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "SCP ARGS"
    )]
    #[arg(add = remora_completion::scp_operand())]
    arguments: Vec<String>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum Role {
    User,
    Admin,
}

impl From<Role> for SshRole {
    fn from(role: Role) -> Self {
        match role {
            Role::User => SshRole::User,
            Role::Admin => SshRole::Admin,
        }
    }
}

/// Runs a `channel` subcommand; returns the process exit code.
///
/// `verbose` adds debug detail on stderr (see [`run_ssh`]).
pub async fn run(
    command: Command,
    service: &ChannelService,
    over: Option<&ContextOverride>,
    verbose: bool,
) -> Result<i32> {
    match command {
        Command::Open {
            device,
            profile,
            quiet,
        } => {
            let channel = service
                .open(over, &device, &profile)
                .await
                .map_err(channel_error)?;
            if channel.opened.kind == ChannelKind::Datagram {
                return Err(Report::new(Error::Datagram(profile)));
            }
            if !quiet {
                tui::success(format!(
                    "Channel open to {} {}",
                    tui::accent(&device),
                    tui::dim(&channel.opened.device_urn)
                ));
            }
            relay::stdio(channel).await?;
            Ok(0)
        }
        Command::Ssh(args) => run_ssh(args, service, over, verbose).await,
        Command::Scp(args) => run_scp(args, service, over, verbose).await,
    }
}

/// `rmra ssh`: certify a throwaway key, then hand the terminal to ssh with
/// this program as its ProxyCommand. Returns ssh's exit code.
///
/// Silent on success: a spinner while the key is certified, cleared before
/// ssh takes the terminal, so the session starts on a clean screen like
/// plain `ssh`. `verbose` prints what the session was set up with instead
/// (context, role, login, host alias, key directory, the ssh command line),
/// for when a login doesn't go through.
pub async fn run_ssh(
    args: SshArgs,
    service: &ChannelService,
    over: Option<&ContextOverride>,
    verbose: bool,
) -> Result<i32> {
    let role = args.role.into();
    let spinner = certifying(&args.device, role);
    let prepared = service
        .prepare_ssh(SshRequest {
            over: over.cloned(),
            device: args.device,
            role,
            login: args.user,
            options: args.ssh_options,
            arguments: args.arguments,
            binary: args.binary,
            proxy_command: proxy_command()?,
        })
        .await;
    run_prepared(service, prepared, spinner, verbose).await
}

/// `rmra scp`: scp to or from a device, `[user@]DEVICE:path` on the device
/// side, through its channel with a throwaway certified key -- `rmra ssh`'s
/// setup for scp. Returns scp's exit code; as quiet as plain scp.
pub async fn run_scp(
    args: ScpArgs,
    service: &ChannelService,
    over: Option<&ContextOverride>,
    verbose: bool,
) -> Result<i32> {
    let role = args.role.into();
    let spinner = certifying("the device", role);
    let prepared = service
        .prepare_scp(ScpRequest {
            over: over.cloned(),
            role,
            login: args.user,
            options: args.ssh_options,
            arguments: args.arguments,
            binary: args.binary,
            proxy_command: proxy_command()?,
        })
        .await;
    run_prepared(service, prepared, spinner, verbose).await
}

/// This program's own `channel open`, quoted for the shell ssh runs a
/// ProxyCommand with, under the context and for the device the session
/// resolved to.
fn proxy_command() -> Result<ProxyCommandBuilder> {
    let executable = std::env::current_exe().change_context(Error::SelfPath)?;
    Ok(Arc::new(move |context: &str, device: &str| {
        [
            executable.display().to_string(),
            "--context".into(),
            context.into(),
            "channel".into(),
            "open".into(),
            device.into(),
            "--profile".into(),
            SSH_PROFILE.into(),
            "--quiet".into(),
        ]
        .iter()
        .map(|word| quote(word))
        .collect::<Vec<_>>()
        .join(" ")
    }))
}

/// A spinner while the key is certified -- only on a terminal: off one, the
/// plain-line fallback would be setup chatter in a script's stderr, which
/// plain ssh/scp don't produce either.
fn certifying(device: &str, role: SshRole) -> Option<tui::Spinner> {
    tui::decorated().then(|| {
        tui::Spinner::start(format!(
            "Certifying a throwaway key for {} {}",
            tui::accent(device),
            tui::dim(format!("as {}", role.as_str()))
        ))
    })
}

async fn run_prepared(
    service: &ChannelService,
    prepared: remora_channel::application::Result<PreparedSsh>,
    spinner: Option<tui::Spinner>,
    verbose: bool,
) -> Result<i32> {
    // The error itself is rendered by the caller; a second line saying the
    // same would only be noise.
    let prepared = prepared.map_err(channel_error)?;
    // Cleared, not finished: nothing of the setup stays on screen.
    drop(spinner);

    if verbose {
        let debug = |label: &str, value: &str| {
            eprintln!("{} {label:<9} {value}", tui::dim("debug:"));
        };
        debug("context", &prepared.context);
        debug("device", &prepared.device);
        debug("role", prepared.role.as_str());
        debug(
            "login",
            &format!("{}@{}", prepared.login, prepared.host_key_alias),
        );
        debug("keys", &prepared.workdir().display().to_string());
        debug(
            "command",
            &std::iter::once(prepared.command.program.display().to_string())
                .chain(prepared.command.arguments.iter().map(|a| quote(a)))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }

    service.run_ssh(prepared).await.map_err(channel_error)
}

/// Quotes a word for the command line ssh runs a ProxyCommand with: `sh -c`
/// on Unix, CreateProcess on Windows.
fn quote(word: &str) -> String {
    if cfg!(windows) {
        if word.contains([' ', '\t', '"']) {
            format!("\"{}\"", word.replace('"', "\\\""))
        } else {
            word.to_owned()
        }
    } else if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,".contains(c))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

// Unix quoting only: Windows quotes for CreateProcess instead.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn quotes_only_what_the_shell_would_split() {
        assert_eq!(quote("/usr/bin/rmra"), "/usr/bin/rmra");
        assert_eq!(quote("/opt/my tools/rmra"), "'/opt/my tools/rmra'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(quote(""), "''");
    }
}
