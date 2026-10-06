# Conventions

DDD-style workspace, matching [remora-edge](https://github.com/sntns/remora-edge)'s
convention. One `components/<vertical>` crate group per bounded context
(currently `disk`, `flash`, `image`, `squashfs`, `identity`, `config`,
`convert`, `batch`, `factory`, `station`, `claim` for remora-etcher;
`channel`, `ota`, `device` for rmra; `context` and `update` for both), plus shared utility crates with
no vertical prefix, plus the binaries in `containers/` (`remora-etcher`,
`rmra`). Components are
generic, not owned by one binary: the directory is `components/<vertical>`
and the package is `remora-<vertical>` (lib `remora_<vertical>`), exactly
like remora-edge — never prefix a component with a binary's name. Before
adding a new vertical or touching an existing one, read the equivalent files
in an already-migrated vertical (`components/flash*` is the smallest
complete example) rather than inventing a new shape.

## Crate-per-role, not module-per-role

Each vertical is (at least) four crates, never one:

- `components/<vertical>` — the **domain crate**: pure model types
  (`model/`), and `*Adapter`/`*ServiceInterface` **port traits** (`adapter/`,
  `application/`). No I/O, no adapter *implementations* — only the
  contracts. It's fine for a domain crate to depend on a third-party crate
  for an inert value type an adapter's signature needs to name (e.g.
  `remora-flash`'s `BlockMap` wraps `bmap_parser::Bmap` directly, and
  `remora-image`'s ext4/vfat errors wrap `fs_ext4`/`fatfs`'s own
  error types) — reimplementing those as parallel pure types purely to avoid
  the dependency would be busywork with no isolation benefit, since the real
  DI seam is the port trait, not the value's shape. What must never leak
  into a domain crate is a third-party crate whose *behavior* (parsing,
  network I/O, subprocess calls) needs to be swappable for tests — that
  behavior stays behind the port, implemented only in an adapter crate.
- `components/<vertical>-application` — the **use case**: a `*ControllerImpl`
  struct holding only injected ports as fields, implementing the domain's
  `*ServiceInterface`. Cross-vertical calls (e.g. `identity`/`config` writing
  into a partition) go through another vertical's `application` port
  (`ImageService`), never its adapters directly — that would duplicate
  logic the other vertical's controller already encapsulates (partition
  selection, ext4/vfat backend dispatch, ...).
- `components/<vertical>-adapter-<name>` — a concrete port implementation
  (e.g. `-adapter-ext4` wraps `am-fs-ext4`). One crate per real backend, not
  per OS: cross-platform dispatch (`linux.rs`/`macos.rs`/`windows.rs` behind
  `#[cfg(target_os = "...")]`) stays *inside* one adapter crate (see
  `remora-disk-adapter-native`, where only `linux.rs` exists yet and other
  platforms are refused as unsupported) — only genuinely alternate backends
  (ext4 vs. vfat, GPT vs. MBR within one reader) get split further, and GPT/
  MBR specifically stay as internal modules of one `-adapter-partition-table`
  crate because they're always tried together (`read()` falls back from one
  to the other), not selected independently.
- `components/<vertical>-application-transport-cli` — the `clap`
  `#[derive(Subcommand)]` enum plus a `run()` function taking `&XService`
  and calling straight into it. No business logic here beyond CLI-only
  concerns (confirmation prompts, output formatting).

Two shared, no-prefix utility crates exist for cross-vertical infrastructure
that isn't itself a bounded context:

- `remora-fs-walk` — wraps `walkdir`. Bundles its port trait and real
  implementation in *one* crate (no separate `-adapter-*`), because unlike
  ext4/vfat there is only ever one real backend — the split exists to make
  swapping backends *and* faking behavior in tests both possible; with a
  single backend and only the second need applying, one crate is enough
  (same reasoning as remora-edge's own `components/store`/`config`).
  Its `walkdir`-backed implementation sits behind a default `walkdir`
  feature, so a domain crate can name its model types with
  `default-features = false` without pulling the behavior in.
- `remora-scratch` — `ScratchDir`, a private scratch directory (random name,
  created exclusively with mode 0700, removed on drop) and `write_private`
  (0600, `create_new`) for secrets that must briefly touch the disk.
  **Not** wrapped in a port/DI seam: where scratch files live isn't a
  business decision any test needs to substitute. Contrast this deliberately
  with `fs-walk`, which *is* seamed — its I/O behavior actually needs
  faking; a scratch directory's doesn't. When adding a new cross-vertical
  helper, ask which case it is before defaulting to a port.

The other unseamed utility crates, same reasoning: `remora-tui` (all
terminal presentation, see below), `remora-completion` (dynamic shell
completion: value kinds, the provider registry, the `completion` command,
the remote-values `Cache` and `command_line()` for providers),
`remora-progress` (progress events, `OperationContext`, and
`cancelled_by_ctrl_c()`: the token every transport hands a long operation,
so Ctrl-C stops it between steps), `remora-format`,
`remora-platform-grpc` (the vendored protos and the gateway connection), and
`remora-station-protocol` (the provisioning station's HTTP wire types and
error codes: one definition, used by the station's HTTP transport and by
`claim-adapter-http`).

`station` is the provisioning station (`remora-etcher station serve`): it
relays SD-cloned hubs' identity requests to the platform through
`FactoryService`, queues them for labelling and runs the operator's hooks.
`claim` is the hub's side of that contract, used by `station simulate`
through `ClaimService` only. The station is stateful by nature: its use
case holds the claims in memory, journals them through a port, and tracks
hook runs; the console loop (stdin, tick, Ctrl-C) lives in the transport,
which drives the port's `handle`/`tick` and calls `shutdown` (draining hook
runs, flushing the journal) before the process exits. `OperatorAdapter` is
display only.

