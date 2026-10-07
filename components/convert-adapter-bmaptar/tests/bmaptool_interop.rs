//! A `.bmaptar` both ways against the real tools meta-remora builds one
//! with (`bmaptool`, `zstd`, `tar`): ours must be what they'd make, and
//! theirs must read back. Dev-only tools, never shelled out to by the
//! shipped binary; a test needing one that's missing says so and passes.
//!
//! Sparse fixtures, holes and all: Linux only, like the tools.
#![cfg(target_os = "linux")]

use std::{
    fs::{self, File},
    io::Read,
    os::unix::fs::{FileExt, MetadataExt},
    path::Path,
    process::Command,
};

use remora_convert::adapter::{ContainerFormatAdapter, Error};
use remora_convert_adapter_bmaptar::BmaptarAdapterImpl;
use remora_scratch::ScratchDir;

const BLOCK: u64 = 4096;
const MIB: u64 = 1024 * 1024;

/// Whether `tool` can be run; when not, the test calling it is skipped.
fn have(tool: &str) -> bool {
    let found = Command::new(tool).arg("--version").output().is_ok();
    if !found {
        eprintln!("skipped: no {tool} binary");
    }
    found
}

fn run(command: &mut Command) {
    let status = command.status().unwrap();
    assert!(status.success(), "{command:?} failed");
}

/// A sparse disk image of `len` bytes: holes, but for a few data ranges,
/// one of them written with zeros (data all the same, for a disk).
fn sparse_image(path: &Path, len: u64) {
    let file = File::create(path).unwrap();
    file.set_len(len).unwrap();
    file.write_all_at(b"boot sector", 0).unwrap();
    file.write_all_at(&[0; 2 * BLOCK as usize], 16 * BLOCK)
        .unwrap();
    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    file.write_all_at(&data, MIB + 100).unwrap();
    file.write_all_at(&[0xee; 3], len - 3).unwrap();
}

/// The `.bmap` a bundle holds.
fn bundled_bmap(bundle: &Path) -> String {
    remora_unpack::open_path(bundle)
        .unwrap()
        .bundled_bmap
        .unwrap()
}

/// A tar's member names, in order.
fn members(bundle: &Path) -> Vec<String> {
    tar::Archive::new(File::open(bundle).unwrap())
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().display().to_string())
        .collect()
}

#[test]
fn round_trips_a_sparse_image() {
    let dir = ScratchDir::new("remora-bmaptar-test").unwrap();
    let raw = dir.join("disk.wic");
    // Not a whole number of blocks: the last one is cut short.
    sparse_image(&raw, 5 * MIB + 1234);

    let bundle = dir.join("disk.wic.bmaptar");
    BmaptarAdapterImpl.encode_from_raw(&raw, &bundle).unwrap();
    let back = dir.join("back.wic");
    BmaptarAdapterImpl.decode_to_raw(&bundle, &back).unwrap();

    assert_eq!(fs::read(&back).unwrap(), fs::read(&raw).unwrap());
    assert_eq!(members(&bundle), ["disk.wic.bmap", "disk.wic.zst"]);
    assert_eq!(
        BmaptarAdapterImpl.decoded_size(&bundle).unwrap(),
        Some(5 * MIB + 1234)
    );
    // The holes stay holes: only the mapped blocks were written.
    let allocated = fs::metadata(&back).unwrap().blocks() * 512;
    assert!(allocated < MIB, "{allocated} bytes allocated");

    // The blocks written with zeros are mapped like any data.
    let bmap = bmap_parser::Bmap::from_xml(&bundled_bmap(&bundle)).unwrap();
    let ranges: Vec<_> = bmap
        .block_map()
        .map(|r| {
            (
                r.offset() / BLOCK,
                (r.offset() + r.length()).div_ceil(BLOCK) - 1,
            )
        })
        .collect();
    let last = (5 * MIB + 1234) / BLOCK;
    assert_eq!(ranges, [(0, 0), (16, 17), (256, 329), (last, last)]);
}

#[test]
fn the_bmap_is_bmaptools_own() {
    if !have("bmaptool") {
        return;
    }
    let dir = ScratchDir::new("remora-bmaptar-test").unwrap();
    let raw = dir.join("disk.wic");
    // A whole number of blocks: bmaptool rounds a cut-short last block
    // differently depending on how it maps the file (FIEMAP or SEEK_HOLE).
    sparse_image(&raw, 5 * MIB);
    let theirs = dir.join("disk.wic.bmap");
    run(Command::new("bmaptool")
        .arg("-q")
        .arg("create")
        .arg(&raw)
        .arg("-o")
        .arg(&theirs));

    let bundle = dir.join("disk.wic.bmaptar");
    BmaptarAdapterImpl.encode_from_raw(&raw, &bundle).unwrap();

    assert_eq!(bundled_bmap(&bundle), fs::read_to_string(&theirs).unwrap());
}

