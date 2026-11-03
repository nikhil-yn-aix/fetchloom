# developer guide

fetchloom's rust crate resolves research data, stores checked bytes and places a project's pinned inputs.

## start here

install [rust](https://rustup.rs), then follow [build requirements](engineering/building-and-testing.md#build-requirements) for the toolchain pinned in `rust-toolchain.toml` and native compilers. x86_64 windows supports GNU or MSVC; the release workflow uses MSVC. linux builds need a C compiler and linker.

from the repository root, build and run one exact test:

```sh
cargo build --locked
cargo test --locked --lib threads::tests::archives_share_the_cap_between_readers_and_writers -- --exact
```

| next step | route |
|---|---|
| choose the narrowest regression test | [select narrowly](engineering/building-and-testing.md#select-narrowly), then the component map below |
| review or update insta output | [update snapshots](engineering/building-and-testing.md#update-snapshots) with `cargo insta review` or `INSTA_UPDATE` |
| test the python helper | [python tests](engineering/building-and-testing.md#python-and-live-tests) |
| run ignored or live tests | [live checks](engineering/building-and-testing.md#python-and-live-tests) |
| run the linux gate from windows | [WSL gate](engineering/building-and-testing.md#wsl-gate) |
| reproduce a reported problem | [debugging evidence](engineering/profiling.md#reproduce-a-reported-problem): verbose version, timed debug logs, JSON counters and per-file traces |
| measure a performance change | [running benchmarks](engineering/benchmarks.md) |
| prepare a change for review | [engineering loop](engineering/README.md#the-fast-loop) and [style](style.md) |

## repository map

| folder | what lives there |
|---|---|
| [.github](https://github.com/nikhil-yn-aix/fetchloom/tree/main/.github) | CI and release workflows, including linux, windows and macos on x86_64 and arm64 |
| [bench](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md) | linux benchmark runner, native client adapters, public-service comparisons and result rendering |
| [docs](../README.md) | [user documentation](../user/README.md) and this contributor guide |
| [python](https://github.com/nikhil-yn-aix/fetchloom/tree/main/python) | a typed subprocess wrapper around `fetchloom --json` and its unittest cases; no rust bindings |
| [src](https://github.com/nikhil-yn-aix/fetchloom/tree/main/src) | one rust crate, its executable entry point and module tests |
| [tests](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests) | real-binary CLI tests, captured source responses, archive fixtures and output snapshots |

[Cargo.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/Cargo.toml) sets dependencies, lints and build profiles; [rust-toolchain.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/rust-toolchain.toml) pins the toolchain. [pyproject.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/pyproject.toml) packages the same executable with the python helper. [releasing](engineering/releasing.md) owns planned release artifacts and installers.

## read the code in this order

| start with | why |
|---|---|
| [main.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/main.rs), [lib.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/lib.rs) | see the entire public entry point and the module list |
| [cli/args.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/args.rs), [cli.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli.rs) | follow arguments into settings, cancellation and command dispatch |
| [project/manifest.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/manifest.rs), [project/lock.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock.rs), [project/state.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/state.rs) | distinguish requested data, pinned bytes and observations about local files |
| [cli/project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/project.rs), then [cli/sync.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/sync.rs) | follow `sync` from its project lock through scanning, resolution and placement |
| [cli/sync/disk.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/sync/disk.rs), [cli/sync/place.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/sync/place.rs) | understand why an existing file is kept, adopted or replaced before changing disk behavior |
| [cli/search.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/search.rs), [source/search.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/search.rs), [output/picker.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/picker.rs), [cli/list.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/list.rs) | separate catalog discovery, the arrow-key choice, explicit selection and project inventory from downloads |
| [source.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source.rs), then one adapter | see how a reference becomes a listing without mixing source policy into the downloader |
| [net/download.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/download.rs), [cache/partial.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cache/partial.rs), [cache/partial/hashing.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cache/partial/hashing.rs), [cache/deferred.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cache/deferred.rs) | trace response bytes through the writer, hash worker and cache publication, including the one flush per batch on linux |
| [output.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output.rs), [output/style.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/style.rs) | see how every line is styled, the color tones and how `FETCHLOOM_COLORS` changes them; each command's own module, such as `cli/list.rs`, assembles its tables |
| [archive.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/archive.rs), [plan.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/plan.rs), [place.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/place.rs) | follow archive members into checked destinations and filesystem operations |
| [tests/cli/support.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/support.rs), then the matching scenario | learn how to reproduce behavior with local servers and isolated folders |

read the [architecture map](architecture/README.md) alongside this route. private types keep their contracts beside their implementations; you do not need to understand every adapter to change one.

## where to start

commands run from the repository root. start with one named test inside the indicated module, then widen to these filters.

| change | page and first file | tests to run |
|---|---|---|
| add a source | [sources](architecture/sources.md), [source.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source.rs) | `cargo test --locked --lib source::`; `cargo test --locked --test cli get::`; `cargo test --locked --test cli project::` |
| change output | [output](architecture/output.md), [output.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output.rs) | `cargo test --locked --lib output::`; `cargo test --locked --test cli progress::`; `cargo test --locked --test cli hints::` |
| fix a placement bug | [placing](architecture/placing.md), [cli/sync/place.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/sync/place.rs) | `cargo test --locked --lib place::`; `cargo test --locked --test cli placing::`; `cargo test --locked --test cli recovery::` |
| change the network layer | [network](architecture/network.md), [net.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net.rs) | `cargo test --locked --lib net::`; `cargo test --locked --test cli split::`; `cargo test --locked --test cli proxy::`; `cargo test --locked --test cli secrets::` |
| add a CLI option | [CLI](architecture/cli.md), [cli/args.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/args.rs) | `cargo test --locked --lib cli::args::`; `cargo test --locked --test cli basics::`; `cargo test --locked --test cli caps::` |

[engineering](engineering/README.md) explains the narrow-test-to-full-gate loop. [performance](performance/README.md) records costs and limits; [decisions](decisions.md) records reasons; [roadmap](roadmap.md) holds pending work. use [style](style.md) for prose, comments and commits.

for behavior rather than implementation, start with [concepts](../user/concepts.md), [commands](../user/commands/README.md) or [sources](../user/sources/README.md).
