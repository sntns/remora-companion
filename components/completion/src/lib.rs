//! Dynamic shell completion (`COMPLETE=$SHELL rmra`, clap_complete's
//! environment-driven engine), shared by the CLI transport crates.
//!
//! A transport marks which arguments take which [`Kind`] of value
//! (`#[arg(add = remora_completion::values(Kind::Device))]`); the binary,
//! which is what can reach the services, [`install`]s the one provider that
//! lists them. Transports thus stay free of any service wiring, and an
//! argument nobody provides for just completes nothing.
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
}
