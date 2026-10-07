//! Our MBR/GPT reader must match the exact partition order/offsets/sizes of
//! real tables, for both table kinds used by meta-remora (GPT for EFI
//! machines, MBR for bios/uboot/rpi), and the roles resolved from what it
//! reads must be the ones each real layout means.
//!
//! The two fixtures under `tests/fixtures/` were built once with the real
//! `sgdisk`/`sfdisk` (never invoked by this test, or by the shipped binary)
//! with partition counts, order and roles mirroring the actual
//! `sdimage-remora.bootmode-{efi,bios,uboot,rpi}.wks.in` templates (sizes are
//! scaled down for a small fixture; ordering/roles are what's being tested).
//! Ground truth below was read back with `sgdisk -p` / `sfdisk -l`.
//!
//! The GPT layouts that put their roles elsewhere (F3APL's, rock-s0's) are
//! built per test with the real `sfdisk` instead, named the way wic names
//! them (a partition's `--part-name`, else its `--label`).

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use remora_image::{
    adapter::partition_table::PartitionTableAdapter,
    model::{PartitionRole, TableKind},
};
use remora_image_adapter_partition_table::PartitionTableAdapterImpl;

const GPT_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gpt-efi.img");
const MBR_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/mbr-bios-uboot-rpi.img"
);

const SECTOR: u64 = 512;

#[test]
fn reads_the_efi_gpt_layout_and_resolves_roles() {
    let table = PartitionTableAdapterImpl
        .read(std::path::Path::new(GPT_FIXTURE))
        .unwrap();

    assert_eq!(table.kind, TableKind::Gpt);
    assert_eq!(table.sector_size, SECTOR);
    assert_eq!(table.partitions.len(), 5);

    let expected = [
        // (index, start_lba, size_sectors, label, type_guid, role)
        (
            1,
            2048,
            2048,
            Some("boot"),
            "0FC63DAF-8483-4772-8E79-3D69D8477DE4",
            PartitionRole::Shared,
        ),
        (
            2,
            4096,
            2048,
            Some("EFI"),
            "C12A7328-F81F-11D2-BA4B-00A0C93EC93B",
            PartitionRole::Efi,
        ),
        (
            3,
            6144,
            4096,
            Some("slotA"),
            "0FC63DAF-8483-4772-8E79-3D69D8477DE4",
            PartitionRole::SlotA,
        ),
        (
            4,
            10240,
            4096,
            None,
            "0FC63DAF-8483-4772-8E79-3D69D8477DE4",
            PartitionRole::SlotB,
        ),
        (
            5,
            14336,
            6144,
            Some("data"),
            "0FC63DAF-8483-4772-8E79-3D69D8477DE4",
            PartitionRole::Data,
        ),
    ];

    for (entry, (index, start_lba, size_sectors, label, type_guid, role)) in
        table.partitions.iter().zip(expected)
    {
        assert_eq!(entry.index, index, "partition index");
        assert_eq!(
            entry.start_bytes,
            start_lba * SECTOR,
            "start offset for partition {index}"
        );
        assert_eq!(
            entry.size_bytes,
            size_sectors * SECTOR,
            "size for partition {index}"
        );
        assert_eq!(entry.label.as_deref(), label, "label for partition {index}");
        assert_eq!(
            entry.partition_type, type_guid,
            "type GUID for partition {index}"
        );
        assert_eq!(
            table.role_of(entry.index),
            Some(role),
            "role for partition {index}"
        );
    }
}

#[test]
fn reads_the_mbr_layout_shared_by_bios_uboot_and_rpi() {
    let table = PartitionTableAdapterImpl
        .read(std::path::Path::new(MBR_FIXTURE))
        .unwrap();

    assert_eq!(table.kind, TableKind::Mbr);
    assert_eq!(table.sector_size, SECTOR);
    assert_eq!(table.partitions.len(), 4);

    let expected = [
        // (index, start_lba, size_sectors, mbr_type, role)
        (1, 2048, 2048, "0C", PartitionRole::Shared),
        (2, 4096, 4096, "83", PartitionRole::SlotA),
        (3, 8192, 4096, "83", PartitionRole::SlotB),
        (4, 12288, 6144, "83", PartitionRole::Data),
    ];

    for (entry, (index, start_lba, size_sectors, mbr_type, role)) in
        table.partitions.iter().zip(expected)
    {
        assert_eq!(entry.index, index, "partition index");
        assert_eq!(
            entry.start_bytes,
            start_lba * SECTOR,
            "start offset for partition {index}"
        );
        assert_eq!(
            entry.size_bytes,
            size_sectors * SECTOR,
            "size for partition {index}"
        );
        assert!(entry.label.is_none(), "MBR partitions never carry a label");
        assert_eq!(entry.partition_type, mbr_type, "type for partition {index}");

        // bios/uboot/rpi all share this one layout.
        assert_eq!(
            table.role_of(entry.index),
            Some(role),
            "role for partition {index}"
        );
    }

    // No MBR layout has an EFI partition.
    assert!(table.select_role(PartitionRole::Efi).is_err());
}