## File names inside a crate

Mirrors remora-edge exactly:

- `mod.rs` — declarations and `pub use` re-exports only, never logic.
- Inside a **domain crate**'s `adapter::<port>` or `application` module:
  `service.rs` holds the port trait, its `<Name>Service` DI wrapper
  (`pub struct XService(busybody::Service<Box<dyn XInterface>>)` +
  `Deref`), and any port-owned model types (e.g. `BlockMap`). `error.rs`
  holds that port's own `thiserror`-derived `Error` enum plus a local
  `pub type Result<T> = std::result::Result<T, error_stack::Report<Error>>;`
  alias — always define this alias; it's what keeps signatures from
  spelling out `error_stack::Report` everywhere (see "Errors" below).
- Inside an **application crate**: `controller.rs` holds the
  `*ControllerImpl` struct and its trait impl. Inside an **adapter crate**:
  `service.rs` holds the `*Impl` struct and its trait impl, plus whatever
  private helpers it needs (these stay as plain `std::result::Result<T,
  Error>` internally, wrapped in `Report::new`/`.change_context(...)` only
  at the trait-impl method boundary — see `remora-image-adapter-ext4`
  for the pattern; a helper's error may carry the underlying `io::Error` as a
  `#[source]` field, which `Report::new` keeps in the chain). Inside a
  **transport-cli crate**: `service.rs` holds the `Command` enum and
  `run()`, and `error.rs` the transport's own error layer — a transport
  never returns another layer's `Result`. A transport with several
  top-level commands may hold one module per command instead (`ota`:
  `release.rs`, `deployment.rs`, plus `shared.rs`).

## Dependency injection

