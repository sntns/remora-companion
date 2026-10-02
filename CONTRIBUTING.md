# Contributing to remora-companion

Thanks for considering a contribution. This project is developed
DDD-style, one Cargo workspace with one `components/<vertical>` set of
crates per bounded context — read [CLAUDE.md](CLAUDE.md) first for the
architecture, error-handling, and testing conventions the codebase follows;
changes are expected to fit that structure rather than introduce a new one.

## Before you start

For anything beyond a small fix (a new adapter, a new subcommand, a change
to an existing port's contract), please open an issue first to discuss the
approach. It avoids spending effort on a PR that goes a direction the
maintainers wouldn't take.

## Building and testing

```
cargo build --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo deny check advisories bans licenses sources
```

This is the same sequence CI (`.github/workflows/ci.yml`) runs on every
push and PR, across Linux/Windows/macOS. A PR won't be merged if any of
these fail.

Some integration tests shell out to real dev-only tools (`sgdisk`/`sfdisk`/
`mke2fs`/`mkfs.vfat`/`fsck.*`/`unsquashfs`/`qemu-img`/a `bmaptool`-derived
fixture) rather than mocking them — see [CLAUDE.md](CLAUDE.md#testing).
Make sure the relevant tool is installed if a test in the crate you're
touching needs it.

## Code style

- No mocking framework — wire real adapters in tests, per the pattern in
  any `components/*-application/src/controller.rs`.
- Errors: a `thiserror` leaf per crate, wrapped in `error-stack::Report`
  context at boundaries. See [CLAUDE.md](CLAUDE.md) for the full
  convention.
- `cargo fmt` (config in `rustfmt.toml`) and `cargo clippy -D warnings`
  must be clean.

## Commit messages

Recent history uses a Conventional-Commits-style prefix (`feat(...)`,
`fix(...)`, `chore(...)`, etc.) with an imperative summary line. Keeping
that convention makes the log easier to skim, but isn't strictly enforced.

## Submitting a pull request

1. Fork the repo and create a branch off `main`.
2. Keep the change focused — unrelated cleanup makes a PR harder to review
   and belongs in its own PR.
3. Make sure the commands above all pass locally.
4. Open the PR against `main` and fill in the template.

By submitting a contribution, you agree it is licensed under this
project's [Apache License, Version 2.0](LICENSE), per section 5 of that
license (no separate CLA is required).

## Reporting bugs / requesting features

Use the GitHub issue templates. For a bug report, the more of the
following you can include the faster it can be triaged: OS/architecture,
the exact command run, the full `error-stack` output (it prints a
file:line cause chain), and, if it involves an image or partition, how
that image was produced.

## Security issues

Please do not open a public issue for a security vulnerability — see
[SECURITY.md](SECURITY.md).
