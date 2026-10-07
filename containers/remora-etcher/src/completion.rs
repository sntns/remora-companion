//! The values remora-etcher's dynamic completion offers (see
//! remora-completion): contexts and roles (the same as rmra's), local
//! disks, for the arguments naming a flash target, and releases, for
//! `flash --release`. Paths complete on their own.
//!
//! Runs inside the shell's Tab: it must be fast and never fail loudly.
//! Releases come from the platform: cached per context for a minute, the
//! call cut at a few seconds, a failure falling back to the last answer.

use std::time::Duration;

use remora_completion::{Cache, Candidate, Kind};
use remora_format::human_size;

use crate::bootstrap;

const FRESH: Duration = Duration::from_secs(60);
const PATIENCE: Duration = Duration::from_secs(3);

pub fn provide(kind: Kind) -> Vec<Candidate> {
    if !matches!(
        kind,
        Kind::Disk | Kind::Context | Kind::Role | Kind::Release
    ) {
        return Vec::new();
    }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return Vec::new();
    };
    runtime.block_on(async {
        let services = bootstrap::wire().await;
        use remora_context_application_transport_cli as context;
        match kind {
            Kind::Context => return context::complete_contexts(&services.context).await,
            Kind::Role => {
                use clap::CommandFactory;
                let over = context::context_on_command_line(crate::Options::command());
                return context::complete_roles(&services.context, over.as_ref()).await;
            }
            Kind::Release => return releases(&services).await,
            _ => {}
        }
        let Ok(disks) = services.disk.list().await else {
            return Vec::new();
        };
        // Never the system disk (flash refuses it anyway), nor loop devices
        // (snaps, mostly: noise); removable disks first, they're what gets
        // flashed.
        let (removable, fixed): (Vec<_>, Vec<_>) = disks
            .into_iter()
            .filter(|disk| {
                !disk.is_system_disk && !disk.path.to_string_lossy().starts_with("/dev/loop")
            })
            .partition(|disk| disk.is_removable);
        removable
            .into_iter()
            .chain(fixed)
            .map(|disk| {
                let mut help = human_size(disk.size_bytes);
                if let Some(model) = &disk.model {
                    help.push_str(&format!(" {model}"));
                }
                if !disk.is_removable {
                    help.push_str(" (not removable)");
                }
                Candidate::new(disk.path.display().to_string()).help(help)
            })
            .collect()
    })
}

async fn releases(services: &bootstrap::Services) -> Vec<Candidate> {
    use clap::CommandFactory;
    let over = remora_context_application_transport_cli::context_on_command_line(
        crate::Options::command(),
    );
    // Per context and role: a role of another tenant has other releases.
    let Ok(resolved) = services.context.resolve(over.as_ref()).await else {
        return Vec::new();
    };
    let scope = match &resolved.role {
        Some(role) => format!("{}.as-{}", resolved.context.name, role.urn),
        None => resolved.context.name.clone(),
    };
    let cache = Cache::new("remora-etcher", &scope, Kind::Release);
    if let Some(cached) = cache.fresh(FRESH) {
        return cached;
    }
    let labels = Default::default();
    let fetch = services.ota.list_releases(over.as_ref(), &labels);
    match tokio::time::timeout(PATIENCE, fetch).await {
        Ok(Ok(releases)) => {
            let values: Vec<_> = releases
                .into_iter()
                .map(|release| Candidate::new(release.name).help(release.version))
                .collect();
            cache.store(&values);
            values
        }
        _ => cache.last().unwrap_or_default(),
    }
}
