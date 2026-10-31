# CLI

`cli` turns arguments and environment settings into commands with one output policy and one cancellation signal.

## ownership

command handlers order effects across modules. `Context` reads settings once and shares output, network limits, work counters and cancellation; it does not own project formats or HTTP parsing.

## files

| file | what it holds | types and functions |
|---|---|---|
| [main.rs](../../../src/main.rs), [lib.rs](../../../src/lib.rs) | executable and public library entry | `main` |
| [cli.rs](../../../src/cli.rs) | startup, settings, runtime and dispatch | `Context`, `run`, `jobs`, `splits`, `open_cache` |
| [args.rs](../../../src/cli/args.rs) | clap declarations | `Args`, `Global`, `Command`, `Filter`, `read` |
| [project.rs](../../../src/cli/project.rs) | project-changing commands and ready-path lookup | `Report`, `add`, `remove`, `sync`, `lock`, `path`, `sync_all` |
| [get.rs](../../../src/cli/get.rs), [get/folder.rs](../../../src/cli/get/folder.rs) | one-off retrieval and destination checks | `Report`, `run`, `Outcome`, `check`, `put_all` |
| [search.rs](../../../src/cli/search.rs) | catalog search, candidate rows and explicit selection | `Report`, `run`, `pick`, `find` |
| [picker.rs](../../../src/output/picker.rs) | terminal candidate selection and cancellation | `choose`, `selection_key`, `row` |
| [list.rs](../../../src/cli/list.rs) | manifest and lock listing without placement | `Report`, `Listed`, `run`, `listed` |
| [status.rs](../../../src/cli/status.rs) | local comparison and summaries | `Status`, `run`, `brief` |
| [cache.rs](../../../src/cli/cache.rs) | cache command reports and retained-project discovery | `Info`, `info`, `gc`, `verify`, `needed` |
| [fetch.rs](../../../src/cli/fetch.rs), [fetch/again.rs](../../../src/cli/fetch/again.rs) | bounded transfer scheduling and expired-link recovery | `Job`, `all`, `listed_job`, `Again`, `Listings`, `recover` |
| [help.rs](../../../src/cli/help.rs), [usage.rs](../../../src/cli/usage.rs) | help and argument errors | `show`, `answer` |
| [logs.rs](../../../src/cli/logs.rs) | debug events and trace-file lifetime | `Logs`, `start` |

## how it works

### startup

`run` parses before creating the command context. `-C` changes the working folder before command work. output style is decided once; the context then applies offline mode, job and thread limits, bandwidth settings and split policy. the runtime uses up to four workers, subject to `Threads`.

network clients are created only where commands need them. `Net` delays TLS roots and proxy setup until the first request. version and help paths avoid that work.

### command boundaries

| command path | ordering that matters |
|---|---|
| `add` | resolve the new reference, learn a server-provided filename when needed, edit the manifest, then sync; attempt to restore both manifest and lock, including their prior absence, if sync fails |
| `remove` | write the reduced manifest and lock, then remove unchanged unwanted files; never fetch replacements |
| `sync` | hold the project, read inputs, reject a stale lock under `--locked`, then enter `sync_all` |
| `lock` | resolve changed entries and learn pins; already pinned files need not fill the cache |
| `lock --check` | compare specs without network access or mutation locking |
| `path` | answer directly when the dataset is ready; otherwise hold, reread and sync before answering |
| `get` | share download, archive and placement machinery without writing project metadata |
| `status` | inspect local state without taking the mutation lock |
| `list` | read manifest and lock without scanning placements or taking the mutation lock |
| `search` | query public catalogs concurrently; return canonical references without downloading datasets |

`add` keeps its mutation lock in `AddProject` through discovery, validation, resolution, manifest writing, sync and rendering. `discover_add`, `validate_add`, `resolve_add`, `write_add`, `sync_add` and `render_add` name those phases. `sync_add` invokes `restore` on a sync failure; rendering errors do not roll back an already completed sync.

