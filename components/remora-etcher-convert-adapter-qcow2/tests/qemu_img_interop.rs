//! Validates this from-scratch codec against the real `qemu-img` binary in
//! both directions -- dev-only, never shelled out to by the shipped binary
//! (same posture as this workspace's other integration tests, e.g.
//! `remora-etcher-config-application`'s `sfdisk`/`mke2fs`-based fixtures).
//!
//! Without this, a subtly wrong bit mask or offset calculation could look
//! fine to our own round-trip test (which only ever reads back what we
//! ourselves wrote) while producing an image no real qcow2 consumer (QEMU,
//! `qemu-img`) can actually open.

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

use remora_etcher_convert::adapter::ContainerFormatAdapter;
use remora_etcher_convert_adapter_qcow2::Qcow2AdapterImpl;

const CLUSTER_SIZE: u64 = 65536;

fn temp_path(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut path = std::env::temp_dir();
    path.push(format!(
        "remora-etcher-convert-adapter-qcow2-interop-{label}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path
}

/// A raw fixture spanning several clusters, mixing all-zero clusters with
/// non-zero ones and ending mid-cluster.
fn build_mixed_raw_fixture(path: &PathBuf) {
    let mut f = fs::File::create(path).unwrap();
    f.write_all(&vec![0xABu8; CLUSTER_SIZE as usize]).unwrap();
    f.write_all(&vec![0u8; CLUSTER_SIZE as usize]).unwrap();
    f.write_all(&vec![0xCDu8; CLUSTER_SIZE as usize]).unwrap();
    f.write_all(&vec![0u8; CLUSTER_SIZE as usize]).unwrap();
    f.write_all(&vec![0xEFu8; (CLUSTER_SIZE / 2) as usize])
        .unwrap();
    // Push past a single L2 table's reach (8192 entries/table at these
    // constants) so a from-scratch bug in L1-group indexing would show up:
    // seek out near the end of a second L1 group and write one more
    // non-zero cluster there, with a large all-zero gap in between.
    f.set_len(CLUSTER_SIZE * 8200).unwrap();
    use std::io::{Seek, SeekFrom};
    f.seek(SeekFrom::Start(CLUSTER_SIZE * 8199)).unwrap();
    f.write_all(&vec![0x42u8; CLUSTER_SIZE as usize]).unwrap();
}

#[test]
fn our_encoded_qcow2_is_accepted_by_real_qemu_img_and_round_trips() {
    let raw = temp_path("raw");
    build_mixed_raw_fixture(&raw);

    let qcow2 = temp_path("ours.qcow2");
    Qcow2AdapterImpl.encode_from_raw(&raw, &qcow2).unwrap();

    let check = Command::new("qemu-img")
        .args(["check", "-f", "qcow2"])
        .arg(&qcow2)
        .output()
        .expect("qemu-img not available");
    assert!(
        check.status.success(),
        "qemu-img check failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&check.stdout),
        String::from_utf8_lossy(&check.stderr)
    );

    let via_qemu_raw = temp_path("via-qemu.raw");
    let convert = Command::new("qemu-img")
        .args(["convert", "-f", "qcow2", "-O", "raw"])
        .arg(&qcow2)
        .arg(&via_qemu_raw)
        .output()
        .expect("qemu-img not available");
    assert!(
        convert.status.success(),
        "qemu-img convert failed: {}",
        String::from_utf8_lossy(&convert.stderr)
    );

    assert_eq!(fs::read(&raw).unwrap(), fs::read(&via_qemu_raw).unwrap());

    for p in [raw, qcow2, via_qemu_raw] {
        let _ = fs::remove_file(p);
    }
}

#[test]
fn our_decoder_reads_a_qcow2_produced_by_real_qemu_img() {
    let raw = temp_path("raw2");
    build_mixed_raw_fixture(&raw);

    let via_qemu_qcow2 = temp_path("via-qemu.qcow2");
    let convert = Command::new("qemu-img")
        .args(["convert", "-f", "raw", "-O", "qcow2"])
        .arg(&raw)
        .arg(&via_qemu_qcow2)
        .output()
        .expect("qemu-img not available");
    assert!(
        convert.status.success(),
        "qemu-img convert failed: {}",
        String::from_utf8_lossy(&convert.stderr)
    );

    let ours_raw = temp_path("ours2.raw");
    Qcow2AdapterImpl
        .decode_to_raw(&via_qemu_qcow2, &ours_raw)
        .unwrap();

    assert_eq!(fs::read(&raw).unwrap(), fs::read(&ours_raw).unwrap());

    for p in [raw, via_qemu_qcow2, ours_raw] {
        let _ = fs::remove_file(p);
    }
}
