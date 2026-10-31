# building and testing

one rust crate builds the library and executable, with module tests and real-binary scenarios covering different contracts.

## build requirements

[rust-toolchain.toml](../../../rust-toolchain.toml) pins rust 1.99.1 with rustfmt and clippy. [Cargo.toml](../../../Cargo.toml) uses edition 2024 and declares a minimum rust version of 1.99. native build tools are needed for compression dependencies and the crypto provider. keep `Cargo.lock` when building the application.

install rust through [rustup](https://rustup.rs). on linux, provide a C compiler and linker through the distribution's build packages. on windows, both `x86_64-pc-windows-msvc` and `x86_64-pc-windows-gnu` build the crate. `x86_64-pc-windows-msvc` is the x86_64 release target and the closest match to CI: install Visual Studio Build Tools with the C++ workload and Windows SDK. a GNU toolchain needs a matching MinGW-w64 compiler and linker on PATH. do not mix MSVC and GNU native dependencies. arm64 windows releases use MSVC. the toolchain file pins the rust version and components, not the host triple; no repository `.cargo` configuration overrides the target. use `rustup show` to check your selected host.

```sh
git clone https://github.com/nikhil-yn-aix/fetchloom.git
cd fetchloom
rustup toolchain install
cargo build --locked
cargo run --locked -- --version
```

the first cold build can take several minutes, especially while compiling native compression libraries and optimized development dependencies. subsequent focused test runs reuse those artifacts. python helper tests need python 3.9 or newer, matching `requires-python` in [pyproject.toml](../../../pyproject.toml).

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

`tests/cli/main.rs` declares scenario modules; `support.rs` owns the custom `TcpListener` server and sandbox. it creates private work, cache and home folders and removes inherited fetchloom settings, source tokens, proxies and synced-folder settings. normal OS inputs such as `PATH`, `CI` and `TERM` remain. default tests use local servers or recorded answers. ignored tests cover live repositories and a larger archive workload.

snapshot filters normalize sandbox paths, ports, separators and durations and remove variable rates. inspect raw output when changing those fields. JSON work counters and output snapshots check different contracts.

### test directories

if you override `TMPDIR` on linux or `TMP` and `TEMP` on windows, choose an existing writable folder outside every git repository and fetchloom project. `Sandbox::new` uses `tempfile::tempdir`, and commands still inspect parent folders for project discovery and git integration. a test folder inside a checkout can therefore produce extra `.gitignore` files or different snapshots. do not accept those differences as intended output.

### select narrowly

```sh
cargo test --locked --lib threads::tests::archives_share_the_cap_between_readers_and_writers -- --exact
cargo test --locked --test cli basics::the_options_scope_names_are_the_ones_add_and_get_take -- --exact
```

the second test checks a documentation option sentence against `add` and `get` help. other markdown transcripts are not executed. use `-- --list` with a filter when checking the test's exact name; a zero-test run is not coverage.

### update snapshots

run the owning test first and inspect the difference. if `cargo insta` is installed, review pending snapshots with `cargo insta review`. install it only when needed with `cargo install --locked cargo-insta`.

to update a focused test without that command, on linux:

```sh
INSTA_UPDATE=always cargo test --locked --test cli basics::get_has_its_own_help -- --exact
git diff -- tests/cli/snapshots
```

on powershell:

```powershell
$env:INSTA_UPDATE = 'always'
try {
    cargo test --locked --test cli basics::get_has_its_own_help -- --exact
} finally {
    Remove-Item Env:\INSTA_UPDATE
}
git diff -- tests/cli/snapshots
```

review the updated text, then rerun without `INSTA_UPDATE`. do not accept a changed snapshot merely to make a test pass.

### python and live tests

```sh
python3 -m unittest discover -s python
cargo test --locked -- --list --ignored
cargo test --locked --lib source::zenodo::tests::live_zenodo -- --ignored --exact
cargo test --locked -- --ignored
```

on windows, use `py -3 -m unittest discover -s python`, or `python -m unittest discover -s python` when python 3.9+ is on PATH. helper tests use the standard library and mocked subprocess results; they do not need a package install or a live dataset. seven ignored rust tests need working DNS, HTTPS and access to public records at zenodo, figshare, dataverse, OSF, hugging face, kaggle and doi.org. they construct clients without credentials and do not require tokens. allow time for real network retries and service rate limits; an unavailable or changed upstream record can fail them. the eighth ignored test writes 20,000 archive members and needs writable scratch storage and time for that filesystem workload. Cargo's `--offline` only disables dependency downloads; it does not make live tests offline. [live-source checks](ci.md#live-source-checks) run them weekly and on request, separately from the required CI result.

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

ext4 does not exercise reflinks. on linux with `xfsprogs` installed, a local image gives the placement tests an xfs destination. the mount commands need root; run cargo as your normal user. the example puts test files outside the checkout; confirm that your home folder is not itself a git repository or fetchloom project.

```sh
scratch="$HOME/fetchloom-xfs-$"
mkdir "$scratch"
mkdir "$scratch/xfs"
truncate -s 4G "$scratch/xfs.img"
mkfs.xfs -q "$scratch/xfs.img"
sudo mount -o loop "$scratch/xfs.img" "$scratch/xfs"
sudo chown "$(id -u):$(id -g)" "$scratch/xfs"
TMPDIR="$scratch/xfs" cargo test --locked --test cli placing::
sudo umount "$scratch/xfs"
rm "$scratch/xfs.img"
rmdir "$scratch/xfs" "$scratch"
```

unmount even if tests fail. do not delete an image while it is mounted. [CI](ci.md) runs every test on xfs and btrfs, and on ReFS, exFAT and FAT32 on windows.

## adding coverage

keep parsers and pure planning tests close to the module. inject `Api` responses, environment readers or archive limits where the code already exposes those seams. retry timing still uses a real clock and randomness; it is not a fully simulated scheduler. use a CLI scenario for interrupted replacement, locked-source recovery or anything whose contract includes on-disk leftovers.

nextest, wiremock, proptest, cargo-fuzz, cargo-mutants, cargo-shear, typos, vale, zizmor and a justfile are not configured. add tooling with the job it will perform rather than treating it as an existing requirement.

related: [engineering loop](README.md), [fuzzing](fuzzing.md), [CI](ci.md), [architecture](../architecture/README.md).
