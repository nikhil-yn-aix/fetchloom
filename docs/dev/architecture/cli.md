# CLI

`cli` turns arguments and environment settings into commands with one output policy and one cancellation signal.

## ownership

command handlers order effects across modules. `Context` reads settings once and shares output, network limits, work counters and cancellation; it does not own project formats or HTTP parsing.

## files

| file | what it holds | types and functions |
|---|---|---|
| [main.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/main.rs), [lib.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/lib.rs) | executable and public library entry | `main` |
| [cli.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli.rs) | startup, settings, runtime and dispatch | `Context`, `run`, `jobs`, `splits`, `open_cache` |
| [args.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/args.rs) | clap declarations | `Args`, `Global`, `Command`, `Filter`, `read` |
| [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/project.rs) | project-changing commands and ready-path lookup | `Report`, `add`, `remove`, `sync`, `lock`, `path`, `sync_all` |
| [get.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/get.rs), [get/folder.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/get/folder.rs) | one-off retrieval of one link or several, link lists and destination checks | `Report`, `run`, `one`, `many`, `Outcome`, `check`, `put_all` |
| [search.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/search.rs) | catalog search, candidate rows and explicit selection | `Report`, `run`, `pick`, `find` |
| [picker.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/picker.rs) | terminal candidate selection and cancellation | `choose`, `selection_key`, `row` |
| [list.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/list.rs) | manifest and lock listing without placement, and its tables | `Report`, `Listed`, `run`, `listed`, `table`, `columns` |
| [status.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/status.rs) | local comparison and summaries | `Status`, `run`, `brief` |
| [cache.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/cache.rs) | cache command reports and retained-project discovery | `Info`, `Collected`, `info`, `gc`, `verify`, `needed_with` |
| [fetch.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/fetch.rs), [fetch/again.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/fetch/again.rs) | bounded transfer scheduling and expired-link recovery | `Job`, `all`, `apart`, `listed_job`, `Again`, `Listings`, `recover` |
| [help.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/help.rs), [usage.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/usage.rs) | help and argument errors | `show`, `answer` |
| [logs.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/logs.rs) | debug events and trace-file lifetime | `Logs`, `start` |

## how it works

### startup

`run` first turns off transparent huge pages for the process on linux, so the allocator's 2 MiB pages do not count a few touched bytes as two megabytes: on ext4 and xfs that cut peak memory by a third to two thirds in every lab scenario, at a few percent more cpu on the shortest runs. it then parses before creating the command context. output style and logging start before `-C`; an explicit trace path is created or truncated there, even for a dry run. version, help and completions return before `-C` and resource-setting validation. other commands change the working folder and read `Threads`; named commands also read job and rate limits. bare welcome and project status bypass the job and rate checks. the runtime uses up to four workers, subject to `Threads`. a fallible startup probe is joined before runtime construction, so an immediate thread-creation refusal follows the ordinary I/O error path.

built-in environment help describes the four-request cap per initial hostname; redirects keep that initial host’s slots.

network clients are created only where commands need them. `Net` delays TLS roots and proxy setup until the first request. version and help paths avoid that work.

help routing reads short-option bundles from clap metadata. `-qh` still requests help, and attached or following values of `-C`, including non-UTF-8 paths, do not become command names. only option spellings are read lossily; command names still need valid UTF-8. help and JSON flags are found before `--`, including after malformed arguments.

### command boundaries

| command path | ordering that matters |
|---|---|
| `add` | resolve the new reference, learn a server-provided filename when needed, edit the manifest, then sync; attempt to restore both manifest and lock, including their prior absence, if sync fails |
| `remove` | write the reduced manifest and lock, then remove unchanged unwanted files; never fetch replacements |
| `sync` | hold the project, read inputs, reject a stale lock under `--locked`, then enter `sync_all` |
| `lock` | resolve changed entries and learn pins; already pinned files need not fill the cache |
| `lock --check` | compare specs without network access or mutation locking |
| `path` | read the lock and build its plan once per readiness check, sharing that plan with member validation and the answer; open the cache at most once; waiting readers recheck changed state before taking the mutation hold, rereading and syncing if still needed |
| `get` | share download, archive and placement machinery without writing project metadata; a lone link prechecks its known folder, while batches check final names and collision suffixes after downloading |
| `status` | inspect local state without taking the mutation lock |
| `list` | read manifest and lock on a cancellable blocking task, without scanning placements or taking the mutation lock |
| `search` | query public catalogs concurrently; return canonical references without downloading datasets |

