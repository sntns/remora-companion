use std::path::PathBuf;

/// A path under the OS temp directory, unique for this process — collisions
/// only across concurrent processes/threads racing on the exact same
/// nanosecond-scale counter value are not a concern this needs to defend
/// against (a build tool, not a security boundary).
///
/// Plain function rather than a DI-injected port: which directory scratch
/// files live in isn't a business decision any test needs to substitute —
/// tests that touch this already want a real temp path, same as production.
pub fn unique_path(prefix: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path
}
