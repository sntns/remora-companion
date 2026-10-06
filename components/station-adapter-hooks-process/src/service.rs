use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
};

use error_stack::{Report, ResultExt};
use remora_factory::adapter::provisioning::ProvisionedIdentity;
use remora_station::{
    adapter::hooks::{
        ClaimRecord, Error, HookInvocation, HookOutcome, HookRun, HookRunnerAdapter, Result,
    },
    model::{BoardPolicy, ClaimState, LabelState},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

/// How much of a script's stdout and stderr is kept, each: enough for any
/// message, without letting a chatty script bloat the journal.
const OUTPUT_LIMIT: usize = 64 * 1024;

/// Runs `<dir>/<event>.d/*` like `run-parts`: in lexical order, one after
/// the other, each with the event as `argv[1]`, the claim as JSON on stdin
/// and as `REMORA_*` variables. On unix only executable files run; on
/// Windows, `.exe`, `.cmd`, `.bat`, and `.ps1` through `powershell -File`.
/// Hidden files, `*~` and `*.disabled` never run. A script running past
/// the timeout is killed.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessHookRunnerImpl;

#[async_trait::async_trait]
impl HookRunnerAdapter for ProcessHookRunnerImpl {
    async fn check(&self, dir: &Path) -> Result<()> {
        tokio::fs::read_dir(dir)
            .await
            .map(drop)
            .change_context_lazy(|| Error::Unreadable(dir.to_path_buf()))
    }

    async fn run(&self, run: &HookRun) -> Result<Vec<HookOutcome>> {
        let directory = run.invocation.event.directory();
        let dir = run.dir.join(&directory);
        let mut outcomes = Vec::new();
        for (name, script) in scripts(&dir).await? {
            let hook = format!("{directory}/{}", name.to_string_lossy());
            outcomes.push(run_one(hook, &script, run).await);
        }
        Ok(outcomes)
    }
}

/// The scripts to run, by name, sorted; none without the directory.
async fn scripts(dir: &Path) -> Result<Vec<(OsString, PathBuf)>> {
    let unreadable = || Error::Unreadable(dir.to_path_buf());
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Report::new(error).change_context(unreadable())),
    };
    let mut scripts = Vec::new();
    while let Some(entry) = entries.next_entry().await.change_context_lazy(unreadable)? {
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if text.starts_with('.') || text.ends_with('~') || text.ends_with(".disabled") {
            continue;
        }
        // Follows a symlink, like run-parts: what matters is what runs.
        let Ok(metadata) = tokio::fs::metadata(entry.path()).await else {
            continue;
        };
        if metadata.is_file() && runnable(&entry.path(), &metadata) {
            scripts.push((name, entry.path()));
        }
    }
    scripts.sort();
    Ok(scripts)
}

#[cfg(unix)]
fn runnable(_path: &Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn runnable(path: &Path, _metadata: &std::fs::Metadata) -> bool {
    matches!(
        extension(path).as_deref(),
        Some("exe" | "cmd" | "bat" | "ps1")
    )
}

#[cfg(not(unix))]
fn extension(path: &Path) -> Option<String> {
    path.extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
}

fn command(script: &Path, event: &str) -> Command {
    #[cfg(not(unix))]
    if extension(script).as_deref() == Some("ps1") {
        let mut command = Command::new("powershell");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(script)
            .arg(event);
        return command;
    }
    let mut command = Command::new(script);
    command.arg(event);
    command
}

async fn run_one(hook: String, script: &Path, run: &HookRun) -> HookOutcome {
    let invocation = &run.invocation;
    let event = invocation.event.name();
    let mut command = command(script, event);
    command
        .envs(environment(invocation))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let failed = |stderr: String, timed_out| HookOutcome {
        hook: hook.clone(),
        exit: None,
        timed_out,
        stdout: String::new(),
        stderr,
    };
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return failed(
                format!("failed to start {}: {error}", script.display()),
                false,
            )
        }
    };
    // Written alongside the wait: a script that doesn't read its stdin
    // must neither block on it nor be blocked by it.
    if let Some(mut stdin) = child.stdin.take() {
        let record = record(invocation).to_string();
        tokio::spawn(async move {
            let _ = stdin.write_all(record.as_bytes()).await;
        });
    }
    // Both pipes are drained alongside the wait (a script filling one must
    // never block on it), keeping only the first OUTPUT_LIMIT bytes of
    // each: a chatty script can't make the station buffer it all.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let finished = async { tokio::join!(bounded(stdout), bounded(stderr), child.wait()) };
    match tokio::time::timeout(run.timeout, finished).await {
        Ok((stdout, stderr, Ok(status))) => HookOutcome {
            hook,
            exit: status.code(),
            timed_out: false,
            stdout,
            stderr,
        },
        Ok((_, _, Err(error))) => failed(
            format!("failed to wait for {}: {error}", script.display()),
            false,
        ),
        // Dropping the child kills it.
        Err(_) => failed(format!("killed after {}s", run.timeout.as_secs_f32()), true),
    }
}

