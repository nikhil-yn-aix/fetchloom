# developer guide

fetchloom resolves research-data references into files pinned by SHA-256 and size. it downloads those bytes into a shared cache and places them where a project expects them. a committed `data.toml` and `data.lock` let another machine retrieve the same inputs while local state protects files you edited.

## repository map

| folder | what lives there |
|---|---|
| [.github](../../.github) | CI and release workflows, including linux and windows on x86_64 and arm64 |
| [docs](../README.md) | [user documentation](../user/README.md) and this contributor guide |
| [python](../../python) | a typed subprocess wrapper around `fetchloom --json` and its unittest cases; no rust bindings |
| [src](../../src) | one rust crate, its executable entry point and module tests |
| [tests](../../tests) | real-binary CLI tests, captured source responses, archive fixtures and output snapshots |

[Cargo.toml](../../Cargo.toml) sets dependencies, lints and build profiles; [rust-toolchain.toml](../../rust-toolchain.toml) pins the toolchain. [install.sh](../../install.sh) and [install.ps1](../../install.ps1) install release archives. [pyproject.toml](../../pyproject.toml) packages the same executable in wheels.

## read the code in this order

| start with | why |
|---|---|
| [main.rs](../../src/main.rs), [lib.rs](../../src/lib.rs) | see the entire public entry point and the module list |
| [cli/args.rs](../../src/cli/args.rs), [cli.rs](../../src/cli.rs) | follow arguments into settings, cancellation and command dispatch |
| [project/manifest.rs](../../src/project/manifest.rs), [project/lock.rs](../../src/project/lock.rs), [project/state.rs](../../src/project/state.rs) | distinguish requested data, pinned bytes and observations about local files |
| [cli/project.rs](../../src/cli/project.rs), then [cli/sync.rs](../../src/cli/sync.rs) | follow `sync` from its project lock through scanning, resolution and placement |
| [cli/sync/disk.rs](../../src/cli/sync/disk.rs), [cli/sync/place.rs](../../src/cli/sync/place.rs) | understand why an existing file is kept, adopted or replaced before changing disk behavior |
| [source.rs](../../src/source.rs), then one adapter | see how a reference becomes a listing without mixing source policy into the downloader |
| [net/download.rs](../../src/net/download.rs), [cache/partial.rs](../../src/cache/partial.rs), [cache/partial/hashing.rs](../../src/cache/partial/hashing.rs) | trace response bytes through the writer, hash worker and cache publication |
| [archive.rs](../../src/archive.rs), [plan.rs](../../src/plan.rs), [place.rs](../../src/place.rs) | follow archive members into checked destinations and filesystem operations |
| [tests/cli/support.rs](../../tests/cli/support.rs), then the matching scenario | learn how to reproduce behavior with local servers and isolated folders |

read the [architecture map](architecture/README.md) alongside this route. private types keep their contracts beside their implementations; you do not need to understand every adapter to change one.

## where to start

commands run from the repository root. start with one named test inside the indicated module, then widen to these filters.

| change | page and first file | tests to run |
|---|---|---|
| add a source | [sources](architecture/sources.md), [source.rs](../../src/source.rs) | `cargo test --locked --lib source::`; `cargo test --locked --test cli get::`; `cargo test --locked --test cli project::` |
| change output | [output](architecture/output.md), [output.rs](../../src/output.rs) | `cargo test --locked --lib output::`; `cargo test --locked --test cli progress::`; `cargo test --locked --test cli hints::` |
| fix a placement bug | [placing](architecture/placing.md), [cli/sync/place.rs](../../src/cli/sync/place.rs) | `cargo test --locked --lib place::`; `cargo test --locked --test cli placing::`; `cargo test --locked --test cli recovery::` |
| change the network layer | [network](architecture/network.md), [net.rs](../../src/net.rs) | `cargo test --locked --lib net::`; `cargo test --locked --test cli split::`; `cargo test --locked --test cli proxy::`; `cargo test --locked --test cli secrets::` |
| add a CLI option | [CLI](architecture/cli.md), [cli/args.rs](../../src/cli/args.rs) | `cargo test --locked --lib cli::args::`; `cargo test --locked --test cli basics::`; `cargo test --locked --test cli caps::` |

[engineering](engineering/README.md) explains the narrow-test-to-full-gate loop. [performance](performance/README.md) records costs and limits; [decisions](decisions.md) records reasons; [roadmap](roadmap.md) holds pending work. use [style](style.md) for prose, comments and commits.

for behavior rather than implementation, start with [concepts](../user/concepts.md), [commands](../user/commands/README.md) or [sources](../user/sources/README.md).
