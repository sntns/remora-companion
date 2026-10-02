# remora-companion

[![CI](https://github.com/sntns/remora-companion/actions/workflows/ci.yml/badge.svg)](https://github.com/sntns/remora-companion/actions/workflows/ci.yml)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

The operator's toolbox for Remora devices, cross-platform
(Linux/Windows/macOS). Two binaries, built from one workspace of shared
components:

- **[`rmra`](#rmra)** — operate your devices through
  [sntns-platform](https://github.com/sntns/sntns-platform): log in,
  switch between accounts docker-context style, `rmra ssh <device>`
  through the device's remora channel, and publish over-the-air updates
  and roll them out (`rmra release`, `rmra deploy`).
- **[`remora-etcher`](#remora-etcher)** — provision and flash images, a bit
  like balena-etcher but for Remora. No dependency on separately-installed
  third-party utilities (no `mksquashfs`, `mkfs.ext4`, `dd`, `bmaptool`,
  `parted`, `e2fsprogs`...): everything is implemented in Rust.

## Installation

From the [latest release](https://github.com/sntns/remora-companion/releases/latest):

macOS / Linux:

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/sntns/remora-companion/releases/latest/download/rmra-installer.sh | sh
```

Windows (PowerShell):

```
powershell -ExecutionPolicy Bypass -c "irm https://github.com/sntns/remora-companion/releases/latest/download/rmra-installer.ps1 | iex"
```

Homebrew:

```
brew install sntns/tap/rmra
```

Use `remora-etcher` in place of `rmra` for the image tool. The installers
put the binary in `~/.local/bin` (`RMRA_INSTALL_DIR=/path` to choose) and
add it to your PATH. Linux binaries are static (musl): one binary for any
distribution.

`rmra update` installs the latest release over the running one, the way it
was installed (`rmra update --check` only tells); a Homebrew install is
updated with `brew upgrade sntns/tap/rmra` instead.

## rmra

### Shell completion

```
rmra completion            # shows the line to add for your shell, e.g. for zsh:
echo 'source <(COMPLETE=zsh rmra)' >> ~/.zshrc
```

Bash, zsh, fish, elvish and PowerShell. Beyond commands and options, Tab
completes real values: context names (`rmra -c <Tab>`, `rmra context use
<Tab>`), device serials (`rmra ssh <Tab>`, `rmra deploy --device <Tab>`,
`rmra scp ./file <Tab>` → `DEVICE:`), releases and deployments. Remote
values come from the selected context's platform (the `-c` on the line
being completed counts), cached for a minute; the completion is generated
on shell start, so it always matches the installed rmra.

### Log in

```
rmra login
```

Guided on a terminal: on a first run it creates a context (a named gateway
endpoint, `api.eu2.sntns.io:50051` by default), then asks how to log in —
an access key's token, or a login profile (user URN and password). The
credentials are checked against the platform before anything is stored.
For scripts: `rmra login --token-stdin`, or `--identity <urn>
--password-stdin`. `rmra whoami` shows who you are logged in as, `rmra
logout` forgets it.

### Switch between accounts

Like `docker context`: one context per platform account or environment,
each with its own login.

```
rmra context create staging --address api.staging.example:50051
rmra context ls
rmra context use staging
rmra --context eu2 whoami          # just this once; or RMRA_CONTEXT=eu2
```

Contexts live in `~/.config/rmra` (`%APPDATA%\rmra` on Windows; override
with `RMRA_CONFIG`): `contexts/<name>/meta.json` for the endpoint,
`contexts/<name>/credentials.json` (mode 0600) for the login, kept apart so
`context inspect`/`ls` never print a secret. Credentials are stored in the
clear for now, like the `sntns` CLI's own configuration; an OS-keyring
store is planned.

### List devices

```
rmra device ls
rmra device ls --label site=lyon
rmra device ls -q                 # names only, one per line
```

The account's devices and their labels (`--format json` for scripts). The
same `--label key=value` pairs pick the targets of `rmra deploy --selector`.

### ssh into a device

```
rmra ssh 525400C0FFEE
rmra ssh 525400C0FFEE --role admin -- journalctl -fu remora-accessd
rmra ssh 525400C0FFEE -L 8080:127.0.0.1:80
```

Each session generates a throwaway ed25519 key, has the platform certify it
for 15 minutes for one role on that device (`user` or `admin`, each its own
IAM action), and runs your `ssh` with `rmra channel open` as its
ProxyCommand and a known_hosts that trusts only the account's host
authority. Nothing is written to `~/.ssh`, nothing listens on your machine,
and the key material is removed when ssh exits. Arguments after the device
go to ssh; `-J`, `-W`, `-F` and `-o ProxyCommand`/`ProxyJump` are refused
since they would bypass the channel or the host pinning. Port forwarding,
`scp` and `sftp` all work through ssh itself.

### Copy files to or from a device

```
rmra scp ./bundle.raucb 525400C0FFEE:/data/
rmra scp -r root@525400C0FFEE:/var/log ./logs
```

`rmra ssh`'s setup for scp: the same throwaway certified key, channel and
host pinning, with the device side written `[user@]DEVICE:path` (one device
per copy). scp's options pass through, except those that would bypass the
channel or the pinning (`-J`, `-F`, `-S`, `-o ProxyCommand`/`ProxyJump`);
`-c`/`-v` are rmra's own, so pass scp's after `--`.

`rmra channel open <device>` is the raw channel on stdin/stdout, for use as
a ProxyCommand of your own.

### Publish an update

```
rmra release create 2026.10.0 --version 2026.10.0 --label channel=beta \
  --artifact remora-rp5.raucb --tag-condition "board:rp5"
rmra release upload 2026.10.0 remora-hdc.raucb --tag-condition "board:hdc"
rmra release ls
rmra release show 2026.10.0
```

A release is a version and its artifacts (RAUC bundles); each artifact's
`--tag-condition` says which devices it is for, as a boolean expression
over device tags (`type:rauc && (board:hdc || board:rp5)`). Uploads show a
live byte bar and resume on their own after a dropped connection; if a run
is interrupted, it prints a token to continue with `--resume <token>`.

### Roll it out and follow it

```
rmra deploy 2026.10.0 --device 525400C0FFEE --watch
rmra deploy 2026.10.0 --selector site=lyon --watch      # lists the devices and asks first
rmra deploy 2026.10.0 --selector site=lyon --draft --yes
rmra deployment start 2026.10.0-525400c0ffee --watch
rmra deployment ls --release 2026.10.0
rmra deployment show 2026.10.0-525400c0ffee
rmra deployment logs 2026.10.0-525400c0ffee --follow
rmra deployment cancel 2026.10.0-525400c0ffee
```

One deployment per device, named `<release>-<serial>`, started right away
unless `--draft`. A device that can't take it (one already has an update in
flight) is reported without stopping the others. `--watch` (or `rmra
deployment watch <names...>`) draws one live line per deployment — status,
progress, the device's latest report — and exits non-zero if any of them
doesn't succeed; Ctrl-C stops watching, not the deployments. Every listing
takes `--format json` for scripts.

## remora-etcher

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
bounded context — `disk`, `flash`, `image`, `squashfs`, `identity`,
`config`, `convert`, `batch` and `factory` for remora-etcher, `context`,
`channel`, `ota` and `device` for rmra. Components are generic, not owned by a binary: the
directory is `components/<vertical>`, the package `remora-<vertical>`. Each
is split further into:

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

Small shared utility crates with no vertical prefix (`remora-fs-walk`,
`remora-scratch`, `remora-format`, `remora-progress`, and for rmra
`remora-platform-grpc` — the vendored sntns-platform protos, compiled with
the pure-Rust [protox](https://docs.rs/protox) so no `protoc` is needed —
and `remora-tui`) mirror remora-edge's own `components/store`/`config`
convention. Each binary in `containers/` (`remora-etcher`, `rmra`) is a
composition root that wires its adapters and use cases together via
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
- `.github/workflows/release.yml` — generated by
  [dist](https://opensource.axo.dev/cargo-dist/) from `dist-workspace.toml`
  (edit that, then `dist generate`; never edit the workflow by hand). On a
  `vX.Y.Z` tag it builds both binaries for `x86_64`/`aarch64` Linux (musl),
  `x86_64` Windows and `x86_64`/`aarch64` macOS on native runners, and
  publishes them with shell/PowerShell installers, checksums and GitHub
  build attestations as a GitHub release of this repository, then pushes
  Homebrew formulas to [sntns/homebrew-tap](https://github.com/sntns/homebrew-tap).
  On pull requests it only plans.
- Repo secret it needs: `HOMEBREW_TAP_TOKEN` (contents: write on the tap).
- `dist plan` shows locally what a release would contain; `dist build`
  builds it for the host.

### Cutting a release

From the [Actions tab](https://github.com/sntns/remora-companion/actions/workflows/cut-release.yml),
run **Cut a release** and pick `patch`/`minor`/`major`. That's it — it uses
[`cargo-release`](https://github.com/crate-ci/cargo-release) to bump
`[workspace.package].version` (every crate inherits it, so the whole
workspace moves together in one commit), tag `vX.Y.Z`, and push, which
triggers dist's `release.yml` to build and publish the release automatically.

This needs a `RELEASE_TOKEN` repo secret — a PAT with `contents: write` on
this repo — because a push made with the default `GITHUB_TOKEN` never
triggers another workflow (GitHub's anti-recursion guard), so `release.yml`
would never fire otherwise.

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
