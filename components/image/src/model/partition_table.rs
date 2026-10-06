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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootMode {
    Efi,
    Bios,
    Uboot,
    Rpi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
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
    ///
    /// Delegates to `TableKind::index_of` — BIOS/UBOOT/RPI share one MBR
    /// table (only their *filesystem type* on shared/slotA/slotB differs,
    /// see `FsKind`/`detect_fs_kind`), so the four-way `BootMode` distinction
    /// was never actually load-bearing for role→index resolution.
    pub fn index_of(self, role: PartitionRole) -> Option<u32> {
        self.table_kind().index_of(role)
    }

    pub fn role_of(self, index: u32) -> Option<PartitionRole> {
        PartitionRole::ALL
            .into_iter()
            .find(|&role| self.index_of(role) == Some(index))
    }
}

impl TableKind {
    /// Partition index for `role` under this table kind, or `None` if no
    /// such partition exists (only `PartitionRole::Efi`, which only GPT/EFI
    /// mode has). GPT here always means the EFI layout; every MBR-based
    /// mode (BIOS/UBOOT/RPI) shares this same 4-partition table.
    pub fn index_of(self, role: PartitionRole) -> Option<u32> {
        use PartitionRole::*;
        match (self, role) {
            (TableKind::Gpt, Shared) => Some(1),
            (TableKind::Gpt, Efi) => Some(2),
            (TableKind::Gpt, SlotA) => Some(3),
            (TableKind::Gpt, SlotB) => Some(4),
            (TableKind::Gpt, Data) => Some(5),

            (TableKind::Mbr, Efi) => None,
            (TableKind::Mbr, Shared) => Some(1),
            (TableKind::Mbr, SlotA) => Some(2),
            (TableKind::Mbr, SlotB) => Some(3),
            (TableKind::Mbr, Data) => Some(4),
        }
    }
}

/// Whether a partition is formatted as ext4 or vfat — a structural fact
/// determined by content (see the partition-table adapter's
/// `detect_fs_kind`), never asked of the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Ext4,
    Vfat,
}

/// How the CLI/caller identifies which partition to act on — always
/// explicit, never auto-detected, for write operations: a guessed partition
/// is a silently corrupted one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PartitionSelector {
    /// Raw partition index, as printed by `image partition list`.
    Index(u32),
    /// A Remora role (shared/efi/slotA/slotB/data), resolved to an index via
    /// a required `BootMode`.
    Role(PartitionRole),
}

#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    #[error("selecting partition role {0:?} requires --boot-mode")]
    RoleRequiresBootMode(PartitionRole),
    #[error("boot mode {1:?} has no {0:?} partition")]
    RoleNotPresentInBootMode(PartitionRole, BootMode),
    #[error("a {1:?} table has no {0:?} partition")]
    RoleNotPresentInTable(PartitionRole, TableKind),
    #[error(
        "boot mode {mode:?} lays out a {expected:?} table, but this image has a {found:?} one"
    )]
    BootModeMismatch {
        mode: BootMode,
        expected: TableKind,
        found: TableKind,
    },
    #[error("no partition with index {0} in this table")]
    NoSuchPartition(u32),
}

impl PartitionTable {
    /// Resolve `selector` to a concrete partition already present in this
    /// table. Never guesses: an index that isn't in the table, a role this
    /// boot mode doesn't have, or a boot mode whose layout is for the other
    /// table kind (its role indices would name the wrong partitions) is an
    /// error rather than a fallback.
    pub fn select(
        &self,
        selector: PartitionSelector,
        boot_mode: Option<BootMode>,
    ) -> Result<&PartitionEntry, SelectionError> {
        let index = match selector {
            PartitionSelector::Index(index) => index,
            PartitionSelector::Role(role) => {
                let mode = boot_mode.ok_or(SelectionError::RoleRequiresBootMode(role))?;
                if mode.table_kind() != self.kind {
                    return Err(SelectionError::BootModeMismatch {
                        mode,
                        expected: mode.table_kind(),
                        found: self.kind,
                    });
                }
                mode.index_of(role)
                    .ok_or(SelectionError::RoleNotPresentInBootMode(role, mode))?
            }
        };
        self.partitions
            .iter()
            .find(|p| p.index == index)
            .ok_or(SelectionError::NoSuchPartition(index))
    }

    /// Resolve `role` using this table's own detected `kind` — no boot mode
    /// needed, since role→index resolution only ever depends on GPT vs MBR
    /// (see `TableKind::index_of`), which is already known once the table's
    /// been read. Used by the identity/config verticals, which must not
    /// require a `--boot-mode` flag.
    pub fn select_role(&self, role: PartitionRole) -> Result<&PartitionEntry, SelectionError> {
        let index = self
            .kind
            .index_of(role)
            .ok_or(SelectionError::RoleNotPresentInTable(role, self.kind))?;
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

    fn table(kind: TableKind, count: u32) -> PartitionTable {
        PartitionTable {
            kind,
            sector_size: 512,
            partitions: (1..=count)
                .map(|index| PartitionEntry {
                    index,
                    label: None,
                    start_bytes: u64::from(index) * 1024 * 1024,
                    size_bytes: 1024 * 1024,
                    partition_type: "83".to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn select_refuses_a_boot_mode_laid_out_for_the_other_table_kind() {
        // BIOS's `data` is index 4, which on GPT is slotB.
        let gpt = table(TableKind::Gpt, 5);
        let err = gpt
            .select(
                PartitionSelector::Role(PartitionRole::Data),
                Some(BootMode::Bios),
            )
            .unwrap_err();
        assert!(matches!(
            err,
            SelectionError::BootModeMismatch {
                mode: BootMode::Bios,
                expected: TableKind::Mbr,
                found: TableKind::Gpt,
            }
        ));

        let mbr = table(TableKind::Mbr, 4);
        assert!(matches!(
            mbr.select(
                PartitionSelector::Role(PartitionRole::Shared),
                Some(BootMode::Efi)
            ),
            Err(SelectionError::BootModeMismatch { .. })
        ));
    }

    #[test]
    fn select_resolves_a_role_with_a_matching_boot_mode() {
        let gpt = table(TableKind::Gpt, 5);
        let entry = gpt
            .select(
                PartitionSelector::Role(PartitionRole::Data),
                Some(BootMode::Efi),
            )
            .unwrap();
        assert_eq!(entry.index, 5);

        // A raw index needs no boot mode, and ignores a mismatched one.
        let mbr = table(TableKind::Mbr, 4);
        let entry = mbr
            .select(PartitionSelector::Index(2), Some(BootMode::Efi))
            .unwrap();
        assert_eq!(entry.index, 2);
    }
}
