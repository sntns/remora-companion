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
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/sntns/remora-companion/releases/latest/download/remora-etcher-installer.sh | sh
```

Windows (PowerShell):

```
powershell -ExecutionPolicy Bypass -c "irm https://github.com/sntns/remora-companion/releases/latest/download/rmra-installer.ps1 | iex"
powershell -ExecutionPolicy Bypass -c "irm https://github.com/sntns/remora-companion/releases/latest/download/remora-etcher-installer.ps1 | iex"
```

Homebrew:

```
brew install sntns/tap/rmra
brew install sntns/tap/remora-etcher
```

Install either one or both. The installers put the binary in
`~/.local/bin` (`RMRA_INSTALL_DIR=/path` to choose) and add it to your
PATH. Linux binaries are static (musl): one binary for any distribution.

Then enable shell completion, e.g. for zsh (`rmra completion` gives the
line for your own shell):

```
rmra completion zsh >> ~/.zshrc
remora-etcher completion zsh >> ~/.zshrc
```

Each appends a line that only loads when its binary is on the PATH, e.g.
`if command -v rmra >/dev/null; then source <(COMPLETE=zsh rmra); fi`, so
it stays harmless once the tool is uninstalled.

`rmra update` (or `remora-etcher update`) installs the latest release over
the running one, the way it was installed (`--check` only tells); a Homebrew
install is updated with `brew upgrade sntns/tap/rmra` (or
`sntns/tap/remora-etcher`) instead.

## rmra

### Act as a role, e.g. in another tenant

A context is a login *and* the IAM role it acts as -- possibly a role of
another tenant, which a login may assume when it has the
`iam::assume-role` permission on it and the role's trust policy allows it.
The role is decided when logging in, and verified with the platform
together with the credentials: a refused role refuses the login.

```
rmra login                                    # interactive: also asks "Act as"
rmra login --assume-role urn:sntns:iam:eu2:<tenant>:role:ops --token-stdin
rmra login --no-assume-role --token-stdin     # back to the login's own account

