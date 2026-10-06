use error_stack::Report;
use remora_tui as tui;
use remora_update::{
    application::{self, UpdateService},
    model::{App, Installation, Version},
};

use crate::error::{update_error, Error, Result};

#[derive(clap::Args)]
pub struct Args {
    /// Only say whether a newer release exists.
    #[arg(long)]
    check: bool,
    /// Also replace a binary that its installer didn't put there (built from
    /// source, copied by hand...). Never a Homebrew one.
    #[arg(long)]
    force: bool,
}

/// `<name> update`: install the latest public release over the running
/// binary, the way it was installed. `version` is the binary's own
/// `CARGO_PKG_VERSION`.
pub async fn run(args: Args, service: &UpdateService, name: &str, version: &str) -> Result<()> {
    let app = running_app(name, version)?;
    let spinner = tui::Spinner::start("Checking for updates");
    let check = match service.check(&app).await {
        Ok(check) => check,
        Err(report) => {
            drop(spinner);
            return Err(update_error(report));
        }
    };
    if !check.available() {
        spinner.done(format!(
            "{} {} is up to date",
            app.name,
            tui::accent(&check.current)
        ));
        return Ok(());
    }
    let available = format!(
        "{} {} is available {}",
        app.name,
        tui::accent(&check.latest.version),
        tui::dim(format!("(you have {})", check.current))
    );
    if args.check {
        spinner.done(available);
        // What `update` itself would accept for this installation, so the
        // suggestion works when followed.
        let how = match &check.installation {
            Installation::Homebrew { formula } => format!("brew upgrade {formula}"),
            Installation::Unmanaged { .. } => format!("{} update --force", app.name),
            Installation::Installer { .. } => format!("{} update", app.name),
        };
        tui::step(format!("Update with {}", tui::accent(how)));
        return Ok(());
    }
    spinner.set_message(format!(
        "Installing {} {}",
        app.name,
        tui::accent(&check.latest.version)
    ));
    match service.update(&app, &check, args.force).await {
        Ok(()) => {
            spinner.done(format!(
                "Updated {} {} → {}",
                app.name,
                check.current,
                tui::accent(&check.latest.version)
            ));
            Ok(())
        }
        // Not a failure: the use case defers to brew, which owns this
        // binary; say so and how.
        Err(report) => match report.current_context() {
            application::Error::Homebrew(formula) => {
                spinner.done(available);
                tui::step(format!(
                    "Installed with Homebrew: update with {}",
                    tui::accent(format!("brew upgrade {formula}"))
                ));
                Ok(())
            }
            _ => {
                drop(spinner);
                Err(update_error(report))
            }
        },
    }
}

/// The running binary, as the update use case sees it. The release
/// process keeps `CARGO_PKG_VERSION` semver, but a local build may not.
fn running_app(name: &str, version: &str) -> Result<App> {
    let version =
        Version::parse(version).ok_or_else(|| Report::new(Error::Version(version.to_owned())))?;
    Ok(App {
        name: name.to_owned(),
        version,
        executable: std::env::current_exe().unwrap_or_else(|_| name.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local build off a non-release version reports it, never panics.
    #[test]
    fn a_non_release_version_is_an_error() {
        let report = running_app("rmra", "dev").unwrap_err();
        assert!(matches!(report.current_context(), Error::Version(v) if v == "dev"));
        assert_eq!(running_app("rmra", "0.8.0").unwrap().name, "rmra");
    }
}
