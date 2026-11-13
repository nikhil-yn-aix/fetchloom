# building and testing

one rust crate builds the library and executable, with module tests and real-binary scenarios covering different contracts.

## build requirements

`rust-toolchain.toml` pins rust 1.99.1 with rustfmt and clippy. `Cargo.toml` uses edition 2024 and declares a minimum rust version of 1.99. native build tools are needed for compression dependencies and the crypto provider. keep `Cargo.lock` when building the application.

install rust through [rustup](https://rustup.rs). on linux, provide a C compiler and linker through the distribution's build packages. on windows, both `x86_64-pc-windows-msvc` and `x86_64-pc-windows-gnu` build the crate. `x86_64-pc-windows-msvc` is the x86_64 release target and the closest match to CI: install Visual Studio Build Tools with the C++ workload and Windows SDK. a GNU toolchain needs a matching MinGW-w64 compiler and linker on PATH. do not mix MSVC and GNU native dependencies. arm64 windows releases use MSVC. the toolchain file pins the rust version and components, not the host triple; no repository `.cargo` configuration overrides the target. use `rustup show` to check your selected host. on macos, `xcode-select --install` provides the C compiler, linker and SDK; both `x86_64-apple-darwin` and `aarch64-apple-darwin` build natively. checking a macos target from linux needs an apple C compiler as well, since ring's build script compiles C and assembly even for `cargo check`.

```sh
git clone https://github.com/nikhil-yn-aix/fetchloom.git
cd fetchloom
rustup toolchain install
cargo build --locked
cargo run --locked -- --version
```

the first cold build can take several minutes, especially while compiling native compression libraries and optimized development dependencies. subsequent focused test runs reuse those artifacts. python helper tests need python 3.10 or newer, matching `requires-python` in `pyproject.toml`.

`build.rs` stamps the commit a build comes from into `FETCHLOOM_COMMIT`, which `--version -v` and its JSON name: a commit `FETCHLOOM_COMMIT` already names at build time, as the release workflow sets, else the `sha1` cargo writes into `.cargo_vcs_info.json` when it packages the crate from git, else `git rev-parse HEAD` where the checkout's top is the crate's own folder, so a crate unpacked inside another repository never takes its commit. a build with none of them says `commit unknown, built outside git`, and JSON gives null. only the hex goes in. cargo runs the script again when this checkout's `HEAD`, its reflog or its branch moves, so a commit makes the crate compile again, which incremental debug builds do in seconds.

the crate package includes `src`, `build.rs`, `Cargo.toml`, `Cargo.lock`, the license and notice files, the README, the three reference pages its tests check (`environment.md`, `exit-codes.md` and `json-output.md`) and the manual page its manual test checks; `include` in `Cargo.toml` keeps test data, other docs and the benchmark out. `fetchloom help` prints shorter built-in topics, not the full reference pages. `pyproject.toml` takes the package version from Cargo and uses maturin's binary binding mode; python calls the executable rather than rust bindings.

## test layers

| location | checks | command |
|---|---|---|
| inline module tests under [src](https://github.com/nikhil-yn-aix/fetchloom/tree/main/src) | parsers, hashes, paths, limits, rendering and storage | `cargo test --locked --lib` |
| [source tests](https://github.com/nikhil-yn-aix/fetchloom/tree/main/src/source) with [recorded fixtures](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/fixtures/sources) | source-specific responses through `Api` and `Recorded` | `cargo test --locked --lib source::` |
| [archive tests](https://github.com/nikhil-yn-aix/fetchloom/tree/main/src/archive) with [archive fixtures](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/fixtures/archives) | decoding, damaged inputs and unsafe entries | `cargo test --locked --lib archive::` |
| [CLI tests](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/cli) | compiled executable against scripted local servers | `cargo test --locked --test cli` |
| [snapshots](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/cli/snapshots) | human output and selected JSON or lock text | run the owning CLI module |
| [python tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/python/test_helper.py) | wrapper arguments, paths, structured errors and the `fl` launcher | `python3 -m unittest discover -s python` |

### select narrowly

```sh
cargo test --locked --lib threads::tests::archives_share_the_cap_between_readers_and_writers -- --exact
cargo test --locked --test cli basics::the_options_scope_names_are_the_ones_add_and_get_take -- --exact
```

the second test checks a documentation option sentence against `add` and `get` help. `tests/cli/docs.rs` checks the rest of the user documentation against `help --json`: every option of every command is on its command page, every `fetchloom` line in a fenced shell example names a real command and real options, and every `FETCHLOOM_` setting the code reads is in the environment reference, while every setting the user docs name is read by fetchloom or its installers, and the agent skill and llms.txt name every command and option, each option on a line that names its command. a renamed or new option therefore fails the suite until its page changes too. it also checks that the [code map](../architecture/codemap.md) has a row for every file under `src/` that is not a test, and none for a file that is gone. `python/test_docs.py` checks that links reach their files and headings, that SUMMARY.md, the user guide index and llms.txt each list every user page, since a page SUMMARY.md leaves out has no place in the site's navigation, and that a nested list, or a block inside a list item, sits four spaces past its item's marker, since the site's markdown otherwise takes it out of the item. markdown transcripts are not executed. use `-- --list` with a filter when checking the test's exact name; a zero-test run is not coverage.

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

the manual page, [docs/man/fetchloom.1](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/man/fetchloom.1), works the same way. `manual::the_manual_page_is_what_the_help_renders` renders it from the command line's definition with clap_mangen and fails when the committed page differs, so a change to the top command's options or to a command's one-line summary needs the page written again:

```sh
FETCHLOOM_TEST_WRITE_MANUAL=1 cargo test --locked --lib manual
man -l docs/man/fetchloom.1
```

### python and live tests

```sh
python3 -m unittest discover -s python
cargo test --locked -- --list --ignored
cargo test --locked --lib source::zenodo::tests::live_zenodo -- --ignored --exact
cargo test --locked -- --ignored
```

on windows, use `py -3 -m unittest discover -s python`, or `python -m unittest discover -s python` when python 3.10 or later is on PATH. helper tests use the standard library and mocked subprocess results; they do not need a package install or a live dataset. 23 ignored rust tests, each named `live_` with the hosts it talks to in its ignore reason, need working DNS, HTTPS and access to public records at zenodo, figshare, dataverse, dryad, mendeley data, OSF, hugging face, kaggle, doi.org, github, gitlab, ENA, GEO, dandi, openneuro, google drive, dropbox, s3, google cloud storage and azure blob storage, and to httpbin.org's basic-auth test page. all but three construct clients without credentials: the kaggle competition test uses a kaggle key when one is found, and the private drive test reads the folder `FETCHLOOM_TEST_LIVE_DRIVE_FOLDER` names with google application default credentials; each skips without them and says so. the httpbin test signs in with a made-up login of its own, which that page accepts by design. allow time for real network retries and service rate limits; an unavailable or changed upstream record can fail them. Cargo's `--offline` only disables dependency downloads; it does not make live tests offline. [live-source checks](ci.md#live-source-checks) run them weekly and on request, separately from the required CI result.

### the documentation site

[uv](https://docs.astral.sh/uv/) installs the site's toolchain, pinned in `docs-site/uv.lock`, then the builder writes the site into `site/` and the checker holds it to the repository:

```sh
uv sync --locked --project docs-site
uv run --locked --project docs-site python python/build_site.py
python3 python/check_site.py
```

the first build downloads mermaid from npm into `docs-site/.cache`. `uv run --locked --project docs-site zensical serve` previews the pages as you edit them, without what the builder adds after zensical: the home page's label and transcripts, transcript colors, the pin and the favicon, mermaid and the pages at old addresses. [CI](ci.md#documentation-site) runs the same build and checks.

`python3 python/docs_index.py` writes each index page below its `<!-- generated by python/docs_index.py -->` line: a table of the pages `docs/SUMMARY.md` nests under it, each with the first sentence of its opening paragraph. run it after adding, moving or retitling a page; `--check` names the index pages that differ, as `test_docs.py` does.

the home page shows a real run, kept in `docs-site/home`: `add.txt`, the stderr of `fetchloom add https://huggingface.co/datasets/scikit-learn/iris` in a new git repository at `~/iris-study`, `sync.txt`, that of `fetchloom sync` in a clone of it with a cache of its own, the `data.lock` the add wrote, and the hub's answers for the dataset, `revision.json` and `tree.json`, saved with curl from the addresses the add asked. each transcript starts with the command after `$ `. the builder makes the label from `data.lock` and shows each transcript under the command a block of `docs/README.md` names, without the lines of work under way. `docs::the_home_page_shows_what_fetchloom_prints` replays the hub's answers to a debug build and fails once fetchloom prints any kept line differently, so a change to those lines needs the run recorded again, with a build of the change and `CLICOLOR=0`.

### installed package checks

[artifact CI](ci.md#installed-artifacts) tests the packages rather than borrowing a source-tree executable. `python/ci_journey.py --binary /absolute/path/to/fetchloom --version 0.1.0` runs the deterministic loopback journey locally; `--commit` also checks the build identity. `--work-root` chooses an existing scratch parent outside the checkout. the wheel and installer checks call that same journey after installing into isolated locations. their command-line help lists the required candidate archive and proof paths.

`cargo package --locked` verifies the source distribution; CI then unpacks its `.crate`, runs `cargo install --path` on that unpacked tree, and checks the installed program. `python3 -m unittest discover -s python -p test_artifacts.py` tests the package-inventory and assertion logic without building Rust. no external dataset service is needed.

### coverage and mutation pilots

[scheduled deep checks](ci.md#scheduled-deep-checks) own weekly linux coverage, longer Bolero runs and external links. coverage includes CLI child processes and publishes LCOV/JSON plus a step summary, without a coverage quota. manual mutation dispatch names one Rust source file and tests at most ten listed mutations against the lib tests, or every test target with `tests=all`, always after an unmutated baseline. neither check replaces a focused regression assertion.

### code gates

unsafe code is forbidden. configured warnings include missing public documentation, unreachable public items, clippy pedantic checks, unwraps, expects, panics, debug prints and unfinished-code macros. lint exceptions need reasons; clippy treats all warnings as errors. there is no enforced file-length or function-length limit.

run the [full gate](README.md#the-fast-loop) before merging. after dependency changes also run `cargo deny --locked check advisories licenses bans sources`; install `cargo-deny` with `cargo install --locked cargo-deny` only if it is missing.

## adding coverage

keep parsers and pure planning tests close to the module. inject `Api` responses, environment readers or archive limits where the code already exposes those seams. a network test under `src/net` that needs exact answers in order uses `src/net/scripted.rs`, which answers each connection with the next response byte for byte and keeps each request's head; the rate limit and split download tests keep servers of their own, which count requests per second or serve byte ranges. a test server must read each request whole, its body by `Content-Length`, before it answers: closing a connection with request bytes unread resets it, and windows then drops the answer its client has not read yet. retry timing still uses a real clock and randomness; it is not a fully simulated scheduler. a test that breaks answers on purpose sets its `Net`'s `first_wait` to zero, as the download tests' `net` helper does, so its tries follow at once while a server's `Retry-After` is still waited for. use a CLI scenario for interrupted replacement, locked-source recovery or anything whose contract includes on-disk leftovers.

CI uses nextest, strict insta checks, typos, zizmor, cargo-deny, cargo-machete and ShellCheck as [CI](ci.md) describes, and the deep checks' manual dispatch runs cargo-mutants; local `cargo test` remains supported. wiremock, proptest, cargo-fuzz, cargo-shear, vale and a justfile are not configured. add tooling with the job it will perform rather than treating it as an existing requirement.

related: [engineering loop](README.md), [fuzzing](fuzzing.md), [CI](ci.md), [architecture](../architecture/README.md).
