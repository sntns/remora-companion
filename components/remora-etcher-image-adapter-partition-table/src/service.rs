use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use error_stack::Report;
use remora_etcher_image::{
    adapter::partition_table::{Error, PartitionTableAdapter, Result},
    model::{FsKind, PartitionTable},
};

use crate::{gpt, mbr};

#[derive(Debug, Default, Clone, Copy)]
pub struct PartitionTableAdapterImpl;

impl PartitionTableAdapter for PartitionTableAdapterImpl {
    fn read(&self, path: &Path) -> Result<PartitionTable> {
        let mut file =
            File::open(path).map_err(|_| Report::new(Error::Open(path.to_path_buf())))?;

        let gpt_err = match gpt::read(&mut file) {
            Ok(table) => return Ok(table),
            Err(e) => e,
        };

        file.seek(SeekFrom::Start(0))
            .map_err(|_| Report::new(Error::Seek))?;

        match mbr::read(&mut file) {
            Ok(table) => Ok(table),
            Err(mbr_err) => Err(Report::new(Error::NoValidTable {
                gpt: gpt_err,
                mbr: mbr_err,
            })),
        }
    }

    fn detect_fs_kind(&self, image: &Path, partition_offset: u64) -> Result<FsKind> {
        detect_fs_kind(image, partition_offset)
    }
}

// ext4: superblock magic, same layout as remora-etcher-image-adapter-ext4.
const EXT4_SUPERBLOCK_OFFSET: u64 = 1024;
const EXT4_MAGIC_OFFSET: u64 = 0x38;
const EXT4_MAGIC: u16 = 0xEF53;

// FAT12/16/32 boot sector (BPB): a fixed 0x55AA signature at the very end of
// the first sector, plus a `"FATxx   "` filesystem-type string whose offset
// depends on whether it's FAT12/16 (0x36) or FAT32 (0x52) — checking both
// locations avoids needing to first decide which FAT variant this is.
const FAT_BOOT_SECTOR_SIGNATURE_OFFSET: usize = 0x1FE;
const FAT_BOOT_SECTOR_SIGNATURE: [u8; 2] = [0x55, 0xAA];
const FAT16_FILSYSTYPE_OFFSET: usize = 0x36;
const FAT32_FILSYSTYPE_OFFSET: usize = 0x52;

/// Determine whether the partition at `partition_offset` inside `image` is
/// ext4 or vfat by reading its own on-disk signature, rather than inferring
/// it from a boot-mode label the caller would otherwise have to supply.
fn detect_fs_kind(image: &Path, partition_offset: u64) -> Result<FsKind> {
    let mut file = File::open(image).map_err(|_| Report::new(Error::Open(image.to_path_buf())))?;

    let mut magic = [0u8; 2];
    file.seek(SeekFrom::Start(
        partition_offset + EXT4_SUPERBLOCK_OFFSET + EXT4_MAGIC_OFFSET,
    ))
    .and_then(|_| file.read_exact(&mut magic))
    .map_err(|_| Report::new(Error::Seek))?;
    if u16::from_le_bytes(magic) == EXT4_MAGIC {
        return Ok(FsKind::Ext4);
    }

    let mut boot_sector = [0u8; 512];
    file.seek(SeekFrom::Start(partition_offset))
        .and_then(|_| file.read_exact(&mut boot_sector))
        .map_err(|_| Report::new(Error::Seek))?;
    let has_boot_signature = boot_sector
        [FAT_BOOT_SECTOR_SIGNATURE_OFFSET..FAT_BOOT_SECTOR_SIGNATURE_OFFSET + 2]
        == FAT_BOOT_SECTOR_SIGNATURE;
    let looks_like_fat = |offset: usize| &boot_sector[offset..offset + 3] == b"FAT";
    if has_boot_signature
        && (looks_like_fat(FAT16_FILSYSTYPE_OFFSET) || looks_like_fat(FAT32_FILSYSTYPE_OFFSET))
    {
        return Ok(FsKind::Vfat);
    }

    Err(Report::new(Error::UnrecognizedFilesystem {
        path: image.to_path_buf(),
        offset: partition_offset,
    }))
}

#[cfg(test)]
mod fs_kind_tests {
    use super::*;
    use std::{io::Write, path::PathBuf, process::Command};

    fn write_temp(bytes: &[u8]) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "remora-etcher-fs-kind-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        File::create(&path).unwrap().write_all(bytes).unwrap();
        path
    }

    #[test]
    fn detects_ext4_from_a_synthetic_superblock() {
        let mut buf = vec![0u8; 2048];
        let magic_at = (EXT4_SUPERBLOCK_OFFSET + EXT4_MAGIC_OFFSET) as usize;
        buf[magic_at..magic_at + 2].copy_from_slice(&EXT4_MAGIC.to_le_bytes());
        let path = write_temp(&buf);
        assert_eq!(detect_fs_kind(&path, 0).unwrap(), FsKind::Ext4);
    }

    #[test]
    fn detects_vfat_from_a_synthetic_fat32_boot_sector() {
        let mut buf = vec![0u8; 2048];
        buf[FAT_BOOT_SECTOR_SIGNATURE_OFFSET..FAT_BOOT_SECTOR_SIGNATURE_OFFSET + 2]
            .copy_from_slice(&FAT_BOOT_SECTOR_SIGNATURE);
        buf[FAT32_FILSYSTYPE_OFFSET..FAT32_FILSYSTYPE_OFFSET + 8].copy_from_slice(b"FAT32   ");
        let path = write_temp(&buf);
        assert_eq!(detect_fs_kind(&path, 0).unwrap(), FsKind::Vfat);
    }

    #[test]
    fn refuses_an_unformatted_region() {
        let path = write_temp(&vec![0u8; 2048]);
        assert!(matches!(
            detect_fs_kind(&path, 0).unwrap_err().current_context(),
            Error::UnrecognizedFilesystem { .. }
        ));
    }

    #[test]
    fn detects_ext4_and_vfat_from_real_mkfs_output() {
        // Dev-only validation against the real tools, same posture as the
        // ext4 round-trip matrix: never shelled out to by the shipped
        // binary, only by this test.
        let ext4_path = write_temp(&[]);
        std::fs::File::create(&ext4_path)
            .unwrap()
            .set_len(1024 * 1024)
            .unwrap();
        let status = Command::new("mke2fs")
            .args(["-F", "-t", "ext4", "-q"])
            .arg(&ext4_path)
            .status()
            .expect("mke2fs not available");
        assert!(status.success());
        assert_eq!(detect_fs_kind(&ext4_path, 0).unwrap(), FsKind::Ext4);

        let vfat_path = write_temp(&[]);
        std::fs::File::create(&vfat_path)
            .unwrap()
            .set_len(1024 * 1024)
            .unwrap();
        let status = Command::new("mkfs.vfat")
            .arg(&vfat_path)
            .status()
            .expect("mkfs.vfat not available");
        assert!(status.success());
        assert_eq!(detect_fs_kind(&vfat_path, 0).unwrap(), FsKind::Vfat);
    }
}
