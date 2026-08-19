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

Not yet implemented (see the project plan): MBR/GPT partition-table parsing,
injecting files into a wic image's `data` partition, and Windows/macOS disk
support.

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