/// What `pipe` carries, up to OUTPUT_LIMIT bytes (marked when cut); the
/// rest is read and dropped until the script closes it.
async fn bounded(pipe: Option<impl AsyncRead + Unpin>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut kept = Vec::new();
    let _ = (&mut pipe)
        .take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut kept)
        .await;
    let truncated = kept.len() > OUTPUT_LIMIT;
    kept.truncate(OUTPUT_LIMIT);
    if truncated {
        let _ = tokio::io::copy(&mut pipe, &mut tokio::io::sink()).await;
    }
    let mut text = String::from_utf8_lossy(&kept).into_owned();
    if truncated {
        text.push_str("\n[truncated]");
    }
    text
}

fn claim_state(state: Option<ClaimState>) -> &'static str {
    match state {
        Some(ClaimState::Issued) => "issued",
        Some(ClaimState::Installed) => "installed",
        Some(ClaimState::Failed) | None => "failed",
    }
}

fn label_state(label: Option<LabelState>) -> Option<&'static str> {
    label.map(|label| match label {
        LabelState::Queued => "queued",
        LabelState::Active => "active",
        LabelState::Labelled => "labelled",
    })
}

fn policy_parts(policy: &BoardPolicy) -> (Option<&str>, Option<&str>) {
    match policy {
        BoardPolicy::SerialNumberPolicy(policy) => (Some(policy), None),
        BoardPolicy::DeviceName(template) => (None, Some(template.source())),
    }
}

/// The `REMORA_*` variables; empty rather than unset when unknown, so a
/// script under `set -u` still runs.
fn environment(invocation: &HookInvocation) -> Vec<(&'static str, String)> {
    let claim = &invocation.claim;
    let hardware = &claim.hardware;
    let identity = claim.identity.as_ref();
    let issued = |field: fn(&ProvisionedIdentity) -> &str| {
        identity.map(field).unwrap_or_default().to_string()
    };
    let (policy, template) = policy_parts(&claim.policy);
    vec![
        ("REMORA_EVENT", invocation.event.name().to_string()),
        ("REMORA_CLAIM_ID", claim.claim_id.to_string()),
        ("REMORA_SERIAL", issued(|i| &i.serial_number)),
        (
            "REMORA_FACTORY_DEVICE_NAME",
            issued(|i| &i.factory_device_name),
        ),
        // The device's hostname once it reboots: its serial, as is.
        ("REMORA_HOSTNAME", issued(|i| &i.serial_number)),
        ("REMORA_TEMP_HOSTNAME", hardware.temp_hostname.clone()),
        ("REMORA_BOARD", hardware.board.clone()),
        (
            "REMORA_ETH_MAC",
            hardware.eth_mac.clone().unwrap_or_default(),
        ),
        (
            "REMORA_BSP_SERIAL",
            hardware.bsp_serial.clone().unwrap_or_default(),
        ),
        (
            "REMORA_MACHINE_ID",
            hardware.machine_id.clone().unwrap_or_default(),
        ),
        (
            "REMORA_IMAGE_VERSION",
            claim.image.version.clone().unwrap_or_default(),
        ),
        (
            "REMORA_IMAGE_COMPATIBLE",
            claim.image.compatible.clone().unwrap_or_default(),
        ),
        ("REMORA_ACCESS_URL", issued(|i| &i.access_url)),
        (
            "REMORA_SERIAL_POLICY",
            policy.unwrap_or_default().to_string(),
        ),
        (
            "REMORA_DEVICE_NAME_TEMPLATE",
            template.unwrap_or_default().to_string(),
        ),
        ("REMORA_CONTEXT", invocation.context.clone()),
        ("REMORA_ATTEMPT", invocation.attempt.to_string()),
        ("REMORA_JOURNAL", invocation.journal.display().to_string()),
    ]
}

