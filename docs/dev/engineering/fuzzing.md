# fuzzing

bolero exercises parsers inside their modules without exposing a separate public fuzzing API.

## targets

| test target | source | property |
|---|---|---|
| `archive::names::tests::fuzz_member_names` | `src/archive/names.rs` | accepted member names obey path and collision rules |
| `archive::input::tests::joined_parts_read_and_seek_as_the_whole` | `src/archive/input.rs` | reads and seeks over bytes cut into parts anywhere, parts of no bytes too, give what they give over the bytes whole |
| `archive::sevenz::tests::fuzz_seven_zip` | `src/archive/sevenz.rs` | a 7z with any of its bytes changed never panics, and fails only as a damaged or refused archive |
| `project::lock::canonical::tests::fuzz_lock_reader` | `src/project/lock/canonical.rs` | the fast lock reader agrees with validated lock parsing |
| `project::manifest::tests::fuzz_manifest` | `src/project/manifest/tests.rs` | accepted manifests keep their invariants through dataset edits |
| `source::drive::folder::tests::fuzz_drive_pages` | `src/source/drive/folder.rs` | folder and download-page parsing remains safe for arbitrary text |
| `net::headers::tests::fuzz_response_headers` | `src/net/headers.rs` | response headers and suggested filenames remain valid when accepted |
| `source::xml::tests::fuzz_object_listings` | `src/source/xml.rs` | the XML reader never panics, and accepted elements have valid names |
| `auth::cookies::sqlite::tests::fuzz_cookie_databases` | `src/auth/cookies/sqlite/tests/built.rs` | a cookie database the input plans, built as SQLite writes one and then bent, never panics the reader or the firefox and chromium readers over it, nor gives more rows than its leaf pages hold; unbent, it reads as built |

all targets must avoid panics. where a writer exists, round trips must preserve the accepted value.

the cookie database target reads its input as a plan rather than as a file: firefox's, chromium's or a table of any statement, pages of 512 bytes to 64 KiB with or without reserved bytes, up to 3,840 rows, most plans far fewer, values long enough to spill onto overflow pages, an index, free pages that still hold old rows, and a write-ahead log of one or two commits, then one never committed, summed big- or little-endian. it builds that database page by page, then makes the plan's edits: a child or overflow pointer turned into a cycle or a shared page, a cell pointer listed twice, a page made an interior page, a commit claiming pages it lacks, a row's size or header made the largest a number says, a statement's characters changed, bytes changed or cut, and the log summed again unless the plan says not to. a test has SQLite's own `PRAGMA integrity_check` pass on what the builder writes, and read the same rows, so the target bends real databases. each plan stays under 128 KiB of rows a version, and the reader takes at most 4 MiB of rows an input. its seeds in `tests/fixtures/fuzz/cookie-databases` are two browser files as they are and one plan for each way a crafted file broke the reader: a cycle, pages shared down 24 levels, a cell listed twice, an overflow page that leads back to itself under a commit that claims every page, a broken statement and a row header that claims the most bytes; a test checks that each reaches its case, and `write_the_seeds`, ignored, writes them again after the plan's bytes change. the test body remains beside the parser so a minimized failing input can become a nearby regression case.

## ordinary test mode

`cargo test` runs each target for one second of random input. run only those tests with:

```sh
cargo test --locked --lib -- fuzz_ joined_parts_read_and_seek_as_the_whole
```

this short pass is useful for local and CI checks; it is not a substitute for sustained exploration of a changed parser.

## longer linux runs

install the pinned nightly and cargo-bolero when preparing a longer run:

```sh
rustup toolchain install nightly-2026-11-03 --profile minimal
cargo install --locked cargo-bolero --version 0.13.5 --no-default-features --features libfuzzer
mkdir -p .fuzz/manifest .fuzz/manifest-crashes
ASAN_OPTIONS=detect_odr_violation=1 cargo bolero test --toolchain nightly-2026-11-03 --target x86_64-unknown-linux-gnu -T 10min --corpus-dir .fuzz/manifest --crashes-dir .fuzz/manifest-crashes project::manifest::tests::fuzz_manifest
```

`ASAN_OPTIONS=detect_odr_violation=1` keeps the address sanitizer's check of one definition made twice to definitions of different sizes. the tests build cookie databases with SQLite, which `cc` compiles with the sanitizer as rustc instruments the Rust code; its one C file holds two equal tables named `aiOff`, which gcc's sanitizer otherwise reports as one definition made twice before any target starts.

`Cargo.toml` defines the `fuzz` profile: optimized builds with debug assertions and overflow checks retained. explicit corpus and crash folders keep the run's files together. without those arguments bolero keeps its corpus in `__fuzz__` beside the source, which git ignores. the explicit `.fuzz` folder is local run data; do not stage it.

when a crash appears, preserve the input, minimize it, and add a deterministic regression test that fails before fixing the parser. run that test and the parser's module after the fix, then repeat the fuzz target. keep only inputs needed to explain and reproduce the behavior.

[weekly deep checks](ci.md#scheduled-deep-checks) run the nine targets above, including the joined-input property whose name lacks `fuzz_`, one after another from one build. each gets five minutes, one worker, a 10-second input timeout and a 64 KiB input cap, and starts from the seeds in `tests/fixtures/fuzz/<name>` when it has some. the cookie database target also fails on any one allocation past 256 MiB, and runs with `-len_control=0`, so inputs reach full length from the start: its plan's edits follow 29 bytes of header, which libFuzzer's slow growth of input length did not pass in five minutes without seeds. logs and crashes survive a failure for fourteen days; a timeout fails. the scheduled corpus starts fresh from those seeds, without a growing cache. the ordinary short properties remain part of every native CI run.

related: [building and testing](building-and-testing.md), [sources](../architecture/sources.md), [archives](../architecture/archives.md), [project](../architecture/project.md).
