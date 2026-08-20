# Conventions

## `mod.rs` contains only imports

A `mod.rs` (or any inline module block) never holds logic — only `mod`/`pub mod`
declarations and `pub use` re-exports. Rustfmt has no setting that enforces this
(it only formats), so it's a review-time convention, not a build-time check.

Split real code into sibling files instead:

- `error.rs` — the module's `Error` enum, its `Display`/`std::error::Error`
  impls, and any `From` conversions.
- `service.rs` — everything else (functions, private helpers, types), with
  `use super::error::Error;` at the top. Unit tests stay inline at the bottom
  of `service.rs` in a `#[cfg(test)] mod tests { ... }` block, following the
  existing pattern in `application::identity::build`,
  `application::ext4image::build`, and `application::squashfs::build`.

For a directory with several backend-specific implementations (e.g.
`adapter::partition_table`'s `gpt`/`mbr`, or `adapter::disk`'s `linux`),
those stay as their own private `mod` files declared in `mod.rs`;
`service.rs` dispatches to them.

Reference: `application::flash` (`error.rs` + `service.rs`) is the clean
example every other module was aligned to.

## Adapter names carry the function, not the third-party crate

`adapter::*` modules are named after what they do (`disk`, `ext4`, `vfat`,
`bmap`, `squashfs`, `partition_table`), never after the crate wrapped inside
(`fs_ext4`, `fatfs`, `bmap-parser`, `backhand`, `gptman`/`mbrman`) — the crate
name is an implementation detail that can change without renaming the public
adapter surface. The crate itself is named in the module's doc comment, not
in the path.

When one function needs several OS-specific implementations, that's a
`linux`/`macos`/`windows` file (or prefix) inside the adapter directory —
see `adapter::disk::linux` — dispatched from `service.rs` behind
`#[cfg(target_os = "...")]`. When it instead needs several *format*-specific
implementations (not OS-specific), name each file after the format it reads,
e.g. `adapter::partition_table::{gpt, mbr}`.

## Every adapter exposes a trait — the DI seam

Each `adapter::*` module has a `port.rs` next to `error.rs`/`service.rs`:

- a trait named `<Name>Adapter` (`DiskAdapter`, `Ext4Adapter`, `VfatAdapter`,
  `BmapAdapter`, `SquashfsAdapter`, `PartitionTableAdapter`) whose methods
  mirror the adapter's own free functions in `service.rs`, taking `&self`
  and concrete (not generic) parameter types so the trait stays object-safe
  (`&dyn Ext4Adapter` has to work);
- a zero-sized struct implementing it by delegating straight to
  `service.rs` (`Disk`, `Ext4`, `Vfat`, `Bmap`, `Squashfs`,
  `PartitionTableReader`).

Both are re-exported from the adapter's `mod.rs`. The plain free functions
stay too — most call sites still use them directly; the trait only matters
once a caller actually needs to inject a different implementation (real vs.
test double, or — see below — one of several real backends).

No application service takes an injected adapter yet; these traits are
groundwork for that, laid out ahead of wiring an actual DI container.

### Composite ports live with their consumer, not with an adapter

`application::image::fs_dispatch` picks between the `ext4` and `vfat`
backends per-partition (from the on-disk filesystem signature, never from
the boot mode). That's not one adapter's concern, so the port describing it
— `PartitionFilesystem`, plus the `PartitionFsError` that unifies
`ext4::Error`/`vfat::Error` behind one type — is defined in
`application::image::partition_fs`, the module that actually needs it, and
implemented there for the `adapter::ext4::Ext4` and `adapter::vfat::Vfat`
structs. `fs_dispatch` resolves a `&'static dyn PartitionFilesystem` from
`FsKind` and calls through it instead of matching on `FsKind` at every
call site. Rust's orphan rule allows this (the trait is local to this
crate), and it's the general rule for any future cross-adapter port: define
it next to the use case that composes the adapters, not inside either one.
