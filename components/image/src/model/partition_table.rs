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

/// A partition's job in a Remora image, independent of where the layout
/// puts it: found by the partition's own GPT name (or, on MBR, by Remora's
/// one fixed 4-partition order) — see `PartitionTable::select_role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PartitionRole {
    /// The shared `/boot` (or, in BIOS mode, the GRUB boot) partition.
    Shared,
    /// The EFI system partition. Only GPT (EFI) images have one.
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

    /// The GPT partition name wic gives this role's partition (its
    /// `--part-name`, else its `--label`), matched case-insensitively.
    fn gpt_name(self) -> &'static str {
        match self {
            PartitionRole::Shared => "boot",
            PartitionRole::Efi => "EFI",
            PartitionRole::SlotA => "slotA",
            PartitionRole::SlotB => "slotB",
            PartitionRole::Data => "data",
        }
    }

    /// Index of this role in the one MBR layout every msdos-table Remora
    /// image (bios/uboot/rpi) shares, or `None` for the EFI partition,
    /// which only GPT images have.
    fn mbr_index(self) -> Option<u32> {
        match self {
            PartitionRole::Shared => Some(1),
            PartitionRole::Efi => None,
            PartitionRole::SlotA => Some(2),
            PartitionRole::SlotB => Some(3),
            PartitionRole::Data => Some(4),
        }
    }
}

/// The EFI System Partition type GUID, as `PartitionEntry::partition_type`
/// spells it.
const ESP_TYPE_GUID: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";

/// Primary partitions in an MBR Remora image: shared, slotA, slotB, data.
const MBR_PARTITION_COUNT: usize = 4;

/// Whether a partition is formatted as ext4 or vfat — a structural fact
/// determined by content (see the partition-table adapter's
/// `detect_fs_kind`), never asked of the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Ext4,
    Vfat,
}

/// How the CLI/caller identifies which partition to act on — always
/// explicit for write operations: a guessed partition is a silently
/// corrupted one. A role is resolved from the image's own layout, never
/// from an index the caller assumes (see `PartitionTable::select_role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PartitionSelector {
    /// Raw partition index, as printed by `image partition list`.
    Index(u32),
    /// A Remora role (shared/efi/slotA/slotB/data).
    Role(PartitionRole),
}

#[derive(Debug, thiserror::Error)]
pub enum SelectionError {
    #[error("a {1:?} table has no {0:?} partition")]
    RoleNotPresentInTable(PartitionRole, TableKind),
    #[error("no {role:?} partition in this image; partitions: {seen}")]
    RoleNotFound { role: PartitionRole, seen: String },
    #[error("{role:?} matches partitions {matches:?}, expected exactly one; partitions: {seen}")]
    AmbiguousRole {
        role: PartitionRole,
        matches: Vec<u32>,
        seen: String,
    },
    #[error(
        "an MBR Remora image has exactly 4 primary partitions (shared, slotA, slotB, data); \
         partitions: {seen}"
    )]
    UnexpectedMbrLayout { seen: String },
    #[error("no partition with index {0} in this table")]
    NoSuchPartition(u32),
}

impl PartitionTable {
    /// Resolve `selector` to a concrete partition already present in this
    /// table. Never guesses: an index that isn't in the table, or a role
    /// this image's layout doesn't identify unambiguously, is an error
    /// rather than a fallback.
    pub fn select(&self, selector: PartitionSelector) -> Result<&PartitionEntry, SelectionError> {
        match selector {
            PartitionSelector::Index(index) => self
                .partitions
                .iter()
                .find(|p| p.index == index)
                .ok_or(SelectionError::NoSuchPartition(index)),
            PartitionSelector::Role(role) => self.select_role(role),
        }
    }

    /// Resolve `role` from this image's own layout. GPT: by partition name
    /// (`PartitionRole::gpt_name`, case-insensitive), the EFI partition
    /// also by its type GUID — names, not indices, because GPT layouts put
    /// the same roles at different indices (F3APL leads with a BIOS boot
    /// partition, rock-s0 with eight raw u-boot ones). MBR has no names,
    /// and Remora has one MBR layout: exactly four primary partitions,
    /// shared/slotA/slotB/data in that order.
    pub fn select_role(&self, role: PartitionRole) -> Result<&PartitionEntry, SelectionError> {
        match self.kind {
            TableKind::Gpt => self.select_gpt_role(role),
            TableKind::Mbr => self.select_mbr_role(role),
        }
    }

    /// The role `index` resolves to, if any — for display only.
    pub fn role_of(&self, index: u32) -> Option<PartitionRole> {
        PartitionRole::ALL.into_iter().find(|&role| {
            self.select_role(role)
                .is_ok_and(|entry| entry.index == index)
        })
    }