# a base context, declined per role -- e.g. one context per tenant:
rmra context create eu2 --use && rmra login
rmra context create acme --from eu2 --assume-role urn:sntns:iam:eu2:<tenant>:role:ops
rmra context create beta --from eu2 --assume-role urn:sntns:iam:eu2:<other>:role:ops
rmra -c acme whoami                           # ada acting as ops @ acme
```

A declined context (`--from`) takes its base's endpoint and role aliases,
and shares its login: logging in or out of any context of the group does it
for all (`context ls` shows `via eu2`), and the base can't be removed while
declined contexts use it. Its role is verified when it's created.

From then on every call of the context (devices, OTA, `ssh`/`scp`, whose
channel is opened as the very role the key was certified for) acts in the
role's account. Roles can also be named and switched without logging in
again (`rmra context role add <alias> <urn>`, `rmra context role assume
<alias>`, `rmra context role drop`). The role is always the context's: no
command takes a role of its own -- for another role, use (or `--from`
create) another context.

### Shell completion

```
rmra completion            # shows the line to add for your shell, e.g. for zsh:
rmra completion zsh >> ~/.zshrc     # i.e. if command -v rmra >/dev/null; then source <(COMPLETE=zsh rmra); fi
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
rmra context rename staging preprod
```

`--context`/`-c` comes before the command (`rmra -c eu2 ssh …`), not after
it. A renamed context keeps its login, its role, its being current, and the
contexts declined from it.

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
`-v` is rmra's own, so pass scp's after `--`; scp's `-c` (cipher) is scp's,
since rmra's `--context` goes before the command.

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
live byte bar and resume on their own after a dropped connection. Ctrl-C (or
a local read error) stops an upload without the platform committing what
was sent; it prints the `rmra release upload … --resume <token>` command
that continues it.

### Download an artifact

```
rmra release download 2026.10.0 --board rp5 --type diskimage -o ~/images/
rmra release download 2026.10.0 --artifact remora-hdc.raucb
```

Picks the artifact like `remora-etcher flash --release` does: by file name
(`--artifact`), or by the `board:` and `type:` tags of its tag condition
(`--board`, `--type`); with several left and nobody to ask, it says which.
It's written to `<file>.part` until it's whole and matches the release's
sha256, then renamed (`--force` replaces an existing file). A dropped
connection is picked up where it stopped, and so is an interrupted run:
run the same command again.

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
doesn't succeed; Ctrl-C stops watching, not the deployments. A status rmra
doesn't know yet stops the watch for that deployment with a warning and
counts as not succeeded. `--interval` sets the polling period of `--watch`
and `deployment logs --follow`. With `--selector`, what is deployed is
exactly the device list shown and confirmed. Every listing — and
`deployment logs` (one object per line with `--follow`) and `deploy`'s
result — takes `--format json` for scripts.

## remora-etcher

### Contexts

The same contexts as rmra's, in the same place, with the same commands:
`remora-etcher login`, `whoami`, `logout`, `context ls|create|use|rename|rm`
and `context role …`. A context created or logged in with one binary is
usable with the other; `-c`/`RMRA_CONTEXT` select one, before the command.

### Shell completion

```
remora-etcher completion   # shows the line to add for your shell, e.g. for zsh:
remora-etcher completion zsh >> ~/.zshrc     # i.e. if command -v remora-etcher >/dev/null; then source <(COMPLETE=zsh remora-etcher); fi
```

Same shells and mechanism as rmra's. Beyond commands, options and paths,
Tab completes disks (`remora-etcher flash --device <Tab>`, `disk info
<Tab>`), removable ones first, never the system disk.

### Flash an image to a USB stick or SD card

```
remora-etcher flash --image remora.wic.bmaptar --device /dev/sdb
remora-etcher flash --release v2026.10.0 --board f3apl --device /dev/sdb
```

`--release` flashes a release's disk image straight from the platform,
as the selected context: one of its artifacts tagged `type:diskimage`
(`--board` picks it by its `board:` tag, `--artifact` by file name; with
several and neither, it asks). It's downloaded while it's written, read
like a local file -- the bundle's `.bmap` first, then the image streamed
to the disk, a dropped connection picked up where it stopped -- with a
third progress line for the download.

The image is best a `.bmaptar`, as meta-remora builds it: the compressed
image and its `.bmap` in one tar, read in place (nothing to extract first).
A plain image works too, raw or compressed (bzip2, gzip, zstd,
decompressed on the fly; bzip2 on every core when it's pbzip2's
multi-stream output, as Yocto's is). It first shows the image (format,
size, what will actually be written, its `.bmap`) and the target disk,
then the copy: a progress bar, with what's written, the speed and the time
left under it. Sparse-aware and checksum-verified whenever there's a `.bmap`:
the bundle's own, else one next to the image (`<image>.bmap`, or the
image's name without its compression extension, `remora.wic.bz2` →
`remora.wic.bmap`, same convention as `bmaptool`); pass one with `--bmap`,
or skip it with `--no-bmap`. The device may be given by any path to it
(`/dev/disk/by-id/…`, a symlink): the checks run on the disk it really
is. Refuses to overwrite what looks like the system
disk, a non-removable disk unless you pass `--force`, and a disk smaller
than the image; it doesn't ask for confirmation otherwise. Ctrl-C stops
the copy (twice: at once).

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

### Manufacture a device

```
remora-etcher -c factory factory provision --serial-policy hubs --output remora-factory.yaml
remora-etcher -c factory factory provision --device-name 1H7Z --force --output remora-factory.yaml
```

Generates the device's key locally and has the platform issue its IDevID,
as the selected context: its login (or the role it acts as) needs
`remora::create-factory-device`, and `remora::use-serial-number-policy` on
the policy. `--serial-policy` allocates a fresh serial, printed alone on
stdout for the label; `--device-name` uses one chosen elsewhere, refused if
already manufactured unless `--force` (re-signed, the old IDevID revoked).
The resulting `remora-factory.yaml` is bundled into an image with
`identity create`. As a `factory-provision` batch step, `"context"` picks
the context, else the batch's own.

### Run a provisioning station

```
remora-etcher -c factory station serve --config station.yaml
```

For hubs made by cloning one SD card: each one boots on the workshop
network, claims an identity from the station (`POST /v1/claims` on port
8484, plain HTTP, the CSR of a key it keeps), and gets one at once from
the platform, as the selected context -- the same permissions as `factory
provision`, a dedicated access-key context recommended. Labelling is one
hub at a time: the station activates the oldest one, whose LED turns
steady, its `label.d` hooks print the label, and the operator sticks it on
that hub and scans it. A scan that isn't its serial is refused (bell, red
line, nothing validated); a matching one validates it, the hub writes its
identity and reboots, and the next hub's LED turns steady. Commands, then
Enter: `r` reprint, `s` skip (to the end of the queue), `f` validate despite
a failed print (the scan is still required), `q` quit -- the hooks still
running get `hook-timeout` to finish, then are killed. The station checks
at start that its context is logged in (it asks the platform), and turns
away any hub whose report isn't plain short identifiers.

```yaml
# station.yaml -- relative paths are from this file's directory
listen: 0.0.0.0:8484
journal: ./station.jsonl          # reloaded at start
hooks: ./station.d                # <event>.d/ directories; must exist
max-claims: 50                    # optional: at most this many per run
confirm: scan                     # scan | key (Enter alone: bench only)
presence-timeout: 10s             # an active hub this silent is requeued
hook-timeout: 60s
boards:                           # any other board is refused
  hub-v2:
    create-factory-device:
      serial-number-policy: hubs-v2
  hub-v1:
    create-factory-device:
      device-name: "{bsp_serial}"  # or {eth_mac}, {temp_hostname}, {board}
