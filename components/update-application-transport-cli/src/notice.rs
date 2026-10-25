use std::time::{Duration, Instant};

use remora_tui as tui;
use remora_update::{
    application::UpdateService,
    model::{Installation, UpdateCheck},
};
use tokio::task::JoinHandle;

use crate::service::running_app;

/// How often the release feed is asked.
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// How long a command's end may wait for a look still under way: a quick
/// command never waits much, and a slow feed is simply asked next time.
const PATIENCE: Duration = Duration::from_millis(1500);

/// Telling the user, once a command is done, that a newer release of the
/// binary exists: looked up alongside the command (at most daily), shown
/// after it on stderr, never in its way.
pub struct Notice {
    look: Option<(JoinHandle<Option<UpdateCheck>>, Instant)>,
    name: String,
}

impl Notice {
    /// Starts looking, unless nobody would see the notice or it would be in
    /// the way: stderr not a terminal (scripts, CI, pipes), `CI` set, or
    /// `<NAME>_NO_UPDATE_CHECK` set (`RMRA_NO_UPDATE_CHECK`). A build whose
    /// version is not a release's never looks either.
    pub fn start(service: &UpdateService, name: &str, version: &str) -> Self {
        let quiet = Self {
            look: None,
            name: name.to_owned(),
        };
        if !tui::decorated() || opted_out(name, |var| std::env::var_os(var)) {
            return quiet;
        }
        let Ok(app) = running_app(name, version) else {
            return quiet;
        };
        let service = service.clone();
        let look = tokio::spawn(async move { service.notice(&app, EVERY).await.ok().flatten() });
        Self {
            look: Some((look, Instant::now())),
            ..quiet
        }
    }

    /// Tells about a newer release, if the look found one in time.
    pub async fn show(self) {
        let Some((mut look, started)) = self.look else {
            return;
        };
        let left = PATIENCE.saturating_sub(started.elapsed());
        let check = match tokio::time::timeout(left, &mut look).await {
            Ok(Ok(Some(check))) => check,
            Ok(_) => return,
            Err(_) => {
                look.abort();
                return;
            }
        };
        tui::info(format!(
            "{} {} is available {}: update with {}",
            self.name,
            tui::accent(&check.latest.version),
            tui::dim(format!("(you have {})", check.current)),
            tui::accent(how(&self.name, &check.installation))
        ));
    }
}

/// What updates this installation, as `update` itself would accept it.
pub(crate) fn how(name: &str, installation: &Installation) -> String {
    match installation {
        Installation::Homebrew { formula } => format!("brew upgrade {formula}"),
        Installation::Unmanaged { .. } => format!("{name} update --force"),
        Installation::Installer { .. } => format!("{name} update"),
    }
}

/// Whether the user asked for no update notice: `CI`, or the binary's own
/// `<NAME>_NO_UPDATE_CHECK`, set to anything but empty, `0` or `false`.
fn opted_out(name: &str, var: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    let own = format!(
        "{}_NO_UPDATE_CHECK",
        name.to_ascii_uppercase().replace('-', "_")
    );
    ["CI", own.as_str()].iter().any(|key| {
        var(key).is_some_and(|value| !matches!(value.to_str(), Some("" | "0" | "false")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opting_out() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            }
        };
        assert!(!opted_out("rmra", env(&[])));
        assert!(opted_out("rmra", env(&[("RMRA_NO_UPDATE_CHECK", "1")])));
        assert!(!opted_out("rmra", env(&[("RMRA_NO_UPDATE_CHECK", "0")])));
        assert!(opted_out(
            "remora-etcher",
            env(&[("REMORA_ETCHER_NO_UPDATE_CHECK", "true")])
        ));
        assert!(opted_out("rmra", env(&[("CI", "true")])));
        assert!(!opted_out("rmra", env(&[("CI", "false")])));
    }
}