    fn select_gpt_role(&self, role: PartitionRole) -> Result<&PartitionEntry, SelectionError> {
        let matches: Vec<&PartitionEntry> = self
            .partitions
            .iter()
            .filter(|p| {
                has_name(p, role.gpt_name())
                    || (role == PartitionRole::Efi
                        && p.partition_type.eq_ignore_ascii_case(ESP_TYPE_GUID))
            })
            .collect();
        match matches.as_slice() {
            [entry] => Ok(entry),
            // wic names slotB only when its `.wks.in` gives it a
            // `--part-name`/`--label`; the generic EFI layout doesn't (it's
            // left unformatted for the first update to fill).
            [] if role == PartitionRole::SlotB => self.unnamed_between_slot_a_and_data(),
            [] => Err(SelectionError::RoleNotFound {
                role,
                seen: self.describe(),
            }),
            _ => Err(self.ambiguous(role, &matches)),
        }
    }

    /// The one unnamed partition lying between slotA and data.
    fn unnamed_between_slot_a_and_data(&self) -> Result<&PartitionEntry, SelectionError> {
        let not_found = || SelectionError::RoleNotFound {
            role: PartitionRole::SlotB,
            seen: self.describe(),
        };
        let slot_a = self
            .select_gpt_role(PartitionRole::SlotA)
            .map_err(|_| not_found())?;
        let data = self
            .select_gpt_role(PartitionRole::Data)
            .map_err(|_| not_found())?;
        let candidates: Vec<&PartitionEntry> = self
            .partitions
            .iter()
            .filter(|p| {
                p.label.is_none()
                    && p.start_bytes > slot_a.start_bytes
                    && p.start_bytes < data.start_bytes
            })
            .collect();
        match candidates.as_slice() {
            [entry] => Ok(entry),
            [] => Err(not_found()),
            _ => Err(self.ambiguous(PartitionRole::SlotB, &candidates)),
        }
    }

    fn select_mbr_role(&self, role: PartitionRole) -> Result<&PartitionEntry, SelectionError> {
        let index = role
            .mbr_index()
            .ok_or(SelectionError::RoleNotPresentInTable(role, self.kind))?;
        let primary =
            (1..=MBR_PARTITION_COUNT as u32).all(|i| self.partitions.iter().any(|p| p.index == i));
        if self.partitions.len() != MBR_PARTITION_COUNT || !primary {
            return Err(SelectionError::UnexpectedMbrLayout {
                seen: self.describe(),
            });
        }
        self.partitions
            .iter()
            .find(|p| p.index == index)
            .ok_or(SelectionError::NoSuchPartition(index))
    }

    fn ambiguous(&self, role: PartitionRole, matches: &[&PartitionEntry]) -> SelectionError {
        SelectionError::AmbiguousRole {
            role,
            matches: matches.iter().map(|p| p.index).collect(),
            seen: self.describe(),
        }
    }

