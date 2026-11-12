# building and testing

one rust crate builds the library and executable, with module tests and real-binary scenarios covering different contracts.

## build requirements

[rust-toolchain.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/rust-toolchain.toml) pins rust 1.99.1 with rustfmt and clippy. [Cargo.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/Cargo.toml) uses edition 2024 and declares a minimum rust version of 1.99. native build tools are needed for compression dependencies and the crypto provider. keep `Cargo.lock` when building the application.

install rust through [rustup](https://rustup.rs). on linux, provide a C compiler and linker through the distribution's build packages. on windows, both `x86_64-pc-windows-msvc` and `x86_64-pc-windows-gnu` build the crate. `x86_64-pc-windows-msvc` is the x86_64 release target and the closest match to CI: install Visual Studio Build Tools with the C++ workload and Windows SDK. a GNU toolchain needs a matching MinGW-w64 compiler and linker on PATH. do not mix MSVC and GNU native dependencies. arm64 windows releases use MSVC. the toolchain file pins the rust version and components, not the host triple; no repository `.cargo` configuration overrides the target. use `rustup show` to check your selected host. on macos, `xcode-select --install` provides the C compiler, linker and SDK; both `x86_64-apple-darwin` and `aarch64-apple-darwin` build natively. checking a macos target from linux needs an apple C compiler as well, since ring's build script compiles C and assembly even for `cargo check`.

```sh
git clone https://github.com/nikhil-yn-aix/fetchloom.git
cd fetchloom
rustup toolchain install
cargo build --locked
cargo run --locked -- --version
```

the first cold build can take several minutes, especially while compiling native compression libraries and optimized development dependencies. subsequent focused test runs reuse those artifacts. python helper tests need python 3.10 or newer, matching `requires-python` in [pyproject.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/pyproject.toml).

the crate package includes `src`, `Cargo.toml`, `Cargo.lock`, the license and notice files, the README, and the two reference pages its help tests check (`environment.md` and `exit-codes.md`); `include` in [Cargo.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/Cargo.toml) keeps test data, other docs and the benchmark out. `fetchloom help` prints shorter built-in topics, not the full reference pages. [pyproject.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/pyproject.toml) takes the package version from Cargo and uses maturin's binary binding mode; python calls the executable rather than rust bindings.

## test layers

| location | checks | command |
|---|---|---|
| inline module tests under [src](https://github.com/nikhil-yn-aix/fetchloom/tree/main/src) | parsers, hashes, paths, limits, rendering and storage | `cargo test --locked --lib` |
| [source tests](https://github.com/nikhil-yn-aix/fetchloom/tree/main/src/source) with [recorded fixtures](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/fixtures/sources) | source-specific responses through `Api` and `Recorded` | `cargo test --locked --lib source::` |
| [archive tests](https://github.com/nikhil-yn-aix/fetchloom/tree/main/src/archive) with [archive fixtures](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/fixtures/archives) | decoding, damaged inputs and unsafe entries | `cargo test --locked --lib archive::` |
| [CLI tests](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/cli) | compiled executable against scripted local servers | `cargo test --locked --test cli` |
| [snapshots](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/cli/snapshots) | human output and selected JSON or lock text | run the owning CLI module |
| [python tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/python/test_helper.py) | wrapper arguments, paths, structured errors and the `fl` launcher | `python3 -m unittest discover -s python` |

`tests/cli/main.rs` declares scenario modules; `support.rs` owns the custom `TcpListener` server and sandbox. it creates private work, cache and home folders and removes inherited fetchloom settings, source tokens, proxies and synced-folder settings. normal OS inputs such as `PATH`, `CI` and the filesystem temporary-directory settings remain. children use `LC_ALL=C.UTF-8`, `TERM=xterm` and `CLICOLOR=0` so a contributor's locale or terminal does not change snapshots. they also get `FETCHLOOM_TEST_TICKER=off`, which debug builds read to skip the lines a long batch prints as time passes, since how busy the machine is decides when those come; a test of those lines sets it to `on`. a test of another setting overrides it on its own `Command`, without changing the test process environment. [server ownership](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/support/server.rs) closes listeners and active connections and joins workers on drop. a worker panic fails its owning test; cleanup during an existing test panic preserves the original panic instead of aborting on a second one. `Running` drains stdout and stderr independently while a test waits for a stderr event, then returns both byte streams when the child finishes. race tests wait without polling where something announces the event: `Server::wait_for_requests` wakes at each request and `Running::wait_for` at each stderr line. `wait_until` polls only state nothing announces, such as the partial files another process writes. a race the code under test would win on a fast disk gets a debug-only hold instead: `FETCHLOOM_TEST_HOLD_PUSH` names a file, and a push holds each copy once its part is in the mirror's `tmp/` until that file appears or a stop is asked, so a stop lands mid-copy even where files clone at once. their deadlines only keep a broken test from hanging; fixed pauses belong only to timed response behavior. on linux, terminal scenarios use `script` with `/bin/sh`, ignore shell startup-file overrides, and report a nonzero command exit as an error. they skip that coverage when `script` is absent. default tests use local servers or recorded answers. ignored tests cover live repositories.

the recipe tests in `tests/cli/build.rs` run [examples/recipe_step.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/examples/recipe_step.rs), a step program with the standard library alone whose first argument picks what it does: copy, write, fail, hold until a file appears, and the like. `cargo test` builds it beside the fetchloom binary, and a run of the CLI tests alone refreshes it once with the `cargo` that runs them; CI prebuilds and archives it, so each nextest process reuses it without invoking cargo; each test copies it into the sandbox's `bin` folder as `recipe-step` and puts that folder first on `PATH`, so a recipe names it as any program. every run appends its mode to `runs` in the folder the step runs in, so a test counts runs without sleeping.

snapshot filters normalize sandbox paths, ports, separators and durations and remove variable rates. on a drive that makes no links, `Sandbox::placed_filters` turns the copy wording of a story into the linking wording, so one snapshot covers every filesystem. inspect raw output when changing those fields. JSON work counters and output snapshots check different contracts.

### test directories

if you override `TMPDIR` on linux or macos, or `TMP` and `TEMP` on windows, choose an existing writable folder outside every git repository and fetchloom project. `Sandbox::new` uses `tempfile::tempdir`, and commands still inspect parent folders for project discovery and git integration. a test folder inside a checkout can therefore produce extra `.gitignore` files or different snapshots. do not accept those differences as intended output.

### select narrowly

```sh
cargo test --locked --lib threads::tests::archives_share_the_cap_between_readers_and_writers -- --exact
cargo test --locked --test cli basics::the_options_scope_names_are_the_ones_add_and_get_take -- --exact
```

the second test checks a documentation option sentence against `add` and `get` help. `tests/cli/docs.rs` checks the rest of the user documentation against `help --json`: every option of every command is on its command page, every `fetchloom` line in a fenced shell example names a real command and real options, and every `FETCHLOOM_` setting the code reads is in the environment reference, while every setting the user docs name is read by fetchloom or its installers, and the agent skill and llms.txt name every command and option, each option on a line that names its command. a renamed or new option therefore fails the suite until its page changes too. `python/test_docs.py` checks that links reach their files and headings, and that SUMMARY.md, the user guide index and llms.txt each list every user page, since a page SUMMARY.md leaves out is never published. markdown transcripts are not executed. use `-- --list` with a filter when checking the test's exact name; a zero-test run is not coverage.

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

on windows, use `py -3 -m unittest discover -s python`, or `python -m unittest discover -s python` when python 3.10 or later is on PATH. helper tests use the standard library and mocked subprocess results; they do not need a package install or a live dataset. nine ignored rust tests need working DNS, HTTPS and access to public records at zenodo, figshare, dataverse, OSF, hugging face, kaggle, doi.org, s3 and google cloud storage. all but one construct clients without credentials; the kaggle competition test uses a kaggle key when one is found and skips without it. allow time for real network retries and service rate limits; an unavailable or changed upstream record can fail them. Cargo's `--offline` only disables dependency downloads; it does not make live tests offline. [live-source checks](ci.md#live-source-checks) run them weekly and on request, separately from the required CI result.

### installed package checks

[artifact CI](ci.md#installed-artifacts) tests the packages rather than borrowing a source-tree executable. `python/ci_journey.py --binary /absolute/path/to/fetchloom --version 0.1.0` runs the deterministic loopback journey locally; `--commit` also checks the build identity. `--work-root` chooses an existing scratch parent outside the checkout. the wheel and installer checks call that same journey after installing into isolated locations. their command-line help lists the required candidate archive and proof paths.

`cargo package --locked` verifies the source distribution; CI then unpacks its `.crate`, runs `cargo install --path` on that unpacked tree, and checks the installed program. `python3 -m unittest discover -s python -p test_artifacts.py` tests the package-inventory and assertion logic without building Rust. no external dataset service is needed.

### coverage and mutation pilots

[scheduled deep checks](ci.md#scheduled-deep-checks) own weekly linux coverage, longer Bolero runs and external links. coverage includes CLI child processes and publishes LCOV/JSON plus a step summary, without a coverage quota. manual mutation dispatch names one Rust source file and tests at most ten listed mutations, always after an unmutated baseline. neither check replaces a focused regression assertion.

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
cargo clippy --release --locked -- -D warnings
cargo test --locked
python3 -m unittest discover -s python
python3 -m unittest discover -s bench
```

keep build output on the linux filesystem. the separate worktree lets windows and linux checks avoid rewriting the same build files. once finished, leave the worktree and remove it with `git -C ~/fetchloom worktree remove ~/fetchloom-gate`.

## xfs loop mount

ext4 does not exercise reflinks. on linux with `xfsprogs` installed, a local image gives the placement tests an xfs destination. the mount commands need root; run cargo as your normal user. the example puts test files outside the checkout; confirm that your home folder is not itself a git repository or fetchloom project.

```sh
scratch="$HOME/fetchloom-xfs-$$"
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

unmount even if tests fail. do not delete an image while it is mounted. [CI](ci.md) reruns the tests that touch the disk on xfs and btrfs, on ReFS, exFAT and FAT32 on windows, and on hfs+ on macos; the full suite runs once on each default filesystem.

a fat image that root mounts with `sudo mount -o loop,umask=000` lets every user write but refuses chmod to all but root, as some shared drives do. `FETCHLOOM_TEST_NOOP_CHMOD=<mount> cargo test --locked --lib place::tests::a_writable_copy_works_where_the_mount_refuses_chmod -- --exact` checks that a writable copy still works there, as linux CI does; without the variable that test returns at once.

on nfs 4.2 from a linux server that hands out delegated timestamps (RFC 9754), as 7.0 does, the tests that fake an edit a coarse clock hides, by writing a file and putting its old time back, fail by design: the server keeps a time the client sets under a write delegation only if it is newer than the file's times when the delegation began, and stamps the file when the delegation ends. run those on a local filesystem.

## adding coverage

keep parsers and pure planning tests close to the module. inject `Api` responses, environment readers or archive limits where the code already exposes those seams. a network test under `src/net` that needs exact answers in order uses [scripted.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/scripted.rs), which answers each connection with the next response byte for byte and keeps each request's head; the rate limit and split download tests keep servers of their own, which count requests per second or serve byte ranges. retry timing still uses a real clock and randomness; it is not a fully simulated scheduler. use a CLI scenario for interrupted replacement, locked-source recovery or anything whose contract includes on-disk leftovers.

CI uses nextest, strict insta checks, typos and zizmor as [CI](ci.md) describes; local `cargo test` remains supported. wiremock, proptest, cargo-fuzz, cargo-mutants, cargo-shear, vale and a justfile are not configured. add tooling with the job it will perform rather than treating it as an existing requirement.

related: [engineering loop](README.md), [fuzzing](fuzzing.md), [CI](ci.md), [architecture](../architecture/README.md).
