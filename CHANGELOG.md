# Changelog

The format is [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Nothing has been released. The version in `Cargo.toml` is an ordinary release
version and, before 1.0, it promises nothing. Until then every entry lands in
Unreleased.

## [Unreleased]

### Added

- `cargo deny check` runs as a step of `cargo xtask verify`, so a crate that
  enters the dependency graph without a reviewed entry in `deny.toml` fails a
  gate instead of appearing in a lockfile diff.
- `cargo xtask verify` builds the workspace at the `rust-version` its manifest
  states, so the stated minimum is a checked claim.
- `cargo xtask verify --install-hook` installs a `pre-push` hook running
  `cargo xtask verify --fast`. Installing over an existing hook it did not write
  refuses and changes nothing.
- `LICENSE`, `README.md`, `SECURITY.md` and this file.
- A release profile, and a profiler that measures it rather than a debug build.
- A two-sided gate on deterministic benchmark metrics: a metric that moves more
  than five percent in either direction fails, as does one the baseline carries
  that a run stops producing.
- A test that checks the work counters against what the platform actually
  performed, rather than against a recorded number.

### Changed

- `cargo xtask verify --fast` is the push gate. It runs format, dependencies,
  both lint arms, the build and the aarch64 Windows compile, sixteen seconds on
  a fully warm tree, and names every lane it declined with a `NOT VERIFIED`
  line. It no longer runs the suite.
- `CONTRIBUTING.md` absorbs the build protocol, the prerequisites the
  verification matrix needs, and how the suites are run here.
- A run that fails names the action first and the error kind second.
- A run opens one connection pool per host rather than four.
- Hashing is spread across threads where a chunk is worth it.
- Glob matching is linear in the pattern and the path, and splits a pattern once
  per path rather than once per component.
- The in-flight ceiling tests assert what the fault server observed rather than
  what a clock measured, so they no longer fail under load.
- The thirty-seven command line test targets are four.
- `main.rs`, `transfer.rs` and `run.rs` are split into one module per thing they
  do.
- The help text, the reference transcripts and the failure messages are
  regenerated from real runs.

### Fixed

- The bytes a run reports mean one thing. The double count in cache ingest is
  gone and both real reads are counted.
- A transfer reports the verification it is conditional on.
- The decider names the failure when the cache format file cannot be written,
  rather than deciding for itself.
- Reconcile scans the destination once rather than once per resolved entry.
- Two document reads judge the declared size before taking the whole file.
- The kill test is linear, and its writers are killed outright rather than
  aborted, which removed the Windows error reporting process from the suite.
- `deny.toml` names every crate the graph actually holds. Six unreviewed crates
  were reviewed and added, five entries for crates no longer in the graph were
  removed, three duplicated entries were merged, and the skip entry naming `ring`
  went with it.

### Known

- `crates/platform/tests/capability.rs` fails on `x86_64-unknown-linux-gnu`
  under whole-suite load. A volume is reported `Scanner::Absent` once and
  `Scanner::Unknown` the next time, because the Linux scanner probe decides from
  a wall clock and its per-volume cache lets a concurrent measurement overwrite
  an answer already given out. Diagnosed in `docs/decisions.md`, not fixed.

### Removed

- `docs/build-protocol.md`, absorbed into `CONTRIBUTING.md`.
- The comment checker, and every docstring the standards do not ask for.

### Security

- A zip symlink member was read in full before the bomb guard was consulted, so a
  100 MB archive could force about 100 GB of decompression before the
  expanded-byte limit or the ratio was checked. The guard is now asked with the
  declared size before the member kind is acted on.
- Glob matching recursed over every suffix at each recursive wildcard, so a
  pattern in a manifest fetched from a remote URL could be made to cost
  exponentially in the number of `**` components. Matching is now linear.