    /// Every partition as `#index "name" type`, for an error to show what
    /// it looked at.
    fn describe(&self) -> String {
        if self.partitions.is_empty() {
            return "none".to_string();
        }
        self.partitions
            .iter()
            .map(|p| match &p.label {
                Some(label) => format!("#{} {label:?} {}", p.index, p.partition_type),
                None => format!("#{} (unnamed) {}", p.index, p.partition_type),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn has_name(entry: &PartitionEntry, name: &str) -> bool {
    entry
        .label
        .as_deref()
        .is_some_and(|label| label.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX: &str = "0FC63DAF-8483-4772-8E79-3D69D8477DE4";
    const BIOS_BOOT: &str = "21686148-6449-6E6F-744E-656564454649";

    /// A GPT table of `(name, type GUID)` partitions, numbered and laid out
    /// in order.
    fn gpt(parts: &[(Option<&str>, &str)]) -> PartitionTable {
        PartitionTable {
            kind: TableKind::Gpt,
            sector_size: 512,
            partitions: parts
                .iter()
                .zip(1..)
                .map(|(&(name, partition_type), index)| PartitionEntry {
                    index,
                    label: name.map(str::to_string),
                    start_bytes: u64::from(index) * 1024 * 1024,
                    size_bytes: 1024 * 1024,
                    partition_type: partition_type.to_string(),
                })
                .collect(),
        }
    }

    fn mbr(count: u32) -> PartitionTable {
        PartitionTable {
            kind: TableKind::Mbr,
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

    fn roles(table: &PartitionTable) -> Vec<(PartitionRole, u32)> {
        PartitionRole::ALL
            .into_iter()
            .filter_map(|role| table.select_role(role).ok().map(|p| (role, p.index)))
            .collect()
    }

    #[test]
    fn resolves_the_generic_efi_layout_with_an_unnamed_slot_b() {
        let table = gpt(&[
            (Some("boot"), LINUX),
            (Some("EFI"), ESP_TYPE_GUID),
            (Some("slotA"), LINUX),
            (None, LINUX),
            (Some("data"), LINUX),
        ]);
        assert_eq!(
            roles(&table),
            [
                (PartitionRole::Shared, 1),
                (PartitionRole::Efi, 2),
                (PartitionRole::SlotA, 3),
                (PartitionRole::SlotB, 4),
                (PartitionRole::Data, 5),
            ]
        );
    }

    #[test]
    fn resolves_the_f3apl_layout_led_by_a_bios_boot_partition() {
        let table = gpt(&[
            (Some("biosboot"), BIOS_BOOT),
            (Some("EFI"), ESP_TYPE_GUID),
            (Some("boot"), LINUX),
            (Some("slotA"), LINUX),
            (Some("slotB"), LINUX),
            (Some("data"), LINUX),
        ]);
        assert_eq!(
            roles(&table),
            [
                (PartitionRole::Shared, 3),
                (PartitionRole::Efi, 2),
                (PartitionRole::SlotA, 4),
                (PartitionRole::SlotB, 5),
                (PartitionRole::Data, 6),
            ]
        );
    }

    #[test]
    fn resolves_the_rock_s0_layout_led_by_raw_u_boot_partitions() {
        let raw = [
            "loader1",
            "v_storage",
            "reserved",
            "reserved1",
            "uboot_env",
            "reserved2",
            "loader2",
            "atf",
        ];
        let mut parts: Vec<(Option<&str>, &str)> =
            raw.iter().map(|&name| (Some(name), LINUX)).collect();
        parts.extend([
            (Some("boot"), LINUX),
            (Some("slotA"), LINUX),
            (Some("slotB"), LINUX),
            (Some("data"), LINUX),
        ]);
        assert_eq!(
            roles(&gpt(&parts)),
            [
                (PartitionRole::Shared, 9),
                (PartitionRole::SlotA, 10),
                (PartitionRole::SlotB, 11),
                (PartitionRole::Data, 12),
            ]
        );
    }

    #[test]
    fn matches_gpt_names_case_insensitively_and_the_esp_by_type_alone() {
        let table = gpt(&[
            (Some("BOOT"), LINUX),
            (None, ESP_TYPE_GUID),
            (Some("SlotA"), LINUX),
            (Some("SLOTB"), LINUX),
            (Some("Data"), LINUX),
        ]);
        assert_eq!(table.select_role(PartitionRole::Shared).unwrap().index, 1);
        assert_eq!(table.select_role(PartitionRole::Efi).unwrap().index, 2);
        assert_eq!(table.select_role(PartitionRole::SlotB).unwrap().index, 4);
    }

    #[test]
    fn a_missing_role_is_an_error_naming_the_partitions_seen() {
        // No EFI partition, and no `data` for the unnamed slotB to sit
        // before: neither falls back to an index.
        let table = gpt(&[(Some("boot"), LINUX), (Some("slotA"), LINUX), (None, LINUX)]);
        for role in [
            PartitionRole::Efi,
            PartitionRole::Data,
            PartitionRole::SlotB,
        ] {
            let err = table.select_role(role).unwrap_err();
            assert!(
                matches!(&err, SelectionError::RoleNotFound { role: r, .. } if *r == role),
                "{role:?}: {err:?}"
            );
            let message = err.to_string();
            assert!(
                message.contains(r#"#1 "boot""#) && message.contains("#3 (unnamed)"),
                "{message}"
            );
        }
    }

    #[test]
    fn a_role_matched_twice_is_ambiguous() {
        let table = gpt(&[
            (Some("boot"), LINUX),
            (Some("slotA"), LINUX),
            (Some("slotB"), LINUX),
            (Some("data"), LINUX),
            (Some("DATA"), LINUX),
        ]);
        assert!(matches!(
            table.select_role(PartitionRole::Data).unwrap_err(),
            SelectionError::AmbiguousRole { role: PartitionRole::Data, matches, .. }
                if matches == [4, 5]
        ));

        // Two unnamed partitions between slotA and data: no guessing which
        // one is slotB.
        let table = gpt(&[
            (Some("slotA"), LINUX),
            (None, LINUX),
            (None, LINUX),
            (Some("data"), LINUX),
        ]);
        assert!(matches!(
            table.select_role(PartitionRole::SlotB).unwrap_err(),
            SelectionError::AmbiguousRole { role: PartitionRole::SlotB, matches, .. }
                if matches == [2, 3]
        ));
    }

    #[test]
    fn resolves_the_four_partition_mbr_layout() {
        let table = mbr(4);
        assert_eq!(
            roles(&table),
            [
                (PartitionRole::Shared, 1),
                (PartitionRole::SlotA, 2),
                (PartitionRole::SlotB, 3),
                (PartitionRole::Data, 4),
            ]
        );
        assert!(matches!(
            table.select_role(PartitionRole::Efi).unwrap_err(),
            SelectionError::RoleNotPresentInTable(PartitionRole::Efi, TableKind::Mbr)
        ));
        assert_eq!(table.role_of(4), Some(PartitionRole::Data));
    }

    #[test]
    fn refuses_roles_on_an_mbr_without_exactly_four_partitions() {
        for count in [1, 3, 5] {
            let table = mbr(count);
            assert!(
                matches!(
                    table.select_role(PartitionRole::Shared).unwrap_err(),
                    SelectionError::UnexpectedMbrLayout { .. }
                ),
                "{count} partitions"
            );
            assert_eq!(table.role_of(1), None);
            // A raw index still resolves: only roles need the layout.
            assert_eq!(table.select(PartitionSelector::Index(1)).unwrap().index, 1);
        }
    }
}
