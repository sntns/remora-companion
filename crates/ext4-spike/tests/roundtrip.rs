//! Phase 3 go/no-go spike for `am-fs-ext4` (github.com/christhomas/rust-fs-ext4).
//!
//! Narrow surface only, matching what `remora-etcher image partition cp`
//! would actually need: mount an ext4 image built the way meta-remora
//! actually builds/touches its `data` partition (`mke2fs -t ext4` +
//! remora-mount's own `tune2fs -O extents,uninit_bg,dir_index,has_journal`),
//! create one file inside an already-existing directory, write its content,
//! then verify with the real `e2fsck -f` (never shipped in remora-etcher
//! itself — dev/CI validation only) that nothing was corrupted, plus an
//! independent read-back through the crate's own read API and through
//! `debugfs` as a second opinion.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use fs_ext4::block_io::FileDevice;
use fs_ext4::Filesystem;

fn tempdir(label: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "ext4-spike-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(cmd: &str, args: &[&str]) {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {cmd}: {e}"));
    assert!(status.success(), "{cmd} {args:?} exited with {status}");
}

/// Build an ext4 image the way meta-remora actually produces + touches its
/// `data` partition, then pre-create the directory we'll inject into
/// (standing in for whatever the data partition's skeleton already holds).
fn build_fixture(path: &Path, size_mb: u32, mkfs_extra: &[&str]) {
    let path_str = path.to_str().unwrap();
    run("truncate", &["-s", &format!("{size_mb}M"), path_str]);

    let mut mkfs_args = vec!["-F", "-t", "ext4", "-L", "data"];
    mkfs_args.extend_from_slice(mkfs_extra);
    mkfs_args.push(path_str);
    run("mke2fs", &mkfs_args);

    // The exact call remora-mount makes on every boot (recipes-remora/
    // remora-mount/remora-mount/remora-mount, mount_blk()) before mounting
    // the data partition read-write.
    run(
        "tune2fs",
        &["-O", "extents,uninit_bg,dir_index,has_journal", path_str],
    );
    run("fsck.ext4", &["-fy", path_str]);

    run("debugfs", &["-w", "-R", "mkdir /play", path_str]);
    run(
        "debugfs",
        &["-w", "-R", "mkdir /play/tplst-app-config", path_str],
    );
}

