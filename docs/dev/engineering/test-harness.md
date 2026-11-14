# Test harness

## the cli harness

- `tests/cli/main.rs` declares the scenario modules, and `tests/cli/support.rs` owns the sandbox and the custom `TcpListener` server. the sandbox makes private work, cache and home folders and removes inherited fetchloom settings, source tokens, proxies and synced-folder settings; normal OS inputs such as `PATH`, `CI` and the temporary-folder settings remain.
- children run with `LC_ALL=C.UTF-8`, `TERM=xterm` and `CLICOLOR=0`, so a contributor's locale or terminal cannot change a snapshot, and with the ticker and log settings the [debug-only hooks](#debug-only-hooks) list. a test of another setting sets it on its own `Command`, never in the test process's environment.
- a test server owns its listeners and connections, closing them and joining its workers on drop (`tests/cli/support/server.rs`). a worker panic fails its test, and cleanup during a test's panic keeps the original panic rather than aborting on a second.
- every command the harness starts goes through `Fetchloom` in `tests/cli/support/command.rs`, which `Sandbox::command` returns with `Command`'s methods. it reads stdout and stderr as they come and ends a command still running 120 s after it started, below the 180 s at which nextest ends a test and keeps nothing it said, failing the test with the command line, its stderr so far and the last 40 lines of its debug log. `Fetchloom::deadline` sets a shorter deadline, as the deadline's own test does.
- `Running` drains stdout and stderr while a test waits for a stderr event, then returns both byte streams when the child finishes. a child still running a minute after `finish` is asked, or at the deadline while a test waits for it, is killed, and the test fails with what it said and the end of its debug log.
- race tests wait without polling where something announces the event: `Server::wait_for_requests` wakes at each request and `Running::wait_for` at each stderr line. `wait_until` polls only state nothing announces, such as the partial files another process writes. a race the code under test would win on a fast disk gets one of the debug-only holds; a hold's deadline only keeps a broken test from hanging, and fixed pauses belong only to timed response behavior.
- a test that needs nothing to answer at an address, a unit test too, takes it from `Refusing` in `tests/support/ports.rs`. its port is the local end of a connection it keeps open: nothing listens there, so each connection is refused, and no other process can take the port, as one could once a dropped listener freed it. a socket bound without listening holds a port too, but macos drops connections to it unanswered, so a test would wait out its timeout.
- on linux, terminal scenarios use `script` with `/bin/sh`, ignore shell startup-file overrides, and report a nonzero command exit as an error.
- default tests use local servers or recorded answers; ignored tests cover live repositories.

a test that needs what a machine may lack returns early and says `skipped: <reason>` on stderr, naming one of these capabilities:

| capability | what the test needs |
|---|---|
| `script` | the `script` program, which gives the terminal scenarios a terminal |
| `git` | git on `PATH`, for merges and git's reads in `project/git.rs` |
| `hardlinks` | a disk that makes hardlinks |
| `placed-hardlinks` | a disk where fetchloom places a read-only file as a hardlink rather than a reflink |
| `aliases` | a disk that takes another case or unicode form of a name for the same file |
| `distinct-names` | a disk that keeps `Data` and `data` apart |
| `raw-names` | a disk that keeps a name that is not text as written (apfs refuses one and hfs+ escapes it) |
| `unprivileged` | a run that is not root, since root writes into a read-only folder |
| `symlinks` | a disk and a run that make symbolic links, and junctions for folders on windows |
| `birth-time` | a disk that records when each file was made |
| `freed-names` | a disk that frees a removed file's name at once, where fat and exfat on windows keep it while the file is open |
| `sparse-files` | a disk that holds a sparse file of 64 GiB |
| `file-locks` | a disk that takes file locks |
| `bash`, `sh`, `zsh`, `fish`, `pwsh` | each shell the quoting test pastes a command into; the completion tests also need `bash` and `pwsh` |

