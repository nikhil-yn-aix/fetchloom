# developer guide

fetchloom is one rust crate that resolves research data, keeps checked bytes in a shared cache and places each project's pinned files.

## start here

install [rust](https://rustup.rs) and your system's native build tools, as [build requirements](engineering/building-and-testing.md#build-requirements) lists; `rust-toolchain.toml` pins the rust version.

from the repository root, build and run one exact test:

```sh
cargo build --locked
cargo test --locked --lib threads::tests::archives_share_the_cap_between_readers_and_writers -- --exact
```

then work in the [engineering loop](engineering/README.md#the-fast-loop): a failing test at the lowest layer first, the full gate before review.

| to | go to |
|---|---|
| find the file that owns something | [code map](architecture/codemap.md) |
| see how the parts fit | [architecture](architecture/README.md) |
| choose the narrowest test | [select narrowly](engineering/building-and-testing.md#select-narrowly), then [common changes](#common-changes) |
| review or update snapshots | [update snapshots](engineering/building-and-testing.md#update-snapshots), with `cargo insta review` or `INSTA_UPDATE` |
| run the python, ignored or live tests | [python and live tests](engineering/building-and-testing.md#python-and-live-tests) |
| run the linux gate from windows | [WSL gate](engineering/building-and-testing.md#wsl-gate) |
| reproduce a reported problem | [debugging evidence](engineering/profiling.md#reproduce-a-reported-problem): verbose version, timed debug logs, JSON counters and per-file traces |
| measure a performance change | [running benchmarks](engineering/benchmarks.md), and past results in [performance](performance/README.md) |
| know why a boundary exists | [decisions](decisions.md) |
| see pending work | [roadmap](roadmap.md) |
| write prose, comments and commits | [style](style.md) |
| learn what fetchloom does for a user | the user guide's [concepts](../user/concepts.md), [commands](../user/commands/README.md) and [sources](../user/sources/README.md) |

## read the code in this order

private types keep their contracts beside their code, so changing one adapter does not need the others.

| read | to see |
|---|---|
| `src/main.rs`, `src/lib.rs` | the entire public entry point and the module list |
| `src/cli/args.rs`, `src/cli.rs` | arguments becoming settings, cancellation and command dispatch |
| `src/project/manifest.rs`, `src/project/lock.rs`, `src/project/state.rs` | requested data, pinned bytes, and what is known about local files |
| `src/cli/project.rs`, then `src/cli/sync.rs` | `sync` from the project lock through scanning, resolution and placement |
| `src/cli/sync/disk.rs`, `src/cli/sync/place.rs` | why an existing file is kept, adopted or replaced, before you change disk behavior |
| `src/cli/search.rs`, `src/source/search.rs`, `src/output/picker.rs`, `src/cli/list.rs` | catalog discovery, the arrow-key choice, explicit selection and the project inventory, apart from downloads |
| `src/source.rs`, then one adapter | a reference becoming a listing, with source policy kept out of the downloader |
| `src/net/download.rs`, `src/cache/partial.rs`, `src/cache/partial/hashing.rs`, `src/cache/deferred.rs` | response bytes through the writer and hash worker into the cache, flushed as [batch durability](architecture/network.md#batch-durability) says |
| `src/output.rs`, `src/output/style.rs` | how every line is styled and how `FETCHLOOM_COLORS` changes the tones; each command's file, such as `src/cli/list.rs`, builds its own tables |
| `src/archive.rs`, `src/plan.rs`, `src/place.rs` | archive members becoming checked destinations and filesystem operations |
| `tests/cli/support.rs`, then the matching scenario | reproducing behavior with local servers and isolated folders |

## common changes

start with one named test in the module, then widen to these filters, from the repository root.

| change | read | tests to run |
|---|---|---|
| add a source | [sources](architecture/sources.md), `src/source.rs` | `cargo test --locked --lib source::`; `cargo test --locked --test cli get::`; `cargo test --locked --test cli add::`; `cargo test --locked --test cli project::` |
| change output | [output](architecture/output.md), `src/output.rs` | `cargo test --locked --lib output::`; `cargo test --locked --test cli progress::`; `cargo test --locked --test cli hints::` |
| fix a placement bug | [placing](architecture/placing.md), `src/cli/sync/place.rs` | `cargo test --locked --lib place::`; `cargo test --locked --test cli placing::`; `cargo test --locked --test cli recovery::` |
| change the network layer | [network](architecture/network.md), `src/net.rs` | `cargo test --locked --lib net::`; `cargo test --locked --test cli split::`; `cargo test --locked --test cli proxy::`; `cargo test --locked --test cli secrets::` |
| add a CLI option | [CLI](architecture/cli.md), `src/cli/args.rs` | `cargo test --locked --lib cli::args::`; `cargo test --locked --test cli basics::`; `cargo test --locked --test cli caps::` |

## repository map

| folder | holds |
|---|---|
| [.github](https://github.com/nikhil-yn-aix/fetchloom/tree/main/.github) | CI and release workflows, for linux, windows and macos on x86_64 and arm64 |
| [bench](https://github.com/nikhil-yn-aix/fetchloom/blob/main/bench/README.md) | the linux benchmark runner, native client adapters, public-service comparisons and result rendering |
| [docs](../README.md) | the [user guide](../user/README.md) and this guide |
| [docs-site](https://github.com/nikhil-yn-aix/fetchloom/tree/main/docs-site) | the documentation site's pinned toolchain, its theme and the old addresses that redirect; `zensical.toml` configures the site |
| [python](https://github.com/nikhil-yn-aix/fetchloom/tree/main/python) | a typed subprocess wrapper around `fetchloom --json` and its unittest cases; no rust bindings |
| [src](https://github.com/nikhil-yn-aix/fetchloom/tree/main/src) | the crate, its executable entry point and module tests, file by file in the [code map](architecture/codemap.md) |
| [tests](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests) | real-binary CLI tests, captured source responses, archive fixtures and output snapshots |

`Cargo.toml` sets dependencies, lints and build profiles; `rust-toolchain.toml` pins the toolchain; `pyproject.toml` packages the same executable with the python helper. [releasing](engineering/releasing.md) covers the planned release artifacts and installers.
