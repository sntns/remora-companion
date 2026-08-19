# remora-etcher

A standalone, cross-platform (Linux/Windows/macOS) provisioning and flashing
tool for Remora devices — a bit like balena-etcher, but for Remora. No
dependency on separately-installed third-party utilities (no `mksquashfs`,
`mkfs.ext4`, `dd`, `bmaptool`, `parted`, `e2fsprogs`...): everything is
implemented in Rust.

## Status

Early scaffold. Implemented so far:

- `remora-etcher squashfs build` / `squashfs inspect` — build a squashfs image
  from a list of files/directories, parameter-compatible with the defaults
  `oe_mksquashfs` uses to build the Remora rootfs (gzip, 128 KiB blocks, real
  uid/gid/mode preserved).
- `remora-etcher disk list` / `disk info` / `disk flash` — enumerate disks
  (Linux only for now, via `/sys/block` + `/proc/self/mountinfo`, no
  shell-out) and flash an image bmaptool-style via the `bmap-parser` crate
  (sparse-aware, checksum-verified when a `.bmap` is given). Refuses to
  overwrite what looks like the system disk, and refuses a non-removable
  disk unless `--force`; also prompts for the device path to be typed back
  unless `--yes`.
- `remora-etcher image inspect` / `image partition list` — read an image's or
  device's MBR/GPT partition table (auto-detected via `mbrman`/`gptman`,
  pure Rust) and, with `--boot-mode efi|bios|uboot|rpi`, annotate each
  partition with its Remora role (shared/efi/slotA/slotB/data) per
  meta-remora's `REMORA_PART_*_INDEX` tables.
- `remora-etcher image partition cp <src> <dest-path> --image <path>
  --partition data|<index> [--boot-mode ...]` — copy a local file into an
  already-existing directory inside one partition's ext4 filesystem (e.g.
  the `data` partition), via the pure-Rust `am-fs-ext4` crate. No temporary
  extraction: it mounts a byte-range window directly inside the larger disk
  image. Validated against `am-fs-ext4`'s real production feature
  combination (metadata_csum + 64bit + uninit_bg, matching
  `remora-mount`'s own `tune2fs` call) with a real `e2fsck -f` pass — see the
  "spike: validate am-fs-ext4 for phase 3" commit for the validation spike
  and its findings, including an upstream aarch64 build fix
  ([PR #36](https://github.com/christhomas/rust-fs-ext4/pull/36)) tracked
  via our fork until it merges and releases.

Not yet implemented (see the project plan): field validation against an
actual meta-remora-produced wic image (no built image was available while
developing this), and Windows/macOS disk support.

## Workspace layout

- `crates/core` (`remora-etcher-core`) — library: `model` (pure data types),
  `application` (use-cases), `adapter` (third-party crate wrappers, e.g.
  `backhand` for squashfs).
- `crates/cli` (`remora-etcher`) — the CLI binary.

## Building

```
cargo build --workspace
cargo test --workspace
```

## CI / packaging

- `.github/workflows/ci.yml` — build/test/clippy/fmt on ubuntu/windows/macos
  for the workspace (`crates/core` + `crates/cli`, fully portable — no
  external tool is shelled out to by that code or its tests).
- `.github/workflows/release.yml` — on a `v*` tag, builds release binaries
  for `x86_64`/`aarch64` Linux, `x86_64` Windows, and `x86_64`/`aarch64`
  macOS, packages them (`.tar.gz`/`.zip`, plus a `.deb` for `x86_64` Linux
  via `cargo-deb`), and attaches them to a draft GitHub release.
- Linux `.deb` packaging is driven by `[package.metadata.deb]` in
  `crates/cli/Cargo.toml` (mirrors `remora-disk`'s own metadata) — validated
  locally with `cargo deb -p remora-etcher`.