```

Every key has its option (`--listen`, `--journal`, `--hooks`,
`--max-claims`, `--confirm`, `--presence-timeout`, `--hook-timeout`,
`--serial-policy BOARD=POLICY`, `--device-name BOARD=TEMPLATE`), which wins
over the file. Each issued serial is printed on stdout; the dashboard is on
stderr. The journal (one JSON object per transition) is the production
register -- serial, MACs, BSP serial, machine-id, date, context -- and the
station's memory: a hub retrying with the same key, or a restarted station,
gets the identity already issued, never a second serial -- nothing is
handed out that the journal hasn't recorded (hubs are told to retry while
it can't be written). Hooks are
run-parts style: the executables of `issued.d`, `label.d` (blocking: the
label can't be validated until they succeed), `labelled.d`, `installed.d`
and `failed.d`, in lexical order, with the event as `$1`, the claim as JSON
on stdin and as `REMORA_*` variables (`REMORA_SERIAL`, `REMORA_BOARD`,
`REMORA_ETH_MAC`, `REMORA_ATTEMPT`...).

```
remora-etcher station simulate --url http://127.0.0.1:8484 --board hub-virtual --output remora-factory.yaml
```

Plays a hub against a station: a key, a claim, the LED on stderr, and
once labelled its `remora-factory.yaml` (as `factory provision` writes it)
and the acknowledgement.

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
`config`, `convert`, `batch`, `factory`, `station` and `claim` (a hub's side
of the station, for `station simulate`) for remora-etcher, `context`,
`channel`, `ota` and `device` for rmra, `update` (and `context`) for both. Components are generic, not owned by a binary: the
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
since writing into a partition, or a standalone ext4 image, is `image`'s
job either way. Likewise `flash` asks `disk`'s `DiskService` which disk a
path really is, and `ota` asks `device`'s `DeviceService` which devices a
selector names.

Small shared utility crates with no vertical prefix (`remora-fs-walk`,
`remora-scratch`, `remora-format`, `remora-progress`, `remora-tui`,
`remora-completion`, and `remora-platform-grpc` — the vendored
sntns-platform protos, compiled with the pure-Rust
[protox](https://docs.rs/protox) so no `protoc` is needed) mirror
remora-edge's own `components/store`/`config` convention. Each binary in `containers/` (`remora-etcher`, `rmra`) is a
composition root that wires its adapters and use cases together via
[`busybody`](https://docs.rs/busybody) (the same DI crate remora-edge uses),
then dispatches CLI subcommands into them. Errors propagate as
[`error-stack`](https://docs.rs/error-stack) `Report`s end to end: a failure
prints its headline and causes for a human, and its full chain with
file:line at every layer with `-v`/`--verbose`.

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
