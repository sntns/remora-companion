# remora-etcher

[![CI](https://github.com/sntns/remora-etcher/actions/workflows/ci.yml/badge.svg)](https://github.com/sntns/remora-etcher/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

A standalone, cross-platform (Linux/Windows/macOS) provisioning and flashing
tool for Remora devices — a bit like balena-etcher, but for Remora. No
dependency on separately-installed third-party utilities (no `mksquashfs`,
`mkfs.ext4`, `dd`, `bmaptool`, `parted`, `e2fsprogs`...): everything is
implemented in Rust.

## Status

Implemented:

- `remora-etcher squashfs build` / `squashfs inspect` — build a squashfs image
  from a list of files/directories, parameter-compatible with the defaults
  `oe_mksquashfs` uses to build the Remora rootfs (gzip, 128 KiB blocks, real
  uid/gid/mode preserved).
- `remora-etcher disk list` / `disk info` — enumerate disks (Linux only for
  now, via `/sys/block` + `/proc/self/mountinfo`, no shell-out).
- `remora-etcher flash` — flash an image to a disk bmaptool-style via the
  `bmap-parser` crate (sparse-aware, checksum-verified when a `.bmap` is
  given). Refuses to overwrite what looks like the system disk, and refuses
  a non-removable disk unless `--force`; also prompts for the device path to
  be typed back unless `--yes`.
- `remora-etcher image inspect` / `image partition list` — read an image's or
  device's MBR/GPT partition table (auto-detected via `mbrman`/`gptman`,
  pure Rust) and, with `--boot-mode efi|bios|uboot|rpi`, annotate each
  partition with its Remora role (shared/efi/slotA/slotB/data) per
  meta-remora's `REMORA_PART_*_INDEX` tables.
- `remora-etcher image partition cp <src> <dest-path> --image <path>
  --partition data|<index> [--boot-mode ...]` / `image partition mkdir` —
  copy a local file into (or create a directory inside) one partition's
  ext4 or vfat filesystem, auto-detected from the partition's own on-disk
  signature. No temporary extraction: it mounts a byte-range window
  directly inside the larger disk image, via the pure-Rust `am-fs-ext4` and
  `fatfs` crates.
- `remora-etcher identity build` / `identity create` — build
  `identity.squashfs` (hostname/machine-id/an ed25519 SSH host keypair,
  generated unless already supplied) and inject it into an image's shared
  partition.
- `remora-etcher config build` / `config upload` — build a standalone
  `config.ext4` image, or add/update one file inside an image's
  `shared:/remora/<slot>/config` (building a fresh `config.ext4` first if it
  doesn't exist yet).

Not yet implemented: field validation against an actual meta-remora-produced
wic image, and Windows/macOS disk support.

## Architecture

DDD-style, matching the [remora-edge](https://github.com/sntns/remora-edge)
convention: one Cargo workspace, one `components/<vertical>` crate per
bounded context (`disk`, `flash`, `image`, `squashfs`, `identity`, `config`),
each split further into:

- `components/<vertical>` — the domain crate: pure model types and the
  `*Adapter`/`*ServiceInterface` port traits (no I/O, no third-party
  infrastructure crates beyond inert value types like a parsed `.bmap`).
- `components/<vertical>-application` — the use case, implemented against
  injected ports only.
- `components/<vertical>-adapter-<name>` — a concrete port implementation
  (e.g. `-adapter-ext4` wraps `am-fs-ext4`).
- `components/<vertical>-application-transport-cli` — the `clap` subcommands
  for that vertical.

Two small shared utility crates with no vertical prefix (`remora-etcher-fs-walk`,
`remora-etcher-scratch`) mirror remora-edge's own `components/store`/`config`
convention. `containers/remora-etcher` is the single binary: a composition
root that wires every adapter and use case together via
[`busybody`](https://docs.rs/busybody) (the same DI crate remora-edge uses),
then dispatches CLI subcommands into them. Errors propagate as
[`error-stack`](https://docs.rs/error-stack) `Report`s end to end, so a
failure prints its full cause chain with file:line at every layer.

See each vertical's `components/<vertical>-application` crate for its
integration tests (real adapters, real `mke2fs`/`mkfs.vfat`/`sgdisk`/`sfdisk`
fixtures where relevant — dev-only tools, never shelled out to by the
shipped binary).

## Building

```
cargo build --workspace
cargo test --workspace
```

## CI / packaging

- `.github/workflows/ci.yml` — build/test/clippy/fmt on ubuntu/windows/macos
  for the whole workspace.
- `.github/workflows/release.yml` — on a `v*` tag, builds release binaries
  for `x86_64`/`aarch64` Linux, `x86_64` Windows, and `x86_64`/`aarch64`
  macOS, packages them (`.tar.gz`/`.zip`, plus a `.deb` for `x86_64` Linux
  via `cargo-deb`), and attaches them to a draft GitHub release.
- Linux `.deb` packaging is driven by `[package.metadata.deb]` in
  `containers/remora-etcher/Cargo.toml` (mirrors `remora-disk`'s own
  metadata) — validated locally with `cargo deb -p remora-etcher`.

## Contributing

Contributions are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md) for the
build/test/lint sequence CI expects and the architecture conventions to
follow, and [CLAUDE.md](CLAUDE.md) for the full DDD/error-handling
convention. This project follows the
[Contributor Covenant](CODE_OF_CONDUCT.md).

## Security

See [SECURITY.md](SECURITY.md) for how to report a vulnerability.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