const ESP: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
const BIOS_BOOT: &str = "21686148-6449-6E6F-744E-656564454649";

/// A sparse 64 MiB image partitioned by the real `sfdisk` from `parts`
/// (`(start sector, size in sectors, type GUID, name)`, in table order).
fn sfdisk_gpt(parts: &[(u64, u64, Option<&str>, Option<&str>)]) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut image = std::env::temp_dir();
    image.push(format!(
        "remora-partition-table-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    fs::File::create(&image)
        .unwrap()
        .set_len(64 * 1024 * 1024)
        .unwrap();

    let mut script = "label: gpt\nunit: sectors\nfirst-lba: 34\n\n".to_string();
    for (start, size, part_type, name) in parts {
        script.push_str(&format!("start={start}, size={size}"));
        if let Some(part_type) = part_type {
            script.push_str(&format!(", type={part_type}"));
        }
        if let Some(name) = name {
            script.push_str(&format!(", name=\"{name}\""));
        }
        script.push('\n');
    }
    let mut child = Command::new("sfdisk")
        .arg(&image)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("sfdisk not available");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "sfdisk failed");
    image
}

fn roles_of(image: &std::path::Path) -> Vec<(PartitionRole, u32)> {
    let table = PartitionTableAdapterImpl.read(image).unwrap();
    assert_eq!(table.kind, TableKind::Gpt);
    PartitionRole::ALL
        .into_iter()
        .filter_map(|role| table.select_role(role).ok().map(|p| (role, p.index)))
        .collect()
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk, a Linux-only dev tool"
)]
fn resolves_roles_in_the_f3apl_layout() {
    // laplaylist-image.f3apl.wks.in: biosboot, EFI, boot, slotA, slotB, data.
    let image = sfdisk_gpt(&[
        (2048, 2048, Some(BIOS_BOOT), Some("biosboot")),
        (4096, 2048, Some(ESP), Some("EFI")),
        (6144, 2048, None, Some("boot")),
        (8192, 4096, None, Some("slotA")),
        (12288, 4096, None, Some("slotB")),
        (16384, 4096, None, Some("data")),
    ]);
    assert_eq!(
        roles_of(&image),
        [
            (PartitionRole::Shared, 3),
            (PartitionRole::Efi, 2),
            (PartitionRole::SlotA, 4),
            (PartitionRole::SlotB, 5),
            (PartitionRole::Data, 6),
        ]
    );
    let _ = fs::remove_file(&image);
}

#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "requires sfdisk, a Linux-only dev tool"
)]
fn resolves_roles_in_the_rock_s0_layout() {
    // sdimage-remora.rock-s0.wks.in: eight raw u-boot partitions at fixed
    // offsets, then boot, slotA, slotB, data.
    let raw = [
        (64, 7104, "loader1"),
        (7168, 512, "v_storage"),
        (7680, 384, "reserved"),
        (8064, 64, "reserved1"),
        (8128, 64, "uboot_env"),
        (8192, 8192, "reserved2"),
        (16384, 8192, "loader2"),
        (24576, 8192, "atf"),
    ];
    let mut parts: Vec<_> = raw
        .iter()
        .map(|&(start, size, name)| (start, size, None, Some(name)))
        .collect();
    parts.extend([
        (32768, 2048, None, Some("boot")),
        (36864, 4096, None, Some("slotA")),
        (40960, 4096, None, Some("slotB")),
        (45056, 4096, None, Some("data")),
    ]);
    let image = sfdisk_gpt(&parts);
    assert_eq!(
        roles_of(&image),
        [
            (PartitionRole::Shared, 9),
            (PartitionRole::SlotA, 10),
            (PartitionRole::SlotB, 11),
            (PartitionRole::Data, 12),
        ]
    );
    let _ = fs::remove_file(&image);
}