#[test]
fn the_real_tools_read_it_as_meta_remoras() {
    if !(have("tar") && have("zstd") && have("bmaptool")) {
        return;
    }
    let dir = ScratchDir::new("remora-bmaptar-test").unwrap();
    let raw = dir.join("disk.wic");
    sparse_image(&raw, 5 * MIB + 1234);
    let bundle = dir.join("laplaylist-image-f3apl.wic.bmaptar");
    BmaptarAdapterImpl.encode_from_raw(&raw, &bundle).unwrap();

    let listing = Command::new("tar")
        .arg("tvf")
        .arg(&bundle)
        .arg("--numeric-owner")
        .output()
        .unwrap();
    assert!(listing.status.success());
    let listing = String::from_utf8(listing.stdout).unwrap();
    let lines: Vec<_> = listing.lines().collect();
    assert_eq!(lines.len(), 2, "{listing}");
    assert!(lines[0].starts_with("-rw-r--r-- 0/0"), "{listing}");
    assert!(
        lines[0].ends_with(" laplaylist-image-f3apl.wic.bmap"),
        "{listing}"
    );
    assert!(
        lines[1].ends_with(" laplaylist-image-f3apl.wic.zst"),
        "{listing}"
    );
    // GNU tar's own record size.
    assert_eq!(fs::metadata(&bundle).unwrap().len() % 10240, 0);

    let extracted = dir.join("extracted");
    fs::create_dir(&extracted).unwrap();
    run(Command::new("tar")
        .arg("xf")
        .arg(&bundle)
        .arg("-C")
        .arg(&extracted));
    let image = extracted.join("laplaylist-image-f3apl.wic.zst");
    run(Command::new("zstd").arg("-tq").arg(&image));
    // As meta-remora's bbclass says to flash it by hand: bmaptool finds the
    // .bmap next to the image, checks it and every range's checksum.
    let flashed = dir.join("flashed.wic");
    run(Command::new("bmaptool")
        .arg("-q")
        .arg("copy")
        .arg(&image)
        .arg(&flashed));
    assert_eq!(fs::read(&flashed).unwrap(), fs::read(&raw).unwrap());
}

#[test]
fn reads_a_bmaptar_made_the_meta_remora_way() {
    if !(have("tar") && have("zstd") && have("bmaptool")) {
        return;
    }
    let dir = ScratchDir::new("remora-bmaptar-test").unwrap();
    let raw = dir.join("image.wic");
    sparse_image(&raw, 5 * MIB + 1234);
    // remora-bmaptar.bbclass's CONVERSION_CMD, with oe-core's zst one.
    run(Command::new("bmaptool").current_dir(dir.path()).args([
        "-q",
        "create",
        "image.wic",
        "-o",
        "image.wic.bmap",
    ]));
    run(Command::new("sh").current_dir(dir.path()).args([
        "-c",
        "zstd -f -k -c --threads=2 -3 image.wic > image.wic.zst",
    ]));
    run(Command::new("tar").current_dir(dir.path()).args([
        "-cf",
        "image.wic.bmaptar",
        "--owner=0",
        "--group=0",
        "--numeric-owner",
        "image.wic.bmap",
        "image.wic.zst",
    ]));
    let bundle = dir.join("image.wic.bmaptar");
    assert!(BmaptarAdapterImpl.recognizes(&fs::read(&bundle).unwrap()[..512]));

    let back = dir.join("back.wic");
    BmaptarAdapterImpl.decode_to_raw(&bundle, &back).unwrap();

    assert_eq!(fs::read(&back).unwrap(), fs::read(&raw).unwrap());
}

#[test]
fn an_image_not_matching_its_bmap_is_refused() {
    let dir = ScratchDir::new("remora-bmaptar-test").unwrap();
    let (a, b) = (dir.join("a.wic"), dir.join("b.wic"));
    sparse_image(&a, 2 * MIB);
    sparse_image(&b, 2 * MIB);
    File::options()
        .write(true)
        .open(&b)
        .unwrap()
        .write_all_at(b"tampered", MIB + 1000)
        .unwrap();
    let (a_bundle, b_bundle) = (dir.join("a.wic.bmaptar"), dir.join("b.wic.bmaptar"));
    BmaptarAdapterImpl.encode_from_raw(&a, &a_bundle).unwrap();
    BmaptarAdapterImpl.encode_from_raw(&b, &b_bundle).unwrap();

    // a's .bmap, b's image.
    let member = |bundle: &Path, index: usize| {
        let mut archive = tar::Archive::new(File::open(bundle).unwrap());
        let mut entry = archive.entries().unwrap().nth(index).unwrap().unwrap();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        data
    };
    let mixed = dir.join("mixed.wic.bmaptar");
    let mut builder = tar::Builder::new(File::create(&mixed).unwrap());
    for (name, data) in [
        ("mixed.wic.bmap", member(&a_bundle, 0)),
        ("mixed.wic.zst", member(&b_bundle, 1)),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, &data[..]).unwrap();
    }
    builder.finish().unwrap();

    let err = BmaptarAdapterImpl
        .decode_to_raw(&mixed, &dir.join("back.wic"))
        .unwrap_err();
    assert!(
        matches!(err.current_context(), Error::Checksum(_)),
        "{err:?}"
    );
}

#[test]
fn a_tar_without_a_bmap_is_not_a_bmaptar() {
    let dir = ScratchDir::new("remora-bmaptar-test").unwrap();
    let bundle = dir.join("disk.wic.bmaptar");
    let mut builder = tar::Builder::new(File::create(&bundle).unwrap());
    let mut header = tar::Header::new_gnu();
    header.set_size(5);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, "disk.wic", &b"hello"[..])
        .unwrap();
    builder.finish().unwrap();

    let err = BmaptarAdapterImpl
        .decode_to_raw(&bundle, &dir.join("back.wic"))
        .unwrap_err();
    assert!(matches!(err.current_context(), Error::InvalidHeader(_)));
}

#[test]
fn an_image_of_holes_round_trips() {
    let dir = ScratchDir::new("remora-bmaptar-test").unwrap();
    let raw = dir.join("disk.wic");
    File::create(&raw).unwrap().set_len(MIB).unwrap();

    let bundle = dir.join("disk.wic.bmaptar");
    BmaptarAdapterImpl.encode_from_raw(&raw, &bundle).unwrap();
    let back = dir.join("back.wic");
    BmaptarAdapterImpl.decode_to_raw(&bundle, &back).unwrap();

    assert_eq!(fs::read(&back).unwrap(), vec![0; MIB as usize]);
}
