# versioning

fetchloom has one package version and separate version fields for JSON output, project locks and local state.

## package version

`Cargo.toml` owns version `0.1.0`. the executable reports it, the python package reads it dynamically, and the [release workflow](releasing.md#workflow) rejects a tag that disagrees with it.

before 1.0, use a minor-version change for a breaking public-interface change and a patch for compatible fixes or additions. use semantic versioning from 1.0. explain breaking changes and required user action in release notes.

## public interfaces

commands, options, environment settings, exit codes, error kinds, JSON fields, manifest keys and python helper functions are public interfaces. do not repurpose an existing name or error kind. document additions with their owning component and check both human and machine-readable output.

| format | current contract |
|---|---|
| [JSON output](../../user/reference/json-output.md) | `schema: 1`; callers check it before interpreting results. changing a field's meaning or type, or removing it, needs a schema change |
| [data.toml](../../user/reference/data-toml.md) | UTF-8 TOML without a version field; unknown keys fail |
| [data.lock](../../user/reference/data-lock.md) | `version = 1`; the reader accepts this version only. a higher value, a key or a value it does not know, and a list column it does not know ask for a newer executable and are never called damaged; other unsupported or broken locks require explicit recovery |
| [exit codes](../../user/reference/exit-codes.md) | stable kind-to-exit mappings; scripts use kinds rather than prose |
| [python helper](../../user/reference/python-helper.md) | typed `path()`, `sync()` and `get()` functions around the executable's JSON interface |

## how data.lock grows

a clone outlives the fetchloom that wrote it, so a teammate on an older release meets every format a newer one writes. data.lock is read strictly, so these rules keep that meeting an update rather than a lost pin:

- any new data.lock field, any new value of a field and any new column of a data.lock.d list bumps `version`, so an older fetchloom meets the higher version before anything it does not know.
- a reader refuses a higher `version`, and a field, value or column it does not know, with words that say a newer fetchloom wrote it and to update. it never calls such a lock damaged, and never suggests deleting it and locking again, which resolves every dataset anew and may pin newer data. `src/project/lock.rs` (`VERSION`, `other_version`, `newer`) and `src/project/lock/lists.rs` (`newer_columns`) hold these words.

## internal formats

| part | current boundary |
|---|---|
| [cache folders and records](../../user/reference/cache-folder.md) | internal indexes, not a public interchange format. URL and partial records require their declared fields; an unusable hint loses reuse, not the project's pins |
| [.fetchloom/state](../../user/reference/project-folder.md#placement-state) | `version: 2`; missing, unreadable, malformed or unsupported state counts as empty and existing files must be checked before adoption |
| human-readable output | wording, colors and alignment may change; scripts read JSON |
| the rust crate's API | fetchloom is a command, not a supported library API |

## minimum rust

`rust-version` in Cargo.toml declares the source-build minimum. `rust-toolchain.toml` pins the toolchain CI and releases use. raising the minimum affects source installs, including a python package built from source, but not a prebuilt executable or binary wheel.

## sources and dependencies

fetchloom depends on web APIs it does not control. the [live checks](ci.md#live-source-checks) exercise the configured public services each monday; they do not cover every adapter. a removed source can still leave usable cached bytes when the manifest and lock remain unchanged.

dependabot proposes github actions and crate updates monthly, once a release is a week old. every update goes through the full test gate and `cargo deny` under the [dependency policy](ci.md#quality).
