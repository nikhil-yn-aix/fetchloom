# building and testing

one rust crate builds the library and executable, with module tests and real-binary scenarios covering different contracts.

## build requirements

[rust-toolchain.toml](../../../rust-toolchain.toml) pins rust 1.99.1 with rustfmt and clippy. [Cargo.toml](../../../Cargo.toml) uses edition 2024 and declares a minimum rust version of 1.99. native build tools are needed for compression dependencies and the crypto provider. keep `Cargo.lock` when building the application.

```sh
cargo build --locked
cargo run --locked -- --version
```

`publish = false` keeps the rust crate off crates.io. [pyproject.toml](../../../pyproject.toml) takes the package version from Cargo and uses maturin's binary binding mode; python calls the executable rather than rust bindings.

## test layers

| location | checks | command |
|---|---|---|
| inline module tests under [src](../../../src) | parsers, hashes, paths, limits, rendering and storage | `cargo test --locked --lib` |
| [source tests](../../../src/source) with [recorded fixtures](../../../tests/fixtures/sources) | source-specific responses through `Api` and `Recorded` | `cargo test --locked --lib source::` |
| [archive tests](../../../src/archive) with [archive fixtures](../../../tests/fixtures/archives) | decoding, damaged inputs and unsafe entries | `cargo test --locked --lib archive::` |
| [CLI tests](../../../tests/cli) | compiled executable against scripted local servers | `cargo test --locked --test cli` |
| [snapshots](../../../tests/cli/snapshots) | human output and selected JSON or lock text | run the owning CLI module |
| [python tests](../../../python/test_helper.py) | wrapper arguments, paths and structured errors | `python3 -m unittest discover -s python` |

`tests/cli/main.rs` declares scenario modules; `support.rs` owns the custom `TcpListener` server and sandbox. it creates private work, cache and home folders and removes inherited fetchloom settings, source tokens, proxies and synced-folder settings. normal OS inputs such as `PATH`, `CI` and `TERM` remain. tests use local servers or recorded answers, not live repositories.

snapshot filters normalize sandbox paths, ports, separators and durations and remove variable rates. inspect raw output when changing those fields. JSON work counters and output snapshots check different contracts.

### select narrowly

```sh
cargo test --locked --lib threads::tests::archives_share_the_cap_between_readers_and_writers -- --exact
cargo test --locked --test cli basics::the_options_scope_names_are_the_ones_add_and_get_take -- --exact
```

the second test checks a documentation option sentence against `add` and `get` help. other markdown transcripts are not executed. use `-- --list` with a filter when checking the test's exact name; a zero-test run is not coverage.

### code gates

unsafe code is forbidden. configured warnings include missing public documentation, unreachable public items, clippy pedantic checks, unwraps, expects, panics, debug prints and unfinished-code macros. lint exceptions need reasons; clippy treats all warnings as errors. there is no enforced file-length or function-length limit.

run the [full gate](README.md#the-fast-loop) before merging. after dependency changes also run `cargo deny --locked check advisories licenses bans sources`; install `cargo-deny` with `cargo install --locked cargo-deny` only if it is missing.

## WSL gate

use a linux clone whose `origin` is your windows repository's local path, not GitHub. with that clone at `~/fetchloom`, these commands test its local `main` without a push. fetch the working branch instead when checking an unmerged change.

```sh
git -C ~/fetchloom fetch origin main
git -C ~/fetchloom worktree add --detach ~/fetchloom-gate origin/main
cd ~/fetchloom-gate
export CARGO_TARGET_DIR="$HOME/fetchloom-target"
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

keep build output on the linux filesystem. the separate worktree lets windows and linux checks avoid rewriting the same build files. once finished, leave the worktree and remove it with `git -C ~/fetchloom worktree remove ~/fetchloom-gate`.

## xfs loop mount

ext4 does not exercise reflinks. on linux with `xfsprogs` installed, a local image gives the placement tests an xfs destination. the mount commands need root; run cargo as your normal user.

```sh
mkdir -p .local-test/xfs
truncate -s 4G .local-test/xfs.img
mkfs.xfs -q .local-test/xfs.img
sudo mount -o loop .local-test/xfs.img .local-test/xfs
sudo chown "$(id -u):$(id -g)" .local-test/xfs
TMPDIR="$PWD/.local-test/xfs" cargo test --locked --test cli placing::
sudo umount .local-test/xfs
rm .local-test/xfs.img
rmdir .local-test/xfs .local-test
```

unmount even if tests fail. do not delete an image while it is mounted. [CI](ci.md) runs every test on xfs and btrfs, and on ReFS, exFAT and FAT32 on windows.

## adding coverage

keep parsers and pure planning tests close to the module. inject `Api` responses, environment readers or archive limits where the code already exposes those seams. retry timing still uses a real clock and randomness; it is not a fully simulated scheduler. use a CLI scenario for interrupted replacement, locked-source recovery or anything whose contract includes on-disk leftovers.

nextest, wiremock, proptest, cargo-fuzz, cargo-mutants, cargo-shear, typos, vale, zizmor and a justfile are not configured. add tooling with the job it will perform rather than treating it as an existing requirement.

related: [engineering loop](README.md), [fuzzing](fuzzing.md), [CI](ci.md), [architecture](../architecture/README.md).