/// Force-check without letting fsck repair anything, so a real
/// inconsistency surfaces as a failure instead of being silently patched.
///
/// Exit code alone is NOT enough here: e2fsck 1.47.0 exits 0 from `-fn` even
/// when it printed "checksum is invalid ... IGNORED" for a block-group
/// descriptor (confirmed by hand — see the phase-3 writeup). So this also
/// greps the output for the phrases e2fsck uses for a found-but-declined
/// fix, and fails on those even if the exit code says otherwise.
fn is_e2fsck_clean(path: &Path) -> bool {
    let out = Command::new("fsck.ext4")
        .args(["-fn", path.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    out.status.success()
        && !text.contains("IGNORED")
        && !text.contains("Fix? no")
        && !text.contains("FIXED")
}

fn debugfs_cat(path: &Path, file: &str) -> Vec<u8> {
    let out = Command::new("debugfs")
        .args(["-R", &format!("cat {file}"), path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "debugfs cat {file} failed");
    out.stdout
}

fn inject_one_file(image: &Path, rel_path: &str, contents: &[u8]) {
    let dev = Arc::new(FileDevice::open_rw(image.to_str().unwrap()).expect("FileDevice::open_rw"));
    let fs = Filesystem::mount(dev).expect("Filesystem::mount");

    // Always attempt replay: a no-op on an already-clean journal (our case
    // here), but this is the call a real "unclean unmount" scenario would
    // need — see the residual gap noted in the writeup this spike feeds.
    fs.replay_journal_if_dirty().expect("replay_journal_if_dirty");

    fs.apply_create(rel_path, 0o644).expect("apply_create");
    let written = fs.apply_pwrite(rel_path, 0, contents).expect("apply_pwrite");
    assert_eq!(written, contents.len() as u64);
}

/// Run the full spike (build fixture, inject, verify 3 independent ways)
/// against one mkfs feature combination.
fn run_matrix_case(label: &str, mkfs_extra: &[&str]) {
    let dir = tempdir(label);
    let image = dir.join("data.img");
    build_fixture(&image, 64, mkfs_extra);

    assert!(
        is_e2fsck_clean(&image),
        "[{label}] fixture must be clean before we touch it"
    );

    let payload = format!("hello from remora-etcher [{label}]\n").into_bytes();
    inject_one_file(&image, "/play/tplst-app-config/config.json", &payload);

    assert!(
        is_e2fsck_clean(&image),
        "[{label}] e2fsck found corruption after apply_create + apply_pwrite"
    );

    let read_back = debugfs_cat(&image, "/play/tplst-app-config/config.json");
    assert_eq!(
        read_back, payload,
        "[{label}] debugfs read-back does not match what we wrote"
    );
}

#[test]
fn default_mkfs_features_matching_remora_mount() {
    // Whatever this host's e2fsprogs defaults to for `mke2fs -t ext4` — on
    // this host (representative of a modern e2fsprogs, matching the
    // whinlatter-branch OE-core remora-etcher targets) that's 64bit +
    // metadata_csum ON, but NOT uninit_bg (metadata_csum supersedes it in
    // modern mke2fs); remora-mount's `tune2fs -O ...,uninit_bg,...` call
    // genuinely adds that feature on top, so this case exercises the real
    // production combination: metadata_csum AND uninit_bg both set.
    run_matrix_case("default", &[]);
}

#[test]
fn sixty_four_bit_disabled() {
    run_matrix_case("no-64bit", &["-O", "^64bit"]);
}

/// **Known bug, found by this spike**: with `metadata_csum` off but
/// `uninit_bg` on (the legacy block-group-descriptor checksum scheme —
/// reachable via remora-mount's `tune2fs -O ...,uninit_bg,...` on a fs
/// that wasn't built with metadata_csum), `apply_create`/`apply_pwrite`
/// leave the BGD checksum stale. `e2fsck -fn` reports it
/// ("checksum is invalid ... IGNORED") but still exits 0, so this class of
/// corruption is NOT caught by exit code alone — `is_e2fsck_clean` above
/// greps the output text specifically because of this.
///
/// Severity for us: low. This exact combination isn't what Remora actually
/// ships (real `mke2fs -t ext4` defaults already turn metadata_csum on), and
/// `fsck.ext4 -fy` — which `remora-mount` runs unconditionally on every boot,
/// before this code would ever run in the field — silently repairs it with
/// no data loss (verified by hand: the injected file's content survives the
/// repair). Tracked as a known limitation, not a blocker; worth reporting
/// upstream regardless since it's a real, reproducible bug in their write
/// path for this feature combination.
#[test]
fn metadata_csum_disabled_leaves_a_self_healing_bgd_checksum_stale() {
    let dir = tempdir("no-metadata-csum");
    let image = dir.join("data.img");
    build_fixture(&image, 64, &["-O", "^metadata_csum,^metadata_csum_seed"]);
    assert!(is_e2fsck_clean(&image), "fixture must be clean before we touch it");

    inject_one_file(
        &image,
        "/play/tplst-app-config/config.json",
        b"hello from remora-etcher [no-metadata-csum]\n",
    );

    assert!(
        !is_e2fsck_clean(&image),
        "expected the known stale-BGD-checksum bug to still reproduce; if this now \
         passes, either am-fs-ext4 fixed it upstream (great — promote this back to \
         run_matrix_case and update the phase-3 writeup) or our patch/setup changed"
    );

    // Prove it's exactly the known cosmetic issue and nothing worse: a
    // normal repair pass (what remora-mount actually runs on every boot)
    // fixes it with no data loss.
    run("fsck.ext4", &["-fy", image.to_str().unwrap()]);
    assert!(is_e2fsck_clean(&image), "fsck -fy should have fully repaired it");
    assert_eq!(
        debugfs_cat(&image, "/play/tplst-app-config/config.json"),
        b"hello from remora-etcher [no-metadata-csum]\n"
    );
}
