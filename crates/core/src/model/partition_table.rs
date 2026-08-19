use std::fmt;

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

/// How the CLI/caller identifies which partition to act on — always
/// explicit, never auto-detected, for write operations (see the project
/// plan's safety notes on partition selection).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionSelector {
    /// Raw partition index, as printed by `image partition list`.
    Index(u32),
    /// A Remora role (shared/efi/slotA/slotB/data), resolved to an index via
    /// a required `BootMode`.
    Role(PartitionRole),
}

#[derive(Debug)]
pub enum SelectionError {
    RoleRequiresBootMode(PartitionRole),
    RoleNotPresentInBootMode(PartitionRole, BootMode),
    NoSuchPartition(u32),
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectionError::RoleRequiresBootMode(role) => {
                write!(f, "selecting partition role {role:?} requires --boot-mode")
            }
            SelectionError::RoleNotPresentInBootMode(role, mode) => {
                write!(f, "boot mode {mode:?} has no {role:?} partition")
            }
            SelectionError::NoSuchPartition(index) => {
                write!(f, "no partition with index {index} in this table")
            }
        }
    }
}

impl std::error::Error for SelectionError {}

impl PartitionTable {
    /// Resolve `selector` to a concrete partition already present in this
    /// table. Never guesses: an index that isn't in the table, or a role
    /// this boot mode doesn't have, is an error rather than a fallback.
    pub fn select(
        &self,
        selector: PartitionSelector,
        boot_mode: Option<BootMode>,
    ) -> Result<&PartitionEntry, SelectionError> {
        let index = match selector {
            PartitionSelector::Index(index) => index,
            PartitionSelector::Role(role) => {
                let mode = boot_mode.ok_or(SelectionError::RoleRequiresBootMode(role))?;
                mode.index_of(role)
                    .ok_or(SelectionError::RoleNotPresentInBootMode(role, mode))?
            }
        };
        self.partitions
            .iter()
            .find(|p| p.index == index)
            .ok_or(SelectionError::NoSuchPartition(index))
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
