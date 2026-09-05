# Changelog

Format is [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Nothing has been released. The version in `Cargo.toml` promises nothing before 1.0, so every entry lands in Unreleased.

## [Unreleased]

### Added

- Cached objects are stored compressed. `--compress <auto|none|zstd:1..19>`, the `compress` configuration key and `FETCHLOOM_COMPRESS` decide how, `auto` being the default. `auto` compresses an object only when compressing its first 1 MiB measured a ratio of at least 1.10, so an archive or a photograph is stored raw rather than spending processor time to grow. `explain` reports the effective value and the level that supplied it.
- A compressed object is written as zstd frames of one outboard chunk group each with a table of their lengths, so a ranged verify or a localized repair decompresses only the frames covering the range it asked for.
- Float arrays are byte shuffled at stride 4 before compression when that measured better on the object itself, which is worth 1.692 to 2.735 on the corpus float entry. Which objects those are is measured from the bytes, never taken from an extension or a media type.
- Bundles are compressed. Every trust property is unchanged: a member is still named by the digest of its own bytes, import still derives every digest from what it reads, a name is still never a path, and a bundle that fails anywhere still publishes nothing.
- A volume that compresses what is written to it is a detected capability on both platforms, queried on Windows through the file attribute and measured on Linux from the blocks a compressible write allocates. A run on one stores objects raw and emits `degrade`, rather than compressing what the filesystem will compress again.
- `cargo deny check` runs as a step of `cargo xtask verify`, so a crate entering the dependency graph without a reviewed entry fails a gate instead of appearing in a lockfile diff.
- `cargo xtask verify` builds the workspace at the `rust-version` its manifest states, so the stated minimum is a checked claim rather than a number.
- `cargo xtask verify --install-hook` installs a `pre-push` hook. Installing over a hook it did not write refuses and changes nothing.
- `cargo xtask surface` reports which public items cross a crate boundary.
- `LICENSE`, `README.md`, `SECURITY.md`, `CHANGELOG.md`.

### Changed

- The zstd codec is libzstd through the `zstd` crate, replacing the pure-Rust `ruzstd`. `ruzstd` implements one of nineteen compression levels and ran 4x to 27x slower on the same corpus entries, so it could not carry `--compress zstd:1..19` without the flag becoming a placeholder. The workspace now compiles and statically links C.
- The musl lint moved from the host into the Linux container, where a C toolchain for it exists. The container lane now denies warnings, which it did not before, so the coverage moved rather than shrank. `cargo xtask verify` is thirteen steps.
- The cache format fingerprint changed, so an existing cache is discarded with `cache clear` rather than migrated. No lock, receipt, plan or bundle manifest changed, and no digest moved.
- Documents are bounded by what wrote them rather than by which parser reads them. A receipt is this machine's record of work it already did, so it carries its own limits; a manifest is written by a stranger and keeps the strict ones.
- A document past a size or node bound fails with `resource.limit`, which is what the contract always said. It previously reported `manifest.invalid`.
- A volume's capability answer is decided once per run, so two callers asking at the same time cannot be given different ones.
- The public surface of every library crate is now what crosses a crate boundary. Roughly 800 items became private.
- `missing_errors_doc` and `missing_panics_doc` are denied and satisfied.
- Documentation rewritten as four files. Sixteen were deleted.

### Fixed

- `verify` failed on any tree past roughly 17,000 files, because a receipt was bounded by the manifest node limit. A run could materialize a tree it could not verify, which is the one thing a receipt exists to prevent.
- `repair` accepted seven flags it could never act on, all of them about materialization, which it does not do.
- A `degrade` event stated one hardcoded reason for three different causes, so a failing fetch of an archive claimed the reference named a directory.
- A zip symlink member was read before the expansion guard observed its size, so a 100 MB archive could force an allocation near 100 GB.
- Selection globs backtracked, so eight star components took 143 ms and each further one multiplied that by about 5.8. Patterns arrive in remote manifests.
- Reconcile was quadratic in entry count. At the contracted limit it extrapolated to about 269 seconds.
- Manifests and plans were read whole before their size was checked.
- `corroborated` is documented as unreachable in this build, which it always was, and a test pins it in both directions.

### Removed

- `cargo xtask check-comments`. A checker for a taste rule is code shipped to police taste.
- `audit.md`, `audit2.md`, `plan.md`, and eleven documentation files replaced by the four that remain.
