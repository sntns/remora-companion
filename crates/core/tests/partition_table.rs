//! Phase 2 exit criterion: our MBR/GPT reader must match the exact
//! partition order/offsets/sizes of real tables, for both boot-mode
//! families used by meta-remora (GPT for EFI mode, MBR for bios/uboot/rpi).
//!
//! The two fixtures under `tests/fixtures/` were built once with the real
//! `sgdisk`/`sfdisk` (never invoked by this test, or by the shipped binary)
//! with partition counts, order and roles mirroring the actual
//! `sdimage-remora.bootmode-{efi,bios,uboot,rpi}.wks.in` templates (sizes are
//! scaled down for a small fixture; ordering/roles are what's being tested).
//! Ground truth below was read back with `sgdisk -p` / `sfdisk -l`.

use remora_etcher_core::{
    application::image,
    model::partition_table::{BootMode, PartitionRole, TableKind},
};

const GPT_FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gpt-efi.img");
const MBR_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/mbr-bios-uboot-rpi.img"
);

const SECTOR: u64 = 512;

#[test]
fn reads_the_efi_gpt_layout_and_resolves_roles() {
    let table = image::inspect(std::path::Path::new(GPT_FIXTURE)).unwrap();

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
            BootMode::Efi.role_of(entry.index),
            Some(role),
            "role for partition {index}"
        );
    }
}

#[test]
fn reads_the_mbr_layout_shared_by_bios_uboot_and_rpi() {
    let table = image::inspect(std::path::Path::new(MBR_FIXTURE)).unwrap();

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

        // bios/uboot/rpi all share the same index table (only EFI mode differs).
        for mode in [BootMode::Bios, BootMode::Uboot, BootMode::Rpi] {
            assert_eq!(
                mode.role_of(entry.index),
                Some(role),
                "role for partition {index} in {mode:?}"
            );
        }
    }

    // None of these modes have an EFI partition.
    for mode in [BootMode::Bios, BootMode::Uboot, BootMode::Rpi] {
        assert_eq!(mode.index_of(PartitionRole::Efi), None);
    }
}