/// The claim on stdin: the request, the identity without its
/// certificates, and where it stands.
fn record(invocation: &HookInvocation) -> serde_json::Value {
    let ClaimRecord {
        claim_id,
        hardware,
        image,
        policy,
        identity,
        state,
        label,
        reason,
    } = &invocation.claim;
    let (serial_policy, device_name_template) = policy_parts(policy);
    serde_json::json!({
        "event": invocation.event.name(),
        "attempt": invocation.attempt,
        "context": invocation.context,
        "claim_id": claim_id.to_string(),
        "state": claim_state(*state),
        "label": label_state(*label),
        "reason": reason,
        "hardware": {
            "board": hardware.board,
            "temp_hostname": hardware.temp_hostname,
            "eth_mac": hardware.eth_mac,
            "bsp_serial": hardware.bsp_serial,
            "machine_id": hardware.machine_id,
            "macs": hardware.macs,
        },
        "image": {
            "version": image.version,
            "compatible": image.compatible,
        },
        "serial_policy": serial_policy,
        "device_name_template": device_name_template,
        "identity": identity.as_ref().map(|identity| serde_json::json!({
            "serial_number": identity.serial_number,
            "factory_device_name": identity.factory_device_name,
            "key_id": identity.key_id,
            "access_url": identity.access_url,
        })),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, time::Duration};

    use remora_station::{
        adapter::hooks::HookEvent,
        model::{ClaimId, DeviceNameTemplate, HardwareInfo, ImageInfo},
    };

    use super::*;

    fn script(dir: &Path, name: &str, body: &str, executable: bool) {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn hook_run(root: &Path, event: HookEvent, timeout: Duration) -> HookRun {
        HookRun {
            dir: root.to_path_buf(),
            timeout,
            invocation: HookInvocation {
                event,
                claim: ClaimRecord {
                    claim_id: ClaimId::new("9f3c"),
                    hardware: HardwareInfo {
                        board: "hub-v2".into(),
                        temp_hostname: "e2b4a1c09f13".into(),
                        eth_mac: None,
                        bsp_serial: Some("c3d2".into()),
                        machine_id: Some("1abf".into()),
                        macs: BTreeMap::new(),
                    },
                    image: ImageInfo {
                        version: Some("1.4.0".into()),
                        compatible: Some("v2".into()),
                    },
                    policy: BoardPolicy::DeviceName(
                        DeviceNameTemplate::parse("{bsp_serial}").unwrap(),
                    ),
                    identity: Some(ProvisionedIdentity {
                        serial_number: "1H7Z".into(),
                        factory_device_name: "urn:test:factory-device:1H7Z".into(),
                        certificate_der: b"idevid".to_vec(),
                        certificate_authority_der: b"factory ca".to_vec(),
                        server_certificate_authority_der: b"server ca".to_vec(),
                        key_id: "urn:test:certificate-1H7Z-1".into(),
                        access_url: "https://access.test/access/v1".into(),
                    }),
                    state: Some(ClaimState::Issued),
                    label: Some(LabelState::Active),
                    reason: None,
                },
                context: "factory".into(),
                attempt: 2,
                journal: root.join("station.jsonl"),
            },
        }
    }

    #[tokio::test]
    async fn runs_executables_in_lexical_order_skipping_the_rest() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("label.d");
        std::fs::create_dir(&dir).unwrap();
        for name in [
            "20-second",
            "10-first",
            "30-third.disabled",
            "40-backup~",
            ".50-hidden",
        ] {
            script(&dir, name, "echo ran", true);
        }
        script(&dir, "15-not-executable", "echo ran", false);
        std::fs::create_dir(dir.join("17-a-directory")).unwrap();
        script(&dir, "25-fails", "echo oops >&2; exit 3", true);

        let outcomes = ProcessHookRunnerImpl
            .run(&hook_run(
                root.path(),
                HookEvent::Label,
                Duration::from_secs(10),
            ))
            .await
            .unwrap();
        let ran: Vec<_> = outcomes.iter().map(|o| (o.hook.as_str(), o.exit)).collect();
        assert_eq!(
            ran,
            [
                ("label.d/10-first", Some(0)),
                ("label.d/20-second", Some(0)),
                ("label.d/25-fails", Some(3)),
            ]
        );
        assert_eq!(outcomes[0].stdout, "ran\n");
        assert_eq!(outcomes[2].stderr, "oops\n");
        assert!(!outcomes[2].succeeded());

        // Another event's directory, and one that doesn't exist.
        let none = ProcessHookRunnerImpl
            .run(&hook_run(
                root.path(),
                HookEvent::Issued,
                Duration::from_secs(10),
            ))
            .await
            .unwrap();
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn hands_the_claim_over_as_arguments_environment_and_stdin() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("label.d");
        std::fs::create_dir(&dir).unwrap();
        script(
            &dir,
            "10-env",
            r#"set -u
echo "$1|$REMORA_EVENT|$REMORA_CLAIM_ID|$REMORA_SERIAL|$REMORA_FACTORY_DEVICE_NAME|$REMORA_HOSTNAME"
echo "$REMORA_TEMP_HOSTNAME|$REMORA_BOARD|$REMORA_ETH_MAC|$REMORA_BSP_SERIAL|$REMORA_MACHINE_ID"
echo "$REMORA_IMAGE_VERSION|$REMORA_IMAGE_COMPATIBLE|$REMORA_ACCESS_URL"
echo "$REMORA_SERIAL_POLICY|$REMORA_DEVICE_NAME_TEMPLATE|$REMORA_CONTEXT|$REMORA_ATTEMPT|$REMORA_JOURNAL"
cat >&2"#,
            true,
        );
        let run = hook_run(root.path(), HookEvent::Label, Duration::from_secs(10));
        let outcomes = ProcessHookRunnerImpl.run(&run).await.unwrap();
        let outcome = &outcomes[0];
        assert!(outcome.succeeded(), "{outcome:?}");
        let journal = root.path().join("station.jsonl");
        assert_eq!(
            outcome.stdout,
            format!(
                "label|label|9f3c|1H7Z|urn:test:factory-device:1H7Z|1H7Z\n\
                 e2b4a1c09f13|hub-v2||c3d2|1abf\n\
                 1.4.0|v2|https://access.test/access/v1\n\
                 |{{bsp_serial}}|factory|2|{}\n",
                journal.display()
            )
        );
        let stdin: serde_json::Value = serde_json::from_str(&outcome.stderr).unwrap();
        assert_eq!(stdin["claim_id"], "9f3c");
        assert_eq!(stdin["label"], "active");
        assert_eq!(stdin["hardware"]["bsp_serial"], "c3d2");
        assert_eq!(stdin["identity"]["serial_number"], "1H7Z");
        assert!(
            stdin["identity"].get("certificate").is_none(),
            "no certificates for hooks"
        );
    }

    #[tokio::test]
    async fn kills_a_script_past_its_timeout_and_runs_the_next() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("issued.d");
        std::fs::create_dir(&dir).unwrap();
        script(&dir, "10-hangs", "exec sleep 30", true);
        script(&dir, "20-next", "echo next", true);
        let started = std::time::Instant::now();
        let outcomes = ProcessHookRunnerImpl
            .run(&hook_run(
                root.path(),
                HookEvent::Issued,
                Duration::from_millis(300),
            ))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(outcomes[0].timed_out);
        assert_eq!(outcomes[0].exit, None);
        assert_eq!(outcomes[1].stdout, "next\n");
    }

    #[tokio::test]
    async fn keeps_only_the_start_of_a_chatty_script() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("issued.d");
        std::fs::create_dir(&dir).unwrap();
        // Ten times the limit on stdout, and it still exits normally.
        script(
            &dir,
            "10-chatty",
            "head -c 655360 /dev/zero | tr '\\0' x; echo done >&2",
            true,
        );
        let outcomes = ProcessHookRunnerImpl
            .run(&hook_run(
                root.path(),
                HookEvent::Issued,
                Duration::from_secs(10),
            ))
            .await
            .unwrap();
        let outcome = &outcomes[0];
        assert!(outcome.succeeded(), "{:?}", outcome.stderr);
        assert_eq!(outcome.stdout.len(), OUTPUT_LIMIT + "\n[truncated]".len());
        assert!(outcome.stdout.ends_with("x\n[truncated]"));
        assert_eq!(outcome.stderr, "done\n");
    }

    #[tokio::test]
    async fn checks_that_the_hooks_directory_reads() {
        let root = tempfile::tempdir().unwrap();
        ProcessHookRunnerImpl.check(root.path()).await.unwrap();
        let missing = root.path().join("missing");
        let report = ProcessHookRunnerImpl.check(&missing).await.unwrap_err();
        assert!(matches!(report.current_context(), Error::Unreadable(path) if path == &missing));
    }
}
