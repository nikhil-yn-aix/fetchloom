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
| choose the narrowest test | [select narrowly](engineering/building-and-testing.md#select-narrowly), then [common changes](engineering/common-changes.md) |
| review or update snapshots | [update snapshots](engineering/building-and-testing.md#update-snapshots), with `cargo insta review` or `INSTA_UPDATE` |
| run the python, ignored or live tests | [python and live tests](engineering/building-and-testing.md#python-and-live-tests) |
| run the linux gate from windows | [WSL gate](engineering/other-platforms.md#wsl-gate) |
| reproduce a reported problem | [debugging evidence](engineering/profiling.md#reproduce-a-reported-problem): verbose version, timed debug logs, JSON counters and per-file traces |
| measure a performance change | [running benchmarks](performance/README.md#running-benchmarks), and past results in [performance](performance/README.md) |
| know why a boundary exists | [decisions](decisions/README.md) |
| see pending work | [roadmap](roadmap.md) |
| write prose, comments and commits | [style](style.md) |
| learn what fetchloom does for a user | the user guide's [concepts](../user/concepts/README.md), [commands](../user/commands/README.md) and [sources](../user/sources/README.md) |

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