plain-word `add` and `get` call `search::pick` before source resolution. an interactive choice returns a canonical reference; without one, the error carries candidate details and a JSON partial result. [catalog search](../../user/commands/search.md) owns user-facing selection rules.

`fetch::all` queues only the allowed number of downloads. it shares progress and counters across them. a locked 404 or 410 may re-resolve its recorded source once per batch; replacement URLs must still produce the old pinned bytes.

### adding a command

1. add the argument type and `Command` variant in [args.rs](../../../src/cli/args.rs), including its `name()` match arm. put its copyable examples and on-disk effects in the `examples` constants and connect them with `after_help`.
2. declare the handler module and add dispatch in [cli.rs](../../../src/cli.rs). pass shared settings through `Context`, render through `Output`, and return a serializable report through `finish` so errors and JSON keep the same envelope.
3. check `fetchloom help <command>` and unknown-topic suggestions. [help.rs](../../../src/cli/help.rs) discovers command topics from clap; update its prose only when the concepts change.
4. add argument tests and real-binary scenarios, declaring any new test module in [tests/cli/main.rs](../../../tests/cli/main.rs). snapshot help and human output and check JSON, exit codes, offline behavior, cancellation and disk effects where applicable.
5. extend the command list in [unattended.rs](../../../tests/cli/unattended.rs) so a pipe left open on stdin cannot make it wait for input. cover choices separately if the command is interactive.
6. add its page under [docs/user/commands](../../user/commands/README.md), link it from that index and the [user guide](../../user/README.md), and update [llms.txt](../../../llms.txt). document its result and failure payloads in the [JSON reference](../../user/reference/json-output.md).
7. run the owning tests, [review snapshots](../engineering/building-and-testing.md#update-snapshots), then run the full gate.

### adding an option

put parsing and conflicts in `args.rs`, then pass the value through `Context` or the command's argument type to its owner. keep presentation in `Output`. update the affected [command reference](../../user/commands/README.md) and [environment reference](../../user/reference/environment.md) when behavior changes.

## invariants and traps

- stdout carries answers or one JSON document. diagnostic text belongs on stderr through `Output`.
- a successful argument parse does not permit overwriting a hand edit. placement makes that decision later.
- do not initialize TLS or source credentials on help, version or local-only paths.
- a second ctrl-c exits immediately; the first asks running work to stop and clean up.
- a single failing source can stop dataset resolution. catalog search instead retains successful catalogs unless all fail. failed batches can carry partial results in their error report.

## tests

| files | cargo filter |
|---|---|
| [args.rs](../../../src/cli/args.rs), [cli.rs](../../../src/cli.rs) | `cargo test --locked --lib cli::args::`; `cargo test --locked --lib cli::tests::` |
| [basics.rs](../../../tests/cli/basics.rs), [caps.rs](../../../tests/cli/caps.rs) | `cargo test --locked --test cli basics::`; `cargo test --locked --test cli caps::` |
| [project.rs](../../../tests/cli/project.rs), [get.rs](../../../tests/cli/get.rs) | `cargo test --locked --test cli project::`; `cargo test --locked --test cli get::` |
| [search.rs](../../../tests/cli/search.rs), [source search tests](../../../src/source/search/tests.rs) | `cargo test --locked --test cli search::`; `cargo test --locked --lib source::search::` |
| [list.rs](../../../src/cli/list.rs), [CLI project tests](../../../tests/cli/project.rs) | `cargo test --locked --lib cli::list::`; `cargo test --locked --test cli project::list_shows_each_dataset_with_its_source_and_size -- --exact` |
| [picker.rs](../../../src/output/picker.rs) | `cargo test --locked --lib output::picker::` |
| [unattended.rs](../../../tests/cli/unattended.rs), [hints.rs](../../../tests/cli/hints.rs) | `cargo test --locked --test cli unattended::`; `cargo test --locked --test cli hints::` |

## related

[sync loop](sync-loop.md), [output](output.md), [network](network.md), [building and testing](../engineering/building-and-testing.md), [exit codes](../../user/reference/exit-codes.md).
