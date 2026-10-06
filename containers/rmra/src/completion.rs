//! The values `rmra`'s dynamic completion offers (see remora-completion).
//!
//! Runs inside the shell's Tab: it must be fast and never fail loudly.
//! Contexts are local, so always fresh; devices, releases and deployments
//! come from the platform, so they're cached per context for a minute, the
//! call is cut at a few seconds, and a failure falls back to the last
//! answer, or to nothing.

use std::time::Duration;

use clap::CommandFactory;
use remora_completion::{Cache, Candidate, Kind};
use remora_context::model::ContextOverride;
use remora_context_application_transport_cli::{
    complete_contexts, complete_roles, context_on_command_line,
};

use crate::{
    bootstrap::{self, Services},
    Options, PROGRAM,
};

const FRESH: Duration = Duration::from_secs(60);
const PATIENCE: Duration = Duration::from_secs(3);

pub fn provide(kind: Kind) -> Vec<Candidate> {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return Vec::new();
    };
    runtime.block_on(async move {
        let services = bootstrap::wire().await;
        let over = context_on_command_line(Options::command());
        match kind {
            Kind::Context => complete_contexts(&services.context).await,
            Kind::Role => complete_roles(&services.context, over.as_ref()).await,
            // No rmra argument takes a local disk.
            Kind::Disk => Vec::new(),
            remote => remote_values(&services, over.as_ref(), remote).await,
        }
    })
}

async fn remote_values(
    services: &Services,
    over: Option<&ContextOverride>,
    kind: Kind,
) -> Vec<Candidate> {
    // Per context and role: a role of another tenant has other devices.
    let Ok(resolved) = services.context.resolve(over).await else {
        return Vec::new();
    };
    let scope = match &resolved.role {
        Some(role) => format!("{}.as-{}", resolved.context.name, role.urn),
        None => resolved.context.name.clone(),
    };
    let cache = Cache::new(PROGRAM, &scope, kind);
    if let Some(cached) = cache.fresh(FRESH) {
        return cached;
    }
    match tokio::time::timeout(PATIENCE, fetch(services, over, kind)).await {
        Ok(Some(values)) => {
            cache.store(&values);
            values
        }
        _ => cache.last().unwrap_or_default(),
    }
}

async fn fetch(
    services: &Services,
    over: Option<&ContextOverride>,
    kind: Kind,
) -> Option<Vec<Candidate>> {
    Some(match kind {
        Kind::Context | Kind::Disk | Kind::Role => return None,
        Kind::Device => services
            .device
            .list(over, &Default::default(), false)
            .await
            .ok()?
            .into_iter()
            .map(|device| Candidate::new(device.name))
            .collect(),
        Kind::Release => services
            .ota
            .list_releases(over, &Default::default())
            .await
            .ok()?
            .into_iter()
            .map(|release| Candidate::new(release.name).help(release.version))
            .collect(),
        Kind::Deployment => services
            .ota
            .list_deployments(over, &Default::default())
            .await
            .ok()?
            .into_iter()
            .map(|deployment| Candidate::new(deployment.name).help(deployment.status.to_string()))
            .collect(),
    })
}
