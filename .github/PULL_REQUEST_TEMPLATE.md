## What does this PR do?

<!-- One or two sentences: the why, not just the what. -->

## How was this tested?

<!-- cargo test output, a manual run against a real image/disk, etc. -->

## Checklist

- [ ] `cargo build --workspace --all-targets` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --all -- --check` is clean
- [ ] `cargo deny check advisories bans licenses sources` is clean (if
      dependencies changed)
- [ ] I've read [CONTRIBUTING.md](CONTRIBUTING.md) and
      [CLAUDE.md](CLAUDE.md) for the architecture/testing conventions
