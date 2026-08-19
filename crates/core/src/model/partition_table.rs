#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    Mbr,
    Gpt,
}

#[derive(Debug, Clone)]
pub struct PartitionEntry {
    pub index: u32,
    /// GPT partition name; always `None` for MBR (it has no such concept).
    pub label: Option<String>,
    pub start_bytes: u64,
    pub size_bytes: u64,
    /// MBR: the 1-byte system id as two hex digits (e.g. "83"). GPT: the
    /// partition type GUID in its usual mixed-endian textual form.
    pub partition_type: String,
}

#[derive(Debug, Clone)]
pub struct PartitionTable {
    pub kind: TableKind,
    pub sector_size: u64,
    pub partitions: Vec<PartitionEntry>,
}

/// The four boot-mode partition layouts baked into meta-remora's
/// `conf/machine/include/remora-{efi,bios,uboot,rpiboot}.inc`
/// (`REMORA_PART_*_INDEX`), so a partition can be identified by role instead
/// of a bare index that means something different in every mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    Efi,
    Bios,
    Uboot,
    Rpi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionRole {
    /// The shared `/boot` (or, in BIOS mode, the GRUB boot) partition.
    Shared,
    /// The EFI system partition. Only present in `BootMode::Efi`.
    Efi,
    SlotA,
    SlotB,
    Data,
}

impl PartitionRole {
    pub const ALL: [PartitionRole; 5] = [
        PartitionRole::Shared,
        PartitionRole::Efi,
        PartitionRole::SlotA,
        PartitionRole::SlotB,
        PartitionRole::Data,
    ];
}

impl BootMode {
    pub fn table_kind(self) -> TableKind {
        match self {
            BootMode::Efi => TableKind::Gpt,
            BootMode::Bios | BootMode::Uboot | BootMode::Rpi => TableKind::Mbr,
        }
    }

    /// Partition index for `role` in this boot mode, or `None` if this mode
    /// has no such partition (only `PartitionRole::Efi`, outside EFI mode).
    pub fn index_of(self, role: PartitionRole) -> Option<u32> {
        use PartitionRole::*;
        match (self, role) {
            (BootMode::Efi, Shared) => Some(1),
            (BootMode::Efi, Efi) => Some(2),
            (BootMode::Efi, SlotA) => Some(3),
            (BootMode::Efi, SlotB) => Some(4),
            (BootMode::Efi, Data) => Some(5),

            (_, Efi) => None,
            (_, Shared) => Some(1),
            (_, SlotA) => Some(2),
            (_, SlotB) => Some(3),
            (_, Data) => Some(4),
        }
    }

    pub fn role_of(self, index: u32) -> Option<PartitionRole> {
        PartitionRole::ALL
            .into_iter()
            .find(|&role| self.index_of(role) == Some(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_mode_index_tables_match_meta_remora() {
        assert_eq!(BootMode::Efi.table_kind(), TableKind::Gpt);
        assert_eq!(BootMode::Bios.table_kind(), TableKind::Mbr);
        assert_eq!(BootMode::Uboot.table_kind(), TableKind::Mbr);
        assert_eq!(BootMode::Rpi.table_kind(), TableKind::Mbr);

        assert_eq!(BootMode::Efi.index_of(PartitionRole::Shared), Some(1));
        assert_eq!(BootMode::Efi.index_of(PartitionRole::Efi), Some(2));
        assert_eq!(BootMode::Efi.index_of(PartitionRole::SlotA), Some(3));
        assert_eq!(BootMode::Efi.index_of(PartitionRole::SlotB), Some(4));
        assert_eq!(BootMode::Efi.index_of(PartitionRole::Data), Some(5));

        for mode in [BootMode::Bios, BootMode::Uboot, BootMode::Rpi] {
            assert_eq!(mode.index_of(PartitionRole::Efi), None);
            assert_eq!(mode.index_of(PartitionRole::Shared), Some(1));
            assert_eq!(mode.index_of(PartitionRole::SlotA), Some(2));
            assert_eq!(mode.index_of(PartitionRole::SlotB), Some(3));
            assert_eq!(mode.index_of(PartitionRole::Data), Some(4));
        }

        assert_eq!(BootMode::Efi.role_of(5), Some(PartitionRole::Data));
        assert_eq!(BootMode::Bios.role_of(4), Some(PartitionRole::Data));
        assert_eq!(BootMode::Bios.role_of(5), None);
    }
}
