//! The values `rmra`'s dynamic completion offers (see remora-completion).
//!
//! Runs inside the shell's Tab: it must be fast and never fail loudly.
//! Contexts are local, so always fresh; devices, releases and deployments
//! come from the platform, so they're cached per context for a minute, the
//! call is cut at a few seconds, and a failure falls back to the last
//! answer, or to nothing.

use std::{
    path::PathBuf,
    time::{Duration, SystemTime},
};

use remora_completion::{Candidate, Kind};
use remora_context::model::{ContextOverride, Selection};

use crate::bootstrap::{self, Services};

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
        let over = context_on_command_line();
        match kind {
            Kind::Context => contexts(&services).await,
            // No rmra argument takes a local disk.
            Kind::Disk => Vec::new(),
            remote => remote_values(&services, over.as_ref(), remote).await,
        }
    })
}

async fn contexts(services: &Services) -> Vec<Candidate> {
    services
        .context
        .list()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|summary| {
            let mut help = summary.context.endpoint.address.clone();
            if summary.current {
                help.push_str(" (current)");
            }
            Candidate::new(summary.context.name).help(help)
        })
        .collect()
}

async fn remote_values(
    services: &Services,
    over: Option<&ContextOverride>,
    kind: Kind,
) -> Vec<Candidate> {
    let Ok(Some((context, _))) = services.context.selected(over).await else {
        return Vec::new();
    };
    let cache = cache_path(&context, kind);
    if let Some(cached) = read_cache(&cache, true) {
        return cached;
    }
    match tokio::time::timeout(PATIENCE, fetch(services, over, kind)).await {
        Ok(Some(values)) => {
            write_cache(&cache, &values);
            values
        }
        _ => read_cache(&cache, false).unwrap_or_default(),
    }
}

async fn fetch(
    services: &Services,
    over: Option<&ContextOverride>,
    kind: Kind,
) -> Option<Vec<Candidate>> {
    Some(match kind {
        Kind::Context | Kind::Disk => return None,
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

/// `--context`/`-c` as typed on the line being completed (the shell hands
/// the whole line to rmra), else RMRA_CONTEXT, else the current context.
fn context_on_command_line() -> Option<ContextOverride> {
    let args: Vec<String> = std::env::args().collect();
    let mut found = None;
    for (index, arg) in args.iter().enumerate() {
        if let Some(value) = arg.strip_prefix("--context=") {
            found = Some(value.to_owned());
        } else if arg == "--context" || arg == "-c" {
            found = args.get(index + 1).cloned();
        } else if let Some(value) = arg.strip_prefix("-c").filter(|v| !v.is_empty()) {
            found = Some(value.to_owned());
        }
    }
    let (name, source) = match found.filter(|name| !name.is_empty()) {
        Some(name) => (name, Selection::Flag),
        None => (
            std::env::var("RMRA_CONTEXT")
                .ok()
                .filter(|name| !name.is_empty())?,
            Selection::Environment,
        ),
    };
    Some(ContextOverride { name, source })
}

fn cache_path(context: &str, kind: Kind) -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::home_dir().map(|home| home.join(".cache")))
    }
    .unwrap_or_else(std::env::temp_dir);
    base.join("rmra")
        .join("completion")
        .join(format!("{context}.{kind:?}").to_lowercase())
}

/// One `value\thelp` line per candidate; `fresh` only if young enough.
fn read_cache(path: &PathBuf, fresh: bool) -> Option<Vec<Candidate>> {
    if fresh {
        let age = SystemTime::now()
            .duration_since(std::fs::metadata(path).ok()?.modified().ok()?)
            .unwrap_or(Duration::MAX);
        if age > FRESH {
            return None;
        }
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(
        text.lines()
            .filter(|line| !line.is_empty())
            .map(|line| match line.split_once('\t') {
                Some((value, help)) if !help.is_empty() => Candidate::new(value).help(help),
                Some((value, _)) => Candidate::new(value),
                None => Candidate::new(line),
            })
            .collect(),
    )
}

fn write_cache(path: &PathBuf, values: &[Candidate]) {
    let text: String = values
        .iter()
        .map(|c| format!("{}\t{}\n", c.value, c.help.as_deref().unwrap_or_default()))
        .collect();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}
