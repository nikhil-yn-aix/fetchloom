# Changelog

Format is [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Nothing has been released. The version in `Cargo.toml` promises nothing before 1.0, so every entry lands in Unreleased.

## [Unreleased]

### Added

- `status <path>` and `diff <path>` say how a directory differs from the record the run that wrote it left. Four states and no others: unchanged, modified, deleted, added. An unchanged entry is not printed, so a directory that is exactly what the run left prints nothing at all. `diff` adds the digest and the length on each side and never looks inside a file. Both are read only and neither touches the network.
- `revert <path>` puts back what the record states, and `revert <path> <entry>...` puts back only what it names, leaving every other edit alone. The bytes come out of the cache, so a revert works offline. An entry the cache no longer holds fails naming the object and the command that would fetch it, rather than quietly going to the network.
- `get` against a directory you edited, where upstream has since moved, is a three way compare against the record rather than a refusal. Per entry and never per line: what only upstream changed is taken, what only you changed is kept, what you added stays, what upstream added arrives, and what you deleted stays deleted. Where both sides changed one entry, upstream's version is written beside yours as `<name>.upstream`, yours is left exactly as it is, every conflict is named, and the run exits 60 with `destination.conflict`. The merged tree is built whole in staging and published by rename, so a run killed halfway leaves the old directory or the new one.
- `promote <path>` makes the directory as it stands a dataset of its own: every file read, the bytes kept in the cache, a manifest naming one artifact per file with both digests, and a lock pinning them. The manifest states `derived_from`, which is the dataset, the manifest digest and the tree digest it came from. Ingestion happens at promote and at no other time.
- A record now states two entry streams: what was materialized, which is what `verify <path>` folds and what a fingerprint answers about, and what the reference resolved to, which is the merge base. They differ only where a run kept your version of an entry over upstream's.
- An artifact whose stated digest the cache already holds resolves out of the cache without its source path existing, which is what makes a promoted manifest fetchable on a machine that took the objects in a bundle.
- `merge.resolution` is a new event, one per entry a three way run decided. `destination.conflict` is a new error kind, exit 60.

- FTP and FTPS are spoken, written here rather than taken as a dependency. `ftp://host/path` and `ftps://host/path` fetch a file or expand a directory, anonymous by default, passive, binary, resumed with `REST` at rung four. A directory is listed with `MLSD` and falls back to `LIST` with a `degrade` when the server refuses it, which two of the four reference hosts checked live do not support. The address a `PASV` reply names is discarded and only its port used, so a data connection is never opened to a host the control connection is not already talking to.
- `ftps://` secures the control connection with `AUTH TLS`, then `PBSZ 0` and `PROT P` for the data connection, using the rustls already in the graph. No flag turns that off. `ftp://` attempts it and says so when it falls back to the clear, and a credential resolved for the host is refused rather than sent over a connection that could not be secured.
- A name with no `sources` configured is searched for across the eight registries that offer search, in parallel: Hugging Face, Kaggle, OpenML, Zenodo, Figshare, CKAN, Dataverse and DataCite. One record carrying the name proceeds and prints what it resolved to; several are printed with size, provenance and what each states about its bytes, the run refuses, and the command for each is given; none fails naming the nearest names it did find. Nothing is chosen for the user. The name is never written to a lock and what it resolved to is.


- Six more providers are reached by their own identifier: `kaggle:owner/slug`, `openml:61`, `github:owner/repo@tag`, `figshare:1234567`, `ckan:host/dataset` and `dataverse:host/doi:10.x/y`. They share one internal listing shape described by an endpoint and a field mapping, so a provider is a value rather than an adapter, and Hugging Face and Zenodo now share the same `Source` implementation. CKAN and Dataverse carry the host in the reference, over HTTPS only, so one description reaches every installation.
- A digest a provider's listing states is carried as a claim and verified against the bytes. Of the six, GitHub releases states a SHA-256 on every asset, and Dataverse states one on an install configured for it; the rest state only MD5, which is not carried, so their trust class stays `tofu` rather than implying evidence a run cannot recheck.
- `KAGGLE_API_TOKEN` and `GITHUB_TOKEN` are read, so a machine that already has the provider's own variable set needs nothing restated. `FETCHLOOM_TOKEN_<HOST>` is still read first and still holds the whole `Authorization` header.
- `doi:10.7910/DVN/OMV93V` resolves. The registration is read once from DataCite and the landing page decides which provider holds the record, reported as a resolution alias. A DOI resolving to a provider no adapter serves fails naming the registrant, the landing page, and what serving it would need.
- `Artifact.sources` is a list of mirrors across adapters. Each candidate is served by whichever adapter states it serves it, so a manifest may name an HTTPS mirror and a provider mirror for the same bytes and the run falls through from one to the other. This is safe by arithmetic rather than by promise: the digest is over the bytes and not the location.
- Cached objects are stored compressed. `--compress <auto|none|zstd:1..19>`, the `compress` configuration key and `FETCHLOOM_COMPRESS` decide how, `auto` being the default. `auto` compresses an object only when compressing its first 1 MiB measured a ratio of at least 1.10, so an archive or a photograph is stored raw rather than spending processor time to grow. `explain` reports the effective value and the level that supplied it.
- A compressed object is written as zstd frames of one outboard chunk group each with a table of their lengths, so a ranged verify or a localized repair decompresses only the frames covering the range it asked for.
- Float arrays are byte shuffled before compression when that measured better on the object itself. The stride is measured rather than assumed: the probe compresses the head at strides 1, 2, 4 and 8 and stores the winner in the frame footer, where a stride of 1 is spelled 0 because shuffling one byte words is a copy. An eight byte array measured 4.388 at stride 8 against 2.147 at the stride 4 that used to be hardcoded. Which objects those are is measured from the bytes, never taken from an extension or a media type.
- Bundles are compressed. Every trust property is unchanged: a member is still named by the digest of its own bytes, import still derives every digest from what it reads, a name is still never a path, and a bundle that fails anywhere still publishes nothing.
- A volume that compresses what is written to it is a detected capability on both platforms, queried on Windows through the file attribute and measured on Linux from the blocks a compressible write allocates. A run on one stores objects raw and emits `degrade`, rather than compressing what the filesystem will compress again.
- `cargo deny check` runs as a step of `cargo xtask verify`, so a crate entering the dependency graph without a reviewed entry fails a gate instead of appearing in a lockfile diff.
- `cargo xtask verify` builds the workspace at the `rust-version` its manifest states, so the stated minimum is a checked claim rather than a number.
- `cargo xtask verify --install-hook` installs a `pre-push` hook. Installing over a hook it did not write refuses and changes nothing.
- `cache compact` rewrites packs, training one zstd dictionary over the objects each pack holds and recompressing them against it at level 19. A pack holds objects at or below one frame, which is where a frame gives an object no context and a dictionary supplies it: measured at 9.260 without and 13.167 with, over the same objects at the same level. The dictionary is stored in the pack it belongs to and shared with no other pack.
- `cargo xtask surface` reports which public items cross a crate boundary.
- `LICENSE`, `README.md`, `SECURITY.md`, `CHANGELOG.md`.

### Changed

- A fingerprint is recorded only for a file whose modification and change times are already behind the instant the run began recording them, so a file written while the record was being taken is always read rather than trusted. The window that leaves is stated in `docs/contracts.md` rather than claimed closed.
- Deciding whether a destination entry is unchanged no longer walks the resolved tree once per file, which was quadratic in the number of entries.
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

- A SHA-256 a manifest stated was never compared to the bytes. contracts.md has always said either algorithm satisfies `verified`; only BLAKE3 did, because it becomes the address the store commits under. A manifest stating a SHA-256 that disagreed materialized the bytes and exited zero.
- A run that verified every artifact reported `tofu`. The receipt's per-artifact class was right; the run's own class was seeded from a provisional `tofu` that a fold taking the weakest class could never improve on. A matching BLAKE3 claim was affected too.
- `reference.md` carried a corrupted row in its Not built table, left over when `--compress` graduated out of it. The reference test only checked the built half; it now also checks that nothing below the Not built marker is something the binary offers.
- `verify` failed on any tree past roughly 17,000 files, because a receipt was bounded by the manifest node limit. A run could materialize a tree it could not verify, which is the one thing a receipt exists to prevent.
- `repair` accepted seven flags it could never act on, all of them about materialization, which it does not do.
- A `degrade` event stated one hardcoded reason for three different causes, so a failing fetch of an archive claimed the reference named a directory.
- A zip symlink member was read before the expansion guard observed its size, so a 100 MB archive could force an allocation near 100 GB.
- Selection globs backtracked, so eight star components took 143 ms and each further one multiplied that by about 5.8. Patterns arrive in remote manifests.
- Reconcile was quadratic in entry count. At the contracted limit it extrapolated to about 269 seconds.
- Manifests and plans were read whole before their size was checked.
- `corroborated` is documented as unreachable in this build, which it always was, and a test pins it in both directions.

### Removed

- `--track` is gone from the Not built list rather than built. Detection costs nothing, because every completed run already writes the record `status`, `diff`, `revert` and `promote` read, so the flag would have been a second way of doing what the tool does.
- `cargo xtask check-comments`. A checker for a taste rule is code shipped to police taste.
- `audit.md`, `audit2.md`, `plan.md`, and eleven documentation files replaced by the four that remain.
