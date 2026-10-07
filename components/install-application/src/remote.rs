//! The shell command lines run on the device, and how their output reads.

use std::collections::HashMap;

use remora_channel::model::shell_quote;

/// Where the bundle lives on the device: `<dir>/rmra-install-<sha>.raucb`,
/// named after its content so that a later run finds what an earlier one
/// left, and `.part` until it's whole and verified.
pub(crate) struct RemotePaths {
    pub dir: String,
    pub bundle: String,
    pub part: String,
}

impl RemotePaths {
    pub fn new(dir: &str, sha256: &str) -> Self {
        let dir = dir.trim_end_matches('/').to_owned();
        let bundle = format!(
            "{dir}/rmra-install-{}.raucb",
            &sha256[..16.min(sha256.len())]
        );
        let part = format!("{bundle}.part");
        Self { dir, bundle, part }
    }
}

/// Exit code of [`check`] when the device has no `remora-otactl`.
pub(crate) const NO_INSTALLER: i32 = 3;

/// What the device has: the installer, the bundle (whole or partial), its
/// boot and the slot it booted -- as `key value` lines.
pub(crate) fn check(paths: &RemotePaths) -> String {
    format!(
        "command -v remora-otactl >/dev/null 2>&1 || exit {NO_INSTALLER}; \
         mkdir -p {dir} || exit 4; \
         echo \"bundle $(stat -c %s {bundle} 2>/dev/null || echo -)\"; \
         echo \"part $(stat -c %s {part} 2>/dev/null || echo -)\"; \
         {state}",
        dir = shell_quote(&paths.dir),
        bundle = shell_quote(&paths.bundle),
        part = shell_quote(&paths.part),
        state = state(),
    )
}

/// The device's boot (its boot id, new at each boot) and booted slot.
pub(crate) fn state() -> String {
    "echo \"boot $(cat /proc/sys/kernel/random/boot_id 2>/dev/null)\"; \
     echo \"slot $(rauc status --output-format=shell 2>/dev/null \
     | sed -n 's/^RAUC_SYSTEM_BOOTED_BOOTNAME=//p' | tr -d \"'\\\"\")\""
        .to_owned()
}

/// The size of `path` on the device, or `-`.
pub(crate) fn size(path: &str) -> String {
    format!(
        "echo \"size $(stat -c %s {path} 2>/dev/null || echo -)\"",
        path = shell_quote(path)
    )
}

/// Appends stdin to `path`.
pub(crate) fn append(path: &str) -> String {
    format!("cat >> {}", shell_quote(path))
}

pub(crate) fn remove(path: &str) -> String {
    format!("rm -f {}", shell_quote(path))
}

pub(crate) fn sha256(path: &str) -> String {
    format!("sha256sum {} | cut -d' ' -f1", shell_quote(path))
}

pub(crate) fn rename(from: &str, to: &str) -> String {
    format!("mv -f {} {}", shell_quote(from), shell_quote(to))
}

pub(crate) fn install(path: &str) -> String {
    format!("remora-otactl install --no-reboot {}", shell_quote(path))
}

/// Reboots a moment after the command returns, detached from the session,
/// so that the session ends cleanly before the device goes down. `reboot`
/// is remora's wrapper, which honours the try-boot RAUC armed.
pub(crate) fn reboot() -> String {
    "nohup sh -c 'sleep 2; reboot' >/dev/null 2>&1 </dev/null &".to_owned()
}

pub(crate) fn validate() -> String {
    "remora-otactl validate".to_owned()
}

/// `key value` lines, as [`check`] and [`state`] print them; `-` or
/// nothing is no value.
pub(crate) fn fields(output: &str) -> HashMap<String, String> {
    output
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ')?;
            let value = value.trim();
            (!value.is_empty() && value != "-").then(|| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// RAUC's progress as remora-otactl draws it: `[████░░░░] 45% Copying…`,
/// read back as the percentage and what follows it.
pub(crate) fn rauc_progress(line: &str) -> Option<(u64, String)> {
    let at = line.find('%')?;
    let digits: String = line[..at]
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let percent = digits.parse::<u64>().ok().filter(|p| *p <= 100)?;
    Some((percent, line[at + 1..].trim().to_owned()))
}

/// What of `remora-otactl`'s output is worth relaying: not its tracing
/// lines below WARN (a WARN/ERROR keeps its message, without timestamp and
/// target), not the copies its in-place redraws leave (`✓ step`, `step
/// done.`, its spinner's `--%`), and not its advice to reboot by hand --
/// this command reboots on its own.
pub(crate) fn otactl_line(line: &str) -> Option<String> {
    let line = line.trim();
    let mut words = line.split_whitespace();
    let (first, level) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
    let timestamped = first.len() > 10 && first.as_bytes()[4] == b'-' && first.contains('T');
    if timestamped {
        return match level {
            "WARN" | "ERROR" => {
                let rest = line[line.find(level).unwrap_or(0) + level.len()..].trim();
                let message = rest.split_once(": ").map_or(rest, |(_, message)| message);
                Some(format!("{level}: {message}"))
            }
            _ => None,
        };
    }
    let noise = line.is_empty()
        || line.starts_with("✓ ")
        || line.ends_with(" done.")
        || line.contains("--%")
        || line.to_ascii_lowercase().contains("reboot manually")
        || line.contains("reboot skipped");
    (!noise).then(|| line.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_skip_missing_values() {
        let fields = fields("bundle -\npart 4096\nboot abc\nslot \nnoise");
        assert_eq!(fields.get("part").map(String::as_str), Some("4096"));
        assert_eq!(fields.get("boot").map(String::as_str), Some("abc"));
        assert!(!fields.contains_key("bundle"));
        assert!(!fields.contains_key("slot"));
    }

    #[test]
    fn rauc_progress_is_read_off_otactls_bar() {
        assert_eq!(
            rauc_progress("[████░░░░]  45% Copying image to bootimg.1"),
            Some((45, "Copying image to bootimg.1".into()))
        );
        assert_eq!(rauc_progress("[⠋      ]  --% Checking bundle"), None);
        assert_eq!(rauc_progress("Install succeeded"), None);
    }

    #[test]
    fn otactls_noise_is_dropped() {
        let kept: Vec<_> = [
            "2026-10-07T18:10:13.147541Z  INFO remora_otactl: Starting Remora OTA Client",
            "2026-10-07T18:10:38.735439Z TRACE remora_otactl: stopped",
            "2026-10-07T18:10:38.735439Z  WARN remora_otactl: slot B looks odd",
            "Checking bundle",
            "✓ Checking bundle",
            "Checking bundle done.",
            "marked new slot active, reboot skipped (--no-reboot) - reboot manually to apply it",
            "Install succeeded - new slot marked active. Reboot manually to apply it.",
            "✗ Copying image to bootimg.0",
        ]
        .into_iter()
        .filter_map(otactl_line)
        .collect();
        assert_eq!(
            kept,
            [
                "WARN: slot B looks odd",
                "Checking bundle",
                "✗ Copying image to bootimg.0"
            ]
        );
    }

    #[test]
    fn paths_are_named_after_the_content() {
        let paths = RemotePaths::new("/data/cache/", "0123456789abcdef0123");
        assert_eq!(
            paths.bundle,
            "/data/cache/rmra-install-0123456789abcdef.raucb"
        );
        assert_eq!(
            paths.part,
            "/data/cache/rmra-install-0123456789abcdef.raucb.part"
        );
    }
}
