use remora_tui as tui;
use remora_update::{
    application::UpdateService,
    model::{App, Installation},
};

use crate::error::{update_error, Result};

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

/// `<app> update`: install the latest public release over the running
/// binary, the way it was installed.
pub async fn run(args: Args, service: &UpdateService, app: &App) -> Result<()> {
    let spinner = tui::Spinner::start("Checking for updates");
    let check = match service.check(app).await {
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
    let how = match &check.installation {
        Installation::Homebrew { formula } => format!("brew upgrade {formula}"),
        _ => format!("{} update", app.name),
    };
    if args.check {
        spinner.done(format!(
            "{} {} is available {}",
            app.name,
            tui::accent(&check.latest.version),
            tui::dim(format!("(you have {})", check.current))
        ));
        tui::step(format!("Update with {}", tui::accent(how)));
        return Ok(());
    }
    if let Installation::Homebrew { formula } = &check.installation {
        spinner.done(format!(
            "{} {} is available {}",
            app.name,
            tui::accent(&check.latest.version),
            tui::dim(format!("(you have {})", check.current))
        ));
        tui::step(format!(
            "Installed with Homebrew: update with {}",
            tui::accent(format!("brew upgrade {formula}"))
        ));
        return Ok(());
    }
    spinner.set_message(format!(
        "Installing {} {}",
        app.name,
        tui::accent(&check.latest.version)
    ));
    match service.update(app, &check, args.force).await {
        Ok(()) => {
            spinner.done(format!(
                "Updated {} {} → {}",
                app.name,
                check.current,
                tui::accent(&check.latest.version)
            ));
            Ok(())
        }
        Err(report) => {
            drop(spinner);
            Err(update_error(report))
        }
    }
}