Manual constructor injection via [`busybody`](https://docs.rs/busybody),
composed once per binary in `containers/<binary>/src/bootstrap.rs` — the
composition root, same recipe as remora-edge's `daemon.rs`: construct an
adapter, `container.set_type(XService::new(adapter)).await`, `get_type`
it back out, hand it to the next constructor that needs it, repeat. Skip the
`set_type`/`get_type` round-trip only when a value has exactly one consumer
constructed immediately afterward and registering it would risk a `TypeId`
collision with another value of the same generic shape (see `ext4_fs`/
`vfat_fs: Arc<dyn PartitionFilesystem>` in `bootstrap.rs` — both are the
same type, so only one could ever live in the container at a time).

Both binaries are async end to end: application ports (`*ServiceInterface`)
are `#[async_trait]`, transports' `run` are `async fn`, and `main()` builds
one multi-thread runtime by hand (not `#[tokio::main]`: shell completion
must answer before it, since a completion provider runs a runtime of its
own and runtimes don't nest). rmra is network I/O throughout; remora-etcher
shares transports and verticals with it (`context`, `factory`, `update`) and
drives progress rendering concurrently with each operation. What must not
happen is blocking work of any size on a runtime thread: a use case or
adapter doing real file or device I/O runs it in
`tokio::task::spawn_blocking` (see `flash`, `squashfs`, `image`,
`convert`). Cheap synchronous adapters stay synchronous behind their port
(`context-adapter-file`); `ota-adapter-file` is async (`tokio::fs`) because
it streams a whole bundle alongside an upload.

## Both binaries

- **stdout is output, stderr is everything else.** Tables, JSON, a
  channel's bytes and a manufactured serial go to stdout; status lines,
  spinners, progress, prompts and errors go to stderr through `remora-tui`
  (clack-style, via cliclack, degrading to plain lines when stderr isn't a
  terminal; `raw_error`/`debug` for ssh's raw-mode terminal and
  `--verbose` lines). `rmra channel open` is an ssh ProxyCommand: one stray
  byte on its stdout corrupts the session. Transports never `println!`
  decoration and never draw with cliclack directly — add what's missing to
  `remora-tui` so every command looks alike.
- **Secrets never reach a `Debug`.** `Credentials`, `remora-platform-grpc`'s
  connection types, the channel's `SshKeys` and factory's `PrivateKey`
  redact or don't implement `Debug`, because error-stack reports print with
  `{:?}`. Keep it that way for any new type holding a token, a password or
  a private key. A secret written to disk is written 0600 (`write_private`,
  factory's credential writer) and, when temporary, removed on every path.
- **Contexts are shared.** Both binaries mount the same context commands
  (`login`, `logout`, `whoami`, `context …` incl. `rename` and `role`) on
  the same store, and take `-c/--context` (`ContextArgs`) before the
  command, never after. The role a context acts as is the context's
  setting only — no command takes a role of its own.
- **Domain errors don't name a binary.** A domain error says what is wrong
  ("not logged in to context \"eu2\""); the context transport's
  `with_hint` attaches what to run, with the program name the composition
  root passes in. Every binary's top-level error handler calls it.

## rmra specifics

- **Vendored protos.** `components/platform-grpc/proto` holds client-side
  subsets of sntns-platform's gateway APIs (see its README for what was
  stripped and why it's wire-safe: each upstream `*_service`/`*_model` pair
  is merged into one file, and the README records the upstream commit of
  each RPC kept). Re-copy from upstream rather than editing; package,
  service, message names and field numbers must stay upstream's.
- **The platform contract is upstream's.** The remora channel's behavior
  (half-close as `eof_response`, the 5 s hangup grace, refused ssh options,
  the ssh pinning options) mirrors sntns-platform's Go client
  (`sntns-service-remora-channel-go`, `sntns-service-remora-go`) on
  purpose; change it there first, then here.

## Errors

[`error-stack`](https://docs.rs/error-stack) end to end. Each port/
application/adapter layer defines its own small `thiserror` `Error` enum
(see "File names" above) and converts across a layer boundary with
`.change_context(Error::Variant)` (or `.change_context_lazy(|| ...)` when
building the new variant needs an owned value, e.g. a cloned `PathBuf`) —
never a blanket `From` impl chaining unrelated layers' errors together, and
never `map_err(|_| Report::new(..))`, which throws the cause away.
`Report::new(Error::Variant)` starts a fresh chain at a leaf (e.g. wrapping
a raw `std::io::Error` whose value you don't want to keep). At the very top,
Display (`{}`) would only show the outermost context's message, while Debug
(`{:?}`) walks the whole `error-stack` chain with a file:line per hop —
which both binaries print with `-v`/`--verbose`. By default they render the
chain for an operator instead (`remora_tui::render_report`: each context
and printable attachment on its own line, outermost first, hints included).

## Testing

No mocking framework. Application-crate tests build a real `*ControllerImpl`
wired to real adapters (added as `[dev-dependencies]`, never `[dependencies]`,
of the application crate) and exercise it like the shipped binary would —
see any `components/*-application/src/controller.rs`'s `#[cfg(test)] mod
tests` for the pattern, or `components/*-application/tests/*.rs` for the
larger integration tests ported from the pre-DDD codebase (real `sgdisk`/
`sfdisk`/`mke2fs`/`mkfs.vfat`/`fsck.*`/`unsquashfs`/a real `bmaptool`-derived
`.bmap` fixture). These dev-only tools are never shelled out to by the
shipped binary — only by tests exercising it.

The one network port is the exception to "real adapters": the platform.
gRPC adapters are tested against an in-process fake gateway built from the
generated tonic servers (`remora-platform-grpc` generates them for exactly
this), and `containers/rmra/tests/` runs the shipped `rmra` binary against
one — `fake_gateway.rs` for login/whoami/`channel open` over stdio,
`real_sshd.rs` for `rmra ssh` end to end against a real user-mode `sshd`
with real host and user CAs (Linux only), `ota.rs` for a release published
(its upload dropped and resumed), rolled out and followed. The OTA fake
lives in `remora-ota-adapter-grpc`'s `test_gateway` module behind the
`test-gateway` feature, enabled from `[dev-dependencies]` only, so the
adapter's, the use case's and the binary's tests share one fake (`cargo run
-p remora-ota-adapter-grpc --features test-gateway --example fake-gateway`
serves it for trying the commands by hand). The same fake backs
`device-adapter-grpc`'s and `factory-adapter-grpc`'s tests; it also serves
IAM `whoami` and can revoke credentials (the station checks its login at
start and answers 503 once it's revoked). Like the
platform, it commits an upload only when its stream ends cleanly; tonic
hands a client reset to a handler as a clean end, so the fake yields once
to let hyper drop the handler first. An application crate that needs
the platform port but isn't about it may use a small hand-written stub of
that one trait (see `context-application`'s tests).