`FETCHLOOM_TEST_REQUIRE` lists the ones the tests must find, as `script,git,hardlinks`; the shared helper in `tests/support/skips.rs` reads it once, and a test that lacks a listed capability fails instead of skipping. CI's native runs set what each runner has:

| runner | `FETCHLOOM_TEST_REQUIRE` |
|---|---|
| ubuntu-24.04 | `script,git,hardlinks,placed-hardlinks,raw-names,distinct-names,unprivileged,symlinks,birth-time,freed-names,sparse-files,file-locks,bash,pwsh,fish`; CI installs fish first |
| ubuntu-24.04-arm | `script,git,hardlinks,placed-hardlinks,raw-names,distinct-names,unprivileged,symlinks,birth-time,freed-names,sparse-files,file-locks,bash` |
| macos-15 | `script,git,hardlinks,unprivileged,pwsh,zsh` |
| windows-2025 | `git,hardlinks,pwsh` |
| windows-11-arm | `git,hardlinks` |

the FAT32, exFAT, ReFS, btrfs, xfs and HFS+ passes require none, and the casefold pass requires `aliases`.

the recipe tests in `tests/cli/build.rs` run `examples/recipe_step.rs`, a step program with the standard library alone whose first argument picks what it does: copy, write, fail, hold until a file appears, and the like. `cargo test` builds it beside the fetchloom binary, and a run of the CLI tests alone refreshes it once with the `cargo` that runs them; CI prebuilds and archives it, so each nextest process reuses it without invoking cargo; each test copies it into the sandbox's `bin` folder as `recipe-step` and puts that folder first on `PATH`, so a recipe names it as any program. every run appends its mode to `runs` in the folder the step runs in, so a test counts runs without sleeping.

snapshot filters normalize sandbox paths, ports, separators and durations and remove variable rates. on a drive that makes no links, `Sandbox::placed_filters` turns the copy wording of a story into the linking wording, so one snapshot covers every filesystem. inspect raw output when changing those fields. JSON work counters and output snapshots check different contracts.

## debug-only hooks

debug builds read these variables so a CLI test can put a local server where a host would be, fake a disk or a machine, or hold a run at a moment that passes too fast to catch. each read sits behind `#[cfg(debug_assertions)]`, so a release build never reads one. the sandbox sets the ticker and the log for every command; a test sets the others on its own `Command`. tests are the files under `tests/cli`.

