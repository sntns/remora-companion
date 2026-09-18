# remora-etcher

[![CI](https://github.com/sntns/remora-etcher/actions/workflows/ci.yml/badge.svg)](https://github.com/sntns/remora-etcher/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

A standalone, cross-platform (Linux/Windows/macOS) provisioning and flashing
tool for Remora devices — a bit like balena-etcher, but for Remora. No
dependency on separately-installed third-party utilities (no `mksquashfs`,
`mkfs.ext4`, `dd`, `bmaptool`, `parted`, `e2fsprogs`...): everything is
implemented in Rust.

## Installation

On Linux or macOS, install the latest release binary with:

```
curl -fsSL https://raw.githubusercontent.com/sntns/remora-etcher/main/install.sh | bash
```

This detects your OS/arch, downloads the matching archive from the
[latest GitHub release](https://github.com/sntns/remora-etcher/releases/latest),
and installs `remora-etcher` into `/usr/local/bin` (or `~/.local/bin` if
that isn't writable). Install a specific version instead of the latest
with `REMORA_ETCHER_VERSION=vX.Y.Z`, or change the install location with
`REMORA_ETCHER_INSTALL_DIR=/path`.

On Windows, or if you'd rather not pipe a script into `bash`, grab the
matching archive/`.deb` directly from the
[releases page](https://github.com/sntns/remora-etcher/releases) instead.

## Features

### Flash an image to a USB stick or SD card

```
remora-etcher flash --image remora.wic --device /dev/sdb
```

Sparse-aware and checksum-verified whenever a `.bmap` file sits next to the
image (auto-discovered as `<image>.bmap`, same convention as `bmaptool`;
skip it with `--no-bmap`). Refuses to overwrite what looks like the system
disk, refuses a non-removable disk unless you pass `--force`, and makes you
type the device path back to confirm — unless `--yes`, for scripted use.

### List and inspect disks

```
remora-etcher disk list          # removable disks only
remora-etcher disk list --all    # every disk
remora-etcher disk info /dev/sdb
```

Linux only for now — Windows/macOS disk enumeration isn't implemented yet.

### Inspect an image's partitions

```
remora-etcher image inspect remora.wic --boot-mode efi
remora-etcher image partition list remora.wic --boot-mode efi
```

Works on a raw image file or directly on a block device. With
`--boot-mode efi|bios|uboot|rpi`, each partition is labeled with its Remora
role (shared/efi/slotA/slotB/data).

### Provision an image before you flash it

Copy a file or a whole directory straight into one partition's filesystem —
no mounting, no loopback devices, no root required:

```
remora-etcher image partition cp ./my-config.json /play/tplst-app-config/config.json \
  --image remora.wic --partition data

remora-etcher image partition mkdir /play/tplst-app-config \
  --image remora.wic --partition shared --boot-mode efi
```

Works against ext4 or vfat partitions, auto-detected from the partition
itself — you don't need to know which.

### Give a device its identity

```
remora-etcher identity create ./ssh-keys --image remora.wic --hostname my-device
```

Builds `identity.squashfs` — hostname, machine-id, and an ed25519 SSH host
keypair (generated for you unless you supply one) — and injects it into the
image's shared partition in one step. Use `identity build` instead if you
just want the squashfs file, without touching an image.

### Configure a device

```
remora-etcher config upload ./timezone /timezone --image remora.wic
```

Adds or updates a single file inside the image's config partition,
creating a fresh `config.ext4` first if one doesn't exist yet. Use
`config build` to build a standalone `config.ext4` from a whole directory
instead.

### Build a rootfs image

```
remora-etcher squashfs build ./rootfs --output rootfs.squashfs
remora-etcher squashfs inspect rootfs.squashfs
```

Parameter-compatible with the defaults `oe_mksquashfs` uses to build the
Remora rootfs (gzip, 128 KiB blocks, real uid/gid/mode preserved).

### Convert between image formats

```
remora-etcher convert to-raw remora.wic.qcow2 --output remora.wic
remora-etcher convert from-raw remora.wic --output remora.wic.gz
```

Reads and writes qcow2 and gzip directly, with the format picked from each
path's own extension — no `qemu-img` or `gzip` binary required.

---

Not yet implemented: field validation against an actual meta-remora-produced
wic image, and Windows/macOS disk support.

## Architecture

DDD-style, matching the [remora-edge](https://github.com/sntns/remora-edge)
convention: one Cargo workspace, one `components/<vertical>` crate per
bounded context (`disk`, `flash`, `image`, `squashfs`, `identity`, `config`,
`convert`), each split further into:

- `components/<vertical>` — the domain crate: pure model types and the
  `*Adapter`/`*ServiceInterface` port traits (no I/O, no third-party
  infrastructure crates beyond inert value types like a parsed `.bmap`).
- `components/<vertical>-application` — the use case, implemented against
  injected ports only.
- `components/<vertical>-adapter-<name>` — a concrete port implementation
  (e.g. `-adapter-ext4` wraps `am-fs-ext4`; `convert` has two, one per
  container format, both implementing the same `ContainerFormatAdapter`
  port).
- `components/<vertical>-application-transport-cli` — the `clap` subcommands
  for that vertical.

Not every vertical needs all four: `identity` and `config` have no
filesystem adapter of their own — they inject the already-wired `image`
vertical's `ImageService` instead (see `containers/remora-etcher/src/bootstrap.rs`),
since writing into a partition is `image`'s job either way.

Three small shared utility crates with no vertical prefix
(`remora-etcher-fs-walk`, `remora-etcher-scratch`, `remora-etcher-format`)
mirror remora-edge's own `components/store`/`config` convention.
`containers/remora-etcher` is the single binary: a composition root that
wires every adapter and use case together via
[`busybody`](https://docs.rs/busybody) (the same DI crate remora-edge uses),
then dispatches CLI subcommands into them. Errors propagate as
[`error-stack`](https://docs.rs/error-stack) `Report`s end to end, so a
failure prints its full cause chain with file:line at every layer.

See each vertical's `components/<vertical>-application` crate for its
integration tests (real adapters, real `mke2fs`/`mkfs.vfat`/`sfdisk`/
`fsck.ext4`/`fsck.vfat`/`unsquashfs`/`gzip`/`qemu-img` fixtures where
relevant — dev-only tools, never shelled out to by the shipped binary).

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
