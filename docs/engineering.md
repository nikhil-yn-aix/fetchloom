# engineering

## build

`rust-toolchain.toml` pins rust 1.99.1 with rustfmt and clippy. `Cargo.toml` uses edition 2024 and declares a minimum rust version of 1.99. one crate builds a library and two executables, `fetchloom` and `fl`.

```sh
cargo build --locked
cargo run -- --version
```

the compression dependencies need native build tools. keep `Cargo.lock` when building this application. [README](../README.md#install) covers source installation.

## code boundaries

parsers, digest types, glob matching and destination expansion can be tested as functions over data. project and cache modules own disk records. the CLI orders their effects; the HTTP layer also writes through the cache. [architecture](architecture.md#modules) maps the modules.

`Api` is the source-listing test seam. tests also inject environment readers and archive limits. retry timing still uses the real clock and randomness. dependencies are not all hidden behind private adapters: `url::Url`, for example, crosses module boundaries.

the crate forbids unsafe code. cargo configures warnings for missing public docs, unreachable public items and clippy's pedantic group, plus unwraps, expects, panics, debug prints and unfinished-code macros. errors use a hand-written `Error` and `Kind`, not `thiserror`. dead code fails the build like any other warning.

keep modules focused and comments useful. there is no enforced file-length or function-length limit. lint exceptions need reasons. writing and commit rules are in [style](style.md).

## checks

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

run the smallest relevant selector while working, then the full suite when the change crosses command boundaries:

```sh
cargo test --locked --test cli
cargo test --locked --test cli basics::the_options_scope_names_are_the_ones_add_and_get_take
cargo test --locked source::zenodo
cargo test --locked archive::
```

the first command runs the real-binary integration target. the second checks the option sentence in `docs/scope.md`. other markdown examples are not executed by the test suite.

## test layers

| location | checks |
|---|---|
| module `#[cfg(test)]` tests | parsing, rendering, hashes, paths, glob matching, limits and storage behavior |
| `src/source/*` tests | source-specific responses through the in-memory `Recorded` implementation of `Api` |
| `tests/fixtures/sources` | captured JSON listings and errors used by source tests |
| `tests/fixtures/archives` | zip and tar files from several writers, including unsafe and damaged cases |
| `tests/cli` | the compiled binary against a custom `TcpListener` server |
| `tests/cli/snapshots` | human output and selected JSON or lock output checked with insta |

CLI cases cover repeat downloads, retries, resume, source-link recovery, offline use, project edits, archives, cache upkeep, concurrent processes, proxies and secret-bearing links. source tests do not all run a common conformance suite.

the CLI harness creates a private working directory, cache and home. it removes inherited fetchloom settings, source token variables, proxy variables and synced-folder settings before starting the binary. it retains normal OS inputs such as `PATH`, `CI` and `TERM`. tests use local servers or recorded answers, not live repositories.

snapshot filters replace sandbox paths, ports and durations, normalize path separators, and remove variable rates. review the unfiltered output when changing wording; a filtered snapshot does not prove every displayed value is right. snapshots and JSON counters check different contracts.

for a bug fix, first add a test that fails for the reported behavior. use the lowest layer that demonstrates it. add a CLI case when exit codes, stdout, files left on disk or cross-command recovery are part of the bug. do not weaken an assertion to hide a failure.

## automation

[ci.yml](../.github/workflows/ci.yml) runs the three checks above on linux, macos and windows. it then runs every test again with its folders on other filesystems, all required: refs, exfat and fat32 volumes on windows, and btrfs and xfs on linux. there are no nightly live-source checks or release workflows.

nextest, wiremock, proptest, cargo-fuzz, cargo-mutants, cargo-deny, cargo-shear, typos, vale, zizmor and a justfile are not configured in this repository. there is no benchmark harness for instruction counts or allocation profiling. add tooling with the job it will run; do not document it as an existing gate first.

## release work

`Cargo.toml` is the version source and has `publish = false`. no python package, wheel configuration, standalone installer or publishing pipeline is tracked. source builds are the available install path. [plan](plan.md#next) lists the remaining release milestones.

`data.lock` and JSON each use format version 1. the cache writes a layout marker of 1 but does not validate it on open. format readers and recovery behavior are in [formats](formats.md); a version field alone is not a migration system.

## license

contributions use apache-2.0. keep [LICENSE](../LICENSE) and [NOTICE](../NOTICE) with distributions. NOTICE names Nikhil Y N and KairosLab India Private Limited. the [license decision](decisions.md#26-apache-20) records why.
