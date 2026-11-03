# versioning

what a release may change, and what it promises to keep. [compatibility](../../user/reference/compatibility.md) lists the systems and sources each release supports.

## version numbers

before 1.0, the minor number marks a breaking change and the patch number marks fixes and additions that break nothing, as uv and most `0.x` rust tools do: `0.2.0` may break what `0.1.5` did, `0.1.6` may not. from 1.0, plain semver.

every breaking change is listed first in the release notes, with what to change.

## public, kept stable

| interface | the rule |
|---|---|
| commands, options and their meaning | a removed or renamed option keeps working, hidden, for one minor release and warns once with its replacement |
| exit codes and error `kind` names | never reused for another meaning; a new kind may be added |
| `--json` documents, `schema: 1` | fields are only added. removing or renaming one, or changing its type, raises `schema` in a minor release |
| `data.toml` | new optional keys may be added. a key is never repurposed |
| `data.lock`, `version = 1` | a format change raises `version` in a minor release. an older fetchloom refuses a newer lock and names the fix, installing a newer fetchloom; a newer one reads every older format |
| environment variables | as options |
| the python helper's functions | as options |

## internal, free to change

| part | why it may change, and what keeps it safe |
|---|---|
| the cache's folders and records | objects are named by SHA-256. reuse trusts an object whose size and time are unchanged and hashes a doubtful one again; `cache verify` hashes them all. a change must read the old layout or move to new folder names; deleting the cache only costs downloads |
| `.fetchloom/state` | a state of another version is dropped and rebuilt by checking the files |
| human-readable output | wording, colors and alignment improve between releases. scripts read `--json` |
| the rust crate's API | fetchloom is a command, not a library |

## minimum rust

`rust-version` in Cargo.toml follows the toolchain in `rust-toolchain.toml`, which CI and releases build with. raising it is not breaking for people who install a release binary or the python package; it only affects `cargo install` from source.

## sources that change

fetchloom depends on web APIs it does not control. the [live tests](ci.md#live-source-checks) run every monday against every source. a source that changes its API gets a patch release; a source that shuts down is marked unsupported in the release notes, and its entries in existing locks still sync from the cache.

## dependencies

dependabot updates the github actions monthly. crates are updated by hand, each update through the full test gate and `cargo deny`, which also fails CI on a new security advisory.
