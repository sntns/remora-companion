//! Dynamic shell completion (`COMPLETE=$SHELL rmra`, clap_complete's
//! environment-driven engine), shared by the CLI transport crates.
//!
//! A transport marks which arguments take which [`Kind`] of value
//! (`#[arg(add = remora_completion::values(Kind::Device))]`); the binary,
//! which is what can reach the services, [`install`]s the one provider that
//! lists them. Transports thus stay free of any service wiring, and an
//! argument nobody provides for just completes nothing.
//!
//! [`Args`]/[`instructions`] are the `<bin> completion` command every
//! binary offers: it prints the line that enables completion in a shell.
//!
//! Not a port: completion is a terminal nicety, never behavior a test needs
//! to substitute (same reasoning as `remora-tui`).

use std::{ffi::OsStr, sync::OnceLock};

use clap_complete::engine::{
    ArgValueCandidates, ArgValueCompleter, CompletionCandidate, PathCompleter, ValueCompleter,
};

/// A kind of value worth completing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A context name (local, instant).
    Context,
    /// A device name, i.e. its serial (from the platform).
    Device,
    /// A release name (from the platform).
    Release,
    /// A deployment name (from the platform).
    Deployment,
    /// A local disk's device path (`/dev/sdb`), e.g. a flash target.
    Disk,
}

/// One completion: the value, and a short description shells may show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub value: String,
    pub help: Option<String>,
}

impl Candidate {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            help: None,
        }
    }

    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

type Provider = Box<dyn Fn(Kind) -> Vec<Candidate> + Send + Sync>;

static PROVIDER: OnceLock<Provider> = OnceLock::new();

/// Sets what lists the values of each [`Kind`]; call once, before
/// `CompleteEnv::complete`. A provider must never fail loudly or hang: on
/// any error it returns nothing, since a shell waits on it at every Tab.
pub fn install(provider: impl Fn(Kind) -> Vec<Candidate> + Send + Sync + 'static) {
    let _ = PROVIDER.set(Box::new(provider));
}

fn provided(kind: Kind) -> Vec<Candidate> {
    PROVIDER
        .get()
        .map(|provider| provider(kind))
        .unwrap_or_default()
}

fn to_clap(candidate: Candidate) -> CompletionCandidate {
    CompletionCandidate::new(candidate.value).help(candidate.help.map(Into::into))
}

/// Completes an argument with the values of `kind` (clap filters them by
/// what's typed so far).
pub fn values(kind: Kind) -> ArgValueCandidates {
    ArgValueCandidates::new(move || provided(kind).into_iter().map(to_clap).collect())
}

/// Completes scp operands: `DEVICE:` for the device side (devices from the
/// provider), local paths otherwise. Past the `:`, nothing: listing a
/// device's files would need a session.
pub fn scp_operand() -> ArgValueCompleter {
    ArgValueCompleter::new(ScpOperand)
}

struct ScpOperand;

impl ValueCompleter for ScpOperand {
    fn complete(&self, current: &OsStr) -> Vec<CompletionCandidate> {
        let typed = current.to_string_lossy();
        if typed.starts_with('-') || typed.contains(':') {
            return Vec::new();
        }
        // `user@DEVICE:` keeps its `user@`.
        let (user, host) = match typed.rsplit_once('@') {
            Some((user, host)) => (format!("{user}@"), host.to_owned()),
            None => (String::new(), typed.to_string()),
        };
        let mut candidates: Vec<_> = provided(Kind::Device)
            .into_iter()
            .filter(|device| {
                device
                    .value
                    .to_lowercase()
                    .starts_with(&host.to_lowercase())
            })
            .map(|device| {
                CompletionCandidate::new(format!("{user}{}:", device.value))
                    .help(device.help.map(Into::into))
            })
            .collect();
        if user.is_empty() {
            candidates.extend(PathCompleter::any().complete(current));
        }
        candidates
    }
}

/// Shells `<bin> completion` can set up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Powershell,
}

/// `<bin> completion`'s arguments.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The shell to set up (default: from $SHELL).
    #[arg(value_enum)]
    shell: Option<Shell>,
}

/// `<bin> completion`: says how to enable completion for `bin`, and prints
/// the line itself on stdout (for `<bin> completion zsh >> ~/.zshrc`).
/// Generated on shell start rather than written to a file, so it always
/// matches the installed binary (clap_complete's own advice, its protocol
/// being unstable). `completes` tells what Tab then offers. Returns the
/// exit code.
pub fn instructions(bin: &str, args: Args, completes: &str) -> i32 {
    let Some(shell) = args.shell.or_else(shell_from_env) else {
        remora_tui::warning(format!(
            "Can't tell your shell: pass it, e.g. `{bin} completion zsh`"
        ));
        return 1;
    };
    let (file, line) = enable_line(bin, shell);
    remora_tui::note(format!("Enable completion: add this line to {file}"), &line);
    remora_tui::step(format!("Then open a new shell. {completes}"));
    println!("{line}");
    0
}

fn shell_from_env() -> Option<Shell> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    match shell.rsplit('/').next().unwrap_or_default() {
        "bash" => Some(Shell::Bash),
        "zsh" => Some(Shell::Zsh),
        "fish" => Some(Shell::Fish),
        "elvish" => Some(Shell::Elvish),
        _ if cfg!(windows) => Some(Shell::Powershell),
        _ => None,
    }
}

/// The startup file to edit, and the line enabling `bin`'s completion there.
fn enable_line(bin: &str, shell: Shell) -> (&'static str, String) {
    match shell {
        Shell::Bash => ("~/.bashrc", format!("source <(COMPLETE=bash {bin})")),
        Shell::Zsh => ("~/.zshrc", format!("source <(COMPLETE=zsh {bin})")),
        Shell::Fish => (
            "~/.config/fish/config.fish",
            format!("COMPLETE=fish {bin} | source"),
        ),
        Shell::Elvish => (
            "~/.config/elvish/rc.elv",
            format!("eval (E:COMPLETE=elvish {bin} | slurp)"),
        ),
        Shell::Powershell => (
            "$PROFILE",
            format!(
                "$env:COMPLETE = \"powershell\"; {bin} | Out-String | Invoke-Expression; Remove-Item Env:\\COMPLETE"
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scp_operands_offer_devices_then_paths() {
        install(|kind| match kind {
            Kind::Device => vec![
                Candidate::new("525400C0FFEE"),
                Candidate::new("B827EB9D6166"),
            ],
            _ => vec![],
        });
        let values = |typed: &str| -> Vec<String> {
            ScpOperand
                .complete(OsStr::new(typed))
                .iter()
                .map(|c| c.get_value().to_string_lossy().into_owned())
                .collect()
        };
        assert!(values("52").contains(&"525400C0FFEE:".to_owned()));
        assert_eq!(values("root@b8"), ["root@B827EB9D6166:"]);
        assert!(values("525400C0FFEE:/da").is_empty());
        assert!(values("-r").is_empty());
    }

    #[test]
    fn enable_lines_name_the_binary() {
        assert_eq!(
            enable_line("remora-etcher", Shell::Zsh),
            (
                "~/.zshrc",
                "source <(COMPLETE=zsh remora-etcher)".to_owned()
            )
        );
        assert_eq!(
            enable_line("rmra", Shell::Powershell).1,
            "$env:COMPLETE = \"powershell\"; rmra | Out-String | Invoke-Expression; Remove-Item Env:\\COMPLETE"
        );
    }
}