| variable | what a debug build does with it | tests that set it |
|---|---|---|
| `FETCHLOOM_TEST_TICKER` | `off` drops the lines a long batch prints as time passes, which depend on how busy the machine is; any other value brings the first at 0.1 s rather than 2 s | every command, as `off`, which `tests_see_no_line_that_time_decides` checks by holding a reply back half a second; `progress.rs`, `cache.rs` and `get.rs` set `on` |
| `FETCHLOOM_TEST_LOG` | appends the debug log, an event a write, to the file it names | every command, as `logs/<n>.log` |
| `FETCHLOOM_TEST_SPLIT` | splits a slow download from 4 MiB rather than 64 MiB, in pieces from 512 KiB, after measuring for 0.2 s | `split.rs` |
| `FETCHLOOM_TEST_HOLD_PUSH` | holds each copy of a push once its part is in the mirror's `tmp/`, until a stop or the file it names | `concurrent.rs` |
| `FETCHLOOM_TEST_HOLD_FLUSH` | a download with every byte in writes `<file>.held`, then holds before its flush until the file appears, whatever is asked | `progress.rs` |
| `FETCHLOOM_TEST_HOLD_FINISH` | a run writes `<file>.held` right after `Stop::finish`, then holds until the file appears | `build.rs`, and `held_at_its_mark` in `support.rs` for `auth.rs`, `cache.rs`, `export.rs`, `get.rs` and `push.rs` |
| `FETCHLOOM_TEST_BUCKETS` | sends each request to an `amazonaws.com`, `storage.googleapis.com` or `blob.core.windows.net` host to `/<host><path>` on the local server it names | `buckets.rs`, `azure.rs`, `google.rs`, `archives.rs`, `drop.rs`, `layout.rs`, `project.rs`, `repositories.rs`, `papers.rs` |
| `FETCHLOOM_TEST_SOURCES` | the same for the repository hosts `test_source` in `src/net/api.rs` lists | `repositories.rs`, `papers.rs` |
| `FETCHLOOM_TEST_DRIVE` | the same for google drive's pages, its download host and its api | `google/drive.rs` |
| `FETCHLOOM_TEST_HF_API`, `FETCHLOOM_TEST_KAGGLE_API`, `FETCHLOOM_TEST_ZENODO_API`, `FETCHLOOM_TEST_ENA_API`, `FETCHLOOM_TEST_GEO_API`, `FETCHLOOM_TEST_DANDI_API`, `FETCHLOOM_TEST_OPENNEURO_API`, `FETCHLOOM_TEST_ARXIV_API` | sends each API request to that site's host, searches and listings alike, to the local server it names, with the same path and query | `search.rs`, `add.rs` and `mirrors.rs`; hugging face also `auth.rs`, zenodo also `get.rs` and `secrets.rs`, arXiv `papers.rs` |
| `FETCHLOOM_TEST_GOOGLE_TOKEN` | stands a local server in for google's token endpoint | `google.rs`, `google/drive.rs` |
| `FETCHLOOM_TEST_AZURE_IDENTITY` | stands a local server in for the instance metadata service a managed identity asks | `azure.rs` |
| `FETCHLOOM_TEST_FILESYSTEM` | the format every disk reports, by the name `Filesystem::named` reads, such as `vfat` | `project.rs`, `get.rs`, `buckets.rs` |
| `FETCHLOOM_TEST_FREE` | the bytes free on every disk | `project.rs`, `import.rs`, `build.rs`, `get.rs` |
| `FETCHLOOM_TEST_ROOM` | the memory the machine has, as a container's limit leaves it | `caps.rs` |
| `FETCHLOOM_TEST_INLINE` | the most files a source lists in data.lock itself, rather than 50,000, so a test writes lists to `data.lock.d` with few files | `project.rs`, `import.rs`, `merge.rs`, `buckets.rs` |

the debug binary never reads these, only test processes do:

| variable | what it does |
|---|---|
| `FETCHLOOM_TEST_REQUIRE` | the capabilities a test must find, as [above](building-and-testing.md#test-layers) |
| `FETCHLOOM_TEST_NOOP_CHMOD` | the fat mount the chmod test places on, as [the xfs loop mount](other-platforms.md#xfs-loop-mount) says |
| `FETCHLOOM_TEST_WRITE_MANUAL` | writes the manual page instead of comparing it, as [update snapshots](building-and-testing.md#update-snapshots) says |
| `FETCHLOOM_TEST_LIVE_DRIVE_FOLDER` | the private drive folder a live test reads, as [python and live tests](building-and-testing.md#python-and-live-tests) says |
| `FETCHLOOM_TEST_CLI_STOPPED_LIST`, `FETCHLOOM_TEST_SCAN_DESCRIPTOR_LIMIT`, `FETCHLOOM_TEST_LOG_TIMELINE`, `FETCHLOOM_TEST_LOG_MODE`, `FETCHLOOM_TEST_COPY_CHILD`, `FETCHLOOM_TEST_FEW_OPEN_FILES`, `FETCHLOOM_TEST_PIPE_CHILD` | marks a child: the test starts its own test binary again with the variable set, so the body runs alone in a process the test can limit, stop or kill |

## test directories

if you override `TMPDIR` on linux or macos, or `TMP` and `TEMP` on windows, choose an existing writable folder outside every git repository and fetchloom project. `Sandbox::new` uses `tempfile::tempdir`, and commands still inspect parent folders for project discovery and git integration. a test folder inside a checkout can therefore produce extra `.gitignore` files or different snapshots. do not accept those differences as intended output.
