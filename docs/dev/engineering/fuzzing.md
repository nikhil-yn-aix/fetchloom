# fuzzing

bolero exercises parsers inside their modules without exposing a separate public fuzzing API.

## targets

| test target | source | property |
|---|---|---|
| `archive::names::tests::fuzz_member_names` | [archive/names.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/archive/names.rs) | accepted member names obey path and collision rules |
| `archive::sevenz::tests::fuzz_seven_zip` | [archive/sevenz.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/archive/sevenz.rs) | a 7z with any of its bytes changed never panics, and fails only as a damaged or refused archive |
| `project::lock::canonical::tests::fuzz_lock_reader` | [project/lock/canonical.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/lock/canonical.rs) | the fast lock reader agrees with validated lock parsing |
| `project::manifest::tests::fuzz_manifest` | [project/manifest.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/project/manifest.rs) | accepted manifests keep their invariants through dataset edits |
| `source::drive::folder::tests::fuzz_drive_pages` | [source/drive/folder.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/drive/folder.rs) | folder and download-page parsing remains safe for arbitrary text |
| `net::headers::tests::fuzz_response_headers` | [net/headers.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/headers.rs) | response headers and suggested filenames remain valid when accepted |
| `source::bucket::xml::tests::fuzz_object_listings` | [source/bucket/xml.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/bucket/xml.rs) | the S3 XML reader never panics, and accepted elements have valid names |

all targets must avoid panics. where a writer exists, round trips must preserve the accepted value. the test body remains beside the parser so a minimized failing input can become a nearby regression case.

## ordinary test mode

`cargo test` runs each target for one second of random input. run only those tests with:

```sh
cargo test --locked --lib fuzz_
```

this short pass is useful for local and CI checks; it is not a substitute for sustained exploration of a changed parser.

## longer linux runs

install a nightly toolchain and cargo-bolero when preparing a longer run:

```sh
rustup toolchain install nightly
cargo install --locked cargo-bolero --no-default-features --features libfuzzer
mkdir -p .fuzz/manifest .fuzz/manifest-crashes
cargo bolero test --toolchain nightly -T 10min --corpus-dir .fuzz/manifest --crashes-dir .fuzz/manifest-crashes project::manifest::tests::fuzz_manifest
```

[Cargo.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/Cargo.toml) defines the `fuzz` profile: optimized builds with debug assertions and overflow checks retained. explicit corpus and crash folders keep the run's files together. without those arguments bolero keeps its corpus in `__fuzz__` beside the source, which git ignores. the explicit `.fuzz` folder is local run data; do not stage it.

when a crash appears, preserve the input, minimize it, and add a deterministic regression test that fails before fixing the parser. run that test and the parser's module after the fix, then repeat the fuzz target. keep only inputs needed to explain and reproduce the behavior.

related: [building and testing](building-and-testing.md), [sources](../architecture/sources.md), [archives](../architecture/archives.md), [project](../architecture/project.md).
