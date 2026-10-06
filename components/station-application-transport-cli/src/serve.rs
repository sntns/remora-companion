use std::path::Path;

use error_stack::{Report, ResultExt};
use remora_context::model::ContextOverride;
use remora_station::{
    application::StationService,
    model::{BoardPolicy, ConfirmMode},
};
use remora_tui as tui;

use crate::{
    config::{self, Overrides, StationFile},
    console::{self, Line},
    error::{Error, Result},
    service::ServeArgs,
};

pub(crate) async fn run(
    args: ServeArgs,
    service: &StationService,
    over: Option<&ContextOverride>,
) -> Result<()> {
    let (file, base) = match &args.config {
        Some(path) => (
            config::read(path)?,
            path.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
                .to_path_buf(),
        ),
        None => (StationFile::default(), Path::new(".").to_path_buf()),
    };
    let (listen, config) = config::resolve(
        file,
        &base,
        Overrides {
            listen: args.listen,
            journal: args.journal,
            hooks: args.hooks,
            max_claims: args.max_claims,
            confirm: args.confirm,
            presence_timeout: args.presence_timeout,
            hook_timeout: args.hook_timeout,
            serial_policies: args.serial_policies,
            device_names: args.device_names,
        },
    )?;

    tui::intro("remora station");
    if config.boards.is_empty() {
        tui::warning("No board is configured: every claim will be refused");
    }
    if config.confirm == ConfirmMode::Key {
        tui::warning(
            "confirm: key -- Enter alone validates a label, nothing checks it is on the right \
             hub. For bench work only, never production.",
        );
    }
    let boards: Vec<String> = config
        .boards
        .iter()
        .map(|(board, policy)| match policy {
            BoardPolicy::SerialNumberPolicy(policy) => format!("{board} (policy {policy})"),
            BoardPolicy::DeviceName(template) => format!("{board} (device-name {template})"),
        })
        .collect();
    let (journal, hooks) = (config.journal.clone(), config.hooks.clone());

    // Everything is checked before listening: a misconfigured station
    // never answers a hub.
    let summary = service
        .start(config, over.cloned())
        .await
        .change_context(Error::Start)?;
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .change_context(Error::Listen(listen))?;
    tui::note(
        "Station ready",
        format!(
            "listening  http://{listen}\ncontext    {}\njournal    {}\nhooks      {}\nboards     {}\nrestored   {} claims, {} waiting for their label",
            tui::accent(&summary.context),
            journal.display(),
            hooks.display(),
            boards.join(", "),
            summary.restored,
            summary.awaiting_label,
        ),
    );
    tui::info(
        "Scan the label stuck on the hub whose LED is steady. Commands, then Enter: \
         r reprint · s skip · f force · q quit",
    );

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let mut server = tokio::spawn(remora_station_application_transport_http::serve(
        listener,
        service.clone(),
        async {
            let _ = stopped.await;
        },
    ));

    // The labelling console: the operator's lines, the queue's clock, and
    // Ctrl-C, until `q`.
    let interrupted = remora_progress::cancelled_by_ctrl_c();
    let mut lines = console::lines();
    let mut reading = true;
    let mut ticker = tokio::time::interval(summary.tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut served = None;
    let outcome = loop {
        let step = tokio::select! {
            line = lines.recv(), if reading => match line.as_deref().map(console::parse) {
                None => {
                    reading = false;
                    Ok(())
                }
                Some(Line::Quit) => break Ok(()),
                Some(Line::Input(input)) => service.handle(input).await,
            },
            _ = ticker.tick() => service.tick().await,
            () = interrupted.cancelled() => break Ok(()),
            done = &mut server => {
                served = Some(done);
                break Ok(());
            }
        };
        if let Err(report) = step {
            break Err(report.change_context(Error::Operate));
        }
    };

    // Devices first: once nothing can reach the station, its hooks get
    // their time and the journal is flushed.
    let served = match served {
        Some(served) => served,
        None => {
            let _ = stop.send(());
            server.await
        }
    };
    let stopped = service.shutdown().await.change_context(Error::Operate);
    tui::outro("Station stopped");
    match served {
        Ok(served) => served.change_context(Error::Serve)?,
        Err(join) => return Err(Report::new(join).change_context(Error::Serve)),
    }
    outcome.and(stopped)
}
