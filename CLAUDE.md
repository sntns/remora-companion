# Conventions

DDD-style workspace, matching [remora-edge](https://github.com/sntns/remora-edge)'s
convention. One `components/<vertical>` crate group per bounded context
(currently `disk`, `flash`, `image`, `squashfs`, `identity`, `config`,
`convert`, `batch`, `factory`), plus shared utility crates with no vertical
prefix, plus the binaries in `containers/` (`remora-etcher`). Components are
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
  `remora-disk-adapter-native`) — only genuinely alternate backends
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
- `remora-scratch` — a plain function (`unique_path`) generating a
  unique temp path. **Not** wrapped in a port/DI seam: which directory
  scratch files live in isn't a business decision any test needs to
  substitute, so the ceremony would be pure overhead. Contrast this
  deliberately with `fs-walk`, which *is* seamed — its I/O behavior actually
  needs faking; a temp path generator's doesn't. When adding a new
  cross-vertical helper, ask which case it is before defaulting to a port.

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
  for the pattern). Inside a **transport-cli crate**: `service.rs` holds the
  `Command` enum and `run()`.

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

`busybody`'s API is async (so it can support resolvers that need to await
something), which is the *only* reason this binary depends on `tokio` at
all — `bootstrap::wire()` runs inside a throwaway `tokio::runtime::Runtime`
in `main()`, and every use case downstream stays plain synchronous Rust.
Don't let async leak past `wire()`.

`rmra` is the exception, by nature rather than by habit: everything it does
is network I/O (gRPC to sntns-platform, a bidirectional channel relayed to
stdio, an ssh child whose signals it relays), so its ports, use cases and
transports are async end to end on one `#[tokio::main]` runtime. Its
blocking local-file adapters (`context-adapter-file`) stay synchronous, as
everywhere else.

## rmra specifics

- **stdout is output, stderr is everything else.** Tables, JSON and a
  channel's bytes go to stdout; status lines, spinners, prompts and errors
  go to stderr through `remora-tui` (clack-style, via cliclack, degrading
  to plain lines when stderr isn't a terminal). `rmra channel open` is an
  ssh ProxyCommand: one stray byte on its stdout corrupts the session.
  Transports never `println!` decoration and never draw with cliclack
  directly — add what's missing to `remora-tui` so every command looks alike.
- **Secrets never reach a `Debug`.** `Credentials` and `remora-platform-
  grpc`'s connection types redact or don't implement `Debug`, because
  error-stack reports print with `{:?}`. Keep it that way for any new type
  holding a token, a password or a private key.
- **Vendored protos.** `components/platform-grpc/proto` holds client-side
  subsets of sntns-platform's gateway APIs (see its README for what was
  stripped and why it's wire-safe). Re-copy from upstream rather than
  editing; package, service, message names and field numbers must stay
  upstream's.
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
never a blanket `From` impl chaining unrelated layers' errors together.
`Report::new(Error::Variant)` starts a fresh chain at a leaf (e.g. wrapping
a raw `std::io::Error` whose value you don't want to keep). At the very top,
`containers/remora-etcher/src/main.rs` prints the error via `{:?}` (Debug),
not `{}` (Display) — Display only shows the outermost context's message,
while Debug walks the whole `error-stack` chain with a file:line per hop,
which is almost always what you actually want to see. `rmra` renders the
same chain for an operator instead (`remora_tui::render_report`: each
context and printable attachment on its own line, outermost first), and
the full `{:?}` with `--verbose`.

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
with real host and user CAs (Linux only). An application crate that needs
the platform port but isn't about it may use a small hand-written stub of
that one trait (see `context-application`'s tests).