`add` settles what needs no project hold first: whether to start a project in the home folder or at a disk's root (`start_here`), and which dataset search words name, so a question answered no or a cancelled list leaves nothing behind. it then takes its mutation lock, kept in `AddProject` through reading, validation, resolution, sync and rendering. `sync_add` relocks and fills the cache first through `Loop::prepare`, then refuses with `project.changed` when `AddProject::changed` finds either file no longer as read, so an edit made during the downloads stays. it then saves recovery copies, records each file's digest through `BeforeAdd::writing` just before writing it, and writes `data.toml`, `data.lock` and the placement through `Loop::apply`. a failure or a stop from the first write on runs `restore`, which puts back only a file still holding what the add wrote and leaves one changed since, with its recovery copy; a stop before it leaves both files untouched. `--dry-run` stops before `sync_add` in `preview_add`. rendering errors do not roll back an already completed sync. a first add that ends without adding anything removes the `.fetchloom` and empty data folder it made through `Project::unstart`. a command that stops returns its own `interrupted` error, which `block_on` passes on, so its details reach the person and `--json`. otherwise `block_on` ends a run that saw a stop as interrupted, unless the command called `Stop::finish` at the point past which it completes its work: `add` and `sync` once `Placing::apply` has looked at the stop for the last time, `lock` and `remove` after their last stop check before writing. a ctrl-c while such a command prints its last lines then leaves it finished.

`get -i` reads its input on a blocking task while the command awaits either the result or cancellation. shutdown does not wait for an interrupted input reader whose producer keeps the pipe open.

plain-word `add` and `get` call `search::pick` before source resolution. an interactive choice returns a canonical reference; without one, the error carries candidate details and a JSON partial result. [catalog search](../../user/commands/search.md) owns user-facing selection rules.

`fetch::all` queues only the allowed number of downloads. it shares progress and counters across them. a locked 404 or 410 may re-resolve its recorded source once per batch; replacement URLs must still produce the old pinned bytes.

### adding a command

1. add the argument type and `Command` variant in [args.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/args.rs), including its `name()` match arm. put its copyable examples and on-disk effects in the `examples` constants and connect them with `after_help`.
2. declare the handler module and add dispatch in [cli.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli.rs). pass shared settings through `Context`, render through `Output`, and return a serializable report through `finish` so errors and JSON keep the same envelope.
3. check `fetchloom help <command>`, `fetchloom help --json` and unknown-topic suggestions. [help.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/help.rs) discovers command topics and the JSON command list from clap; update its prose only when the concepts change.
4. add argument tests and real-binary scenarios, declaring any new test module in [tests/cli/main.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/main.rs). snapshot help and human output and check JSON, exit codes, offline behavior, cancellation and disk effects where applicable.
5. extend the command list in [unattended.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/unattended.rs) so a pipe left open on stdin cannot make it wait for input. cover choices separately if the command is interactive.
6. add its page under [docs/user/commands](../../user/commands/README.md), link it from that index, the [user guide](../../user/README.md) and the book's [SUMMARY.md](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/SUMMARY.md), and update [llms.txt](https://github.com/nikhil-yn-aix/fetchloom/blob/main/llms.txt). document its result and failure payloads in the [JSON reference](../../user/reference/json-output.md).
7. run the owning tests, [review snapshots](../engineering/building-and-testing.md#update-snapshots), then run the full gate.

`cache verify` has no other name. `auth status` is a hidden name for `auth`, the form people know from other tools.

### adding an option

put parsing and conflicts in `args.rs`, then pass the value through `Context` or the command's argument type to its owner. keep presentation in `Output`. update the affected [command reference](../../user/commands/README.md) and [environment reference](../../user/reference/environment.md) when behavior changes.

## invariants and traps

- stdout carries answers or one JSON document. diagnostic text belongs on stderr through `Output`.
- a successful argument parse does not permit overwriting a hand edit. placement makes that decision later.
- do not initialize TLS or source credentials on help, version or local-only paths.
- a second ctrl-c clears the bars through `Output::abandon` and exits immediately; the first prints `Stopping` through `Output::stopping` and asks running work to stop and clean up. `interrupts` in cli.rs holds that order. prompts read keys raw, so at a question ctrl-c is a key that cancels it rather than a signal.
- a single failing source can stop dataset resolution. catalog search instead retains successful catalogs unless all fail. failed batches can carry partial results in their error report.

## tests

| files | cargo filter |
|---|---|
| [args.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/args.rs), [cli.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli.rs) | `cargo test --locked --lib cli::args::`; `cargo test --locked --lib cli::tests::` |
| [basics.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/basics.rs), [caps.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/caps.rs) | `cargo test --locked --test cli basics::`; `cargo test --locked --test cli caps::` |
| [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/project.rs), [get.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/get.rs) | `cargo test --locked --test cli project::`; `cargo test --locked --test cli get::` |
| [search.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/search.rs), [source search tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/search/tests.rs) | `cargo test --locked --test cli search::`; `cargo test --locked --lib source::search::` |
| [list.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/list.rs), [CLI project tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/project.rs) | `cargo test --locked --lib cli::list::`; `cargo test --locked --test cli project::list_shows_each_dataset_with_its_source_and_size -- --exact` |
| [picker.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/picker.rs) | `cargo test --locked --lib output::picker::` |
| [unattended.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/unattended.rs), [hints.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/hints.rs) | `cargo test --locked --test cli unattended::`; `cargo test --locked --test cli hints::` |

## related

[sync loop](sync-loop.md), [output](output.md), [network](network.md), [building and testing](../engineering/building-and-testing.md), [exit codes](../../user/reference/exit-codes.md).
