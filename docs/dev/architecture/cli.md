# CLI

`cli` turns arguments and environment settings into commands with one output policy and one cancellation signal.

## ownership

command handlers order effects across modules. `Context` reads settings once and shares output, network limits, work counters and cancellation; it does not own project formats or HTTP parsing.

## files

| file | what it holds | types and functions |
|---|---|---|
| [main.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/main.rs), [lib.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/lib.rs) | executable and public library entry | `main` |
| [cli.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli.rs) | startup, settings, runtime and dispatch | `Context`, `run`, `jobs`, `splits`, `open_cache`, `meant` |
| [args.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/args.rs) | clap declarations | `Args`, `Global`, `Command`, `Filter`, `read` |
| [add.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/add.rs) | adding a dataset: its name, its own sync and putting the project back when it fails | `add`, `AddProject`, `BeforeAdd`, `sync_add`, `restore`, `again`, `choose_name` |
| [remove.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/remove.rs) | removing a dataset from data.toml, data.lock and the data folder | `remove` |
| [drop.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/drop.rs) | leaving files out of their dataset: the pattern for each, written to data.toml, the pin rewritten offline, and the unchanged files deleted | `run`, `Report`, `in_data_folder`, `asked`, `edits`, `refuse_what_breaks`, `written` |
| [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/project.rs) | `sync`, `lock` and the steps the project commands share | `Report`, `Outcome`, `sync`, `lock`, `sync_all`, `preview_all`, `find`, `hold`, `known` |
| [path.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/path.rs) | ready-path lookup and the sync it runs when files are missing | `path`, `ready_path`, `planned_path`, `hold_path` |
| [get.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/get.rs), [get/folder.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/get/folder.rs), [get/file.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/get/file.rs) | one-off retrieval of one link or several, link lists, destination checks and the one file `-o` writes | `Report`, `run`, `one`, `many`, `Outcome`, `check`, `put_all`, `file::usage`, `file::one` |
| [search.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/search.rs) | catalog search, candidate rows and explicit selection | `Report`, `run`, `pick`, `find` |
| [picker.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/picker.rs) | terminal candidate selection and cancellation | `choose`, `selection_key`, `row` |
| [list.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/list.rs) | manifest and lock listing without placement, and its tables | `Report`, `Listed`, `run`, `listed`, `table`, `columns` |
| [export.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/export.rs) | the checksums of placed files and the links of downloaded ones, checked and written whole | `Report`, `run`, `inventory`, `sources`, `link`, `complete`, `render`, `publish` |
| [status.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/status.rs) | local comparison and summaries | `Status`, `run`, `brief` |
| [cache.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/cache.rs) | cache command reports and retained-project discovery | `Info`, `Collected`, `info`, `gc`, `verify`, `needed_with` |
| [fetch.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/fetch.rs), [fetch/again.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/fetch/again.rs) | bounded transfer scheduling and expired-link recovery | `Job`, `all`, `apart`, `listed_job`, `Again`, `Listings`, `recover` |
| [fetch/data_folder.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/fetch/data_folder.rs) | pinned files the data folder holds, copied into the cache when it lacks them, before any mirror is asked | `DataFolder`, `found`, `planned`, `holds`, `may_hold`, `take` |
| [help.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/help.rs), [usage.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/usage.rs) | help and argument errors | `show`, `answer` |
| [logs.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/logs.rs) | debug events and trace-file lifetime | `Logs`, `start` |

## how it works

### startup

`run` parses before creating the command context. output style and logging start before `-C`; an explicit trace path is created or truncated there, even for a dry run. version, help and completions return before `-C` and resource-setting validation. other commands change the working folder and read `Threads`; named commands first warn about each set variable one typo from one fetchloom reads, then read `FETCHLOOM_OFFLINE`, refusing a value that is not true or false before any request, then the settings file, `Machine` and the [settings](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/settings.rs) built from them, then the rate limit. `Machine::read` raises the open-file limit as far as the system allows. bare welcome and project status bypass the settings and rate checks. the runtime uses up to four workers, subject to `Threads`. a fallible startup probe is joined before runtime construction, so an immediate thread-creation refusal follows the ordinary I/O error path.

built-in environment help describes `FETCHLOOM_HOST_JOBS`, the request cap per initial hostname; redirects keep that initial host’s slots.

network clients are created only where commands need them. `Net` delays TLS roots and proxy setup until the first request. version and help paths avoid that work.

help routing reads short-option bundles from clap metadata. `-qh` still requests help, and attached or following values of `-C`, including non-UTF-8 paths, do not become command names. only option spellings are read lossily; command names still need valid UTF-8. help and JSON flags are found before `--`, including after malformed arguments.

### command boundaries

| command path | ordering that matters |
|---|---|
| `add` | resolve the new reference, learn a server-provided filename when needed, edit the manifest, then sync; attempt to restore both manifest and lock, including their prior absence, if sync fails |
| `remove` | write the reduced manifest and lock, then remove unchanged unwanted files; never fetch replacements |
| `drop` | plan from the cache's records of each archive with `plan::inputs`, which lays out a dataset with `place = false` where its files would land, map each path to its dataset and to its path inside its archive or in the listing, refuse what would leave a dataset no file, move a kept file or rewrite a stale pin, and for a path that names no planned file, say which pattern leaves it out already, if one does, then write data.toml, data.lock when a listed file leaves it, and remove unchanged unwanted files of those datasets alone; never ask a server. a listed file's pin is rewritten by `draft::without_files`, which equals relocking the same listing |
| `sync` | hold the project, read inputs, reject a stale lock under `--locked`, then enter `sync_all` |
| `lock` | resolve changed entries and learn pins; already pinned files need not fill the cache. when data.toml or data.lock holds git's conflict lines, [project/conflicts.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/project/conflicts.rs) merges them instead, under the project hold, and writes data.toml, the lists and data.lock without resolving anything |
| `lock --check` | compare specs without network access or mutation locking |
| `path` | read the lock and build its plan once per readiness check, sharing that plan with member validation and the answer; open the cache at most once; waiting readers recheck changed state before taking the mutation hold, rereading and syncing if still needed |
| `get` | share download, archive and placement machinery without writing project metadata; a lone link prechecks its known folder, while batches check final names and collision suffixes after downloading. `-o` refuses what cannot be one file before resolving, decides about a file already at the path by its hash, or, when no hash is known before the download, refuses one of another size than the source states and says the download comes first for any other, publishes a writable clone or copy through the placer without replacing, or with `--force` through `Volume::replace`, and drops a download it wrote unless a registered project needs it |
| `status` | inspect local state without taking the mutation lock |
| `list` | read manifest and lock on a cancellable blocking task, and the cache's records of what each archive unpacked to, which `plan::expand` turns into the files each dataset puts in its folder, without scanning placements or taking the mutation lock |
| `export` | read manifest, lock and the cache's unpack records as `status` plans them, without a hold, a request or a new cache; check the inventory is complete for the text form before writing a byte, and publish `-o` under a hidden name without replacing, or with `--force` after moving the old file aside |
| `search` | query public catalogs concurrently; return canonical references without downloading datasets |

`add` settles what needs no project hold first: whether to start a project in the home folder or at a disk's root (`start_here`), and which dataset search words name, so a question answered no or a cancelled list leaves nothing behind. it then takes its mutation lock, kept in `AddProject` through reading, validation, resolution, sync and rendering. an `Output::lasting` line, `Pinning <name>`, tells the work from relocking until data.lock is written; it starts once every byte is in, or at once for an import, whose bytes are in the cache already, since downloads, unpacking and placing tell lines of their own. `sync_add` relocks and fills the cache first through `Loop::prepare`, then refuses with `project.changed` when `AddProject::changed` finds either file no longer as read, so an edit made during the downloads stays. it then saves recovery copies and, since flushing them takes a while, checks `AddProject::changed` again right before the first write. it records each file's digest through `BeforeAdd::writing` just before writing it, and writes `data.toml`, `data.lock` and the placement through `Loop::apply`; a `data.lock` that changed once `data.toml` was written fails with `project.changed` instead of being written, so `restore` puts `data.toml` back. a failure or a stop from the first write on runs `restore`, which puts back only a file still holding what the add wrote and leaves one changed since, with its recovery copy; a stop before it leaves both files untouched. `--dry-run` stops before `sync_add` in `preview_add`. rendering errors do not roll back an already completed sync. a first add that ends without adding anything removes the `.fetchloom` and empty data folder it made through `Project::unstart`. whether it is a first add is decided again from what it reads once it holds the project: one that waited while another add started the project in the same folder says no `Started` line and takes nothing back. a command that stops returns its own `interrupted` error, which `block_on` passes on, so its details reach the person and `--json`. otherwise `block_on` ends a run that saw a stop as interrupted, unless the command called `Stop::finish` at the point past which it completes its work: `add` and `sync` once `Placing::apply` has looked at the stop for the last time, a `sync` that finds its project settled once `place::untouched` says nothing is left to do, `lock` and `remove` after their last stop check before writing, `build`, `build --prepare` and `build --accept` right before they write data.lock, `get` once its last file is placed or before `-o` writes, `export -o` before its file, and `auth login` and `auth logout` before they save or remove a key. `cache gc` and `cache verify` run without `block_on` and call it once they swept or hashed, since a ctrl-c after that would otherwise print `Stopping` and still end with exit 0. a ctrl-c while such a command prints its last lines then leaves it finished, and says so with `Finishing what it started`; a closing `done` line told before that ctrl-c is told again at the end, since a terminal drops output it had not yet shown when ctrl-c comes. a stop asked between the command's last look and its mark is told the same way, late, through `Stop::on_late`, and the mark takes it back, so no work after the mark stops for it.

an `add` whose hugging face or kaggle id gets an answer that the source shows no dataset of that name, signed in or not, which the source marks with `Error::unseen`, asks `search::near` once for a public dataset of the same owner one typo away, never for `--json` or offline. the request gets one try through `Api::get_once`, with no retry and no wait for the host's rate limit, so a busy source gives no suggestion at once; `near_miss` then gives the person's command line with the name fixed, through `meant`, and drops the need, so no sign-in is offered.

several links typed for one `add` go the way of an `add -i` list, `add_list_held`: `links::typed` checks them as `links::checked` checks a list's lines, the list has no name, so messages name each link by itself, and each link's origin is trusted as a single typed link's is.

`get -i` reads its input on a blocking task while the command awaits either the result or cancellation. shutdown does not wait for an interrupted input reader whose producer keeps the pipe open.

plain-word `add` and `get` call `search::pick` before source resolution. an interactive choice returns a canonical reference; without one, the error carries candidate details and a JSON partial result. [catalog search](../../user/commands/search.md) owns user-facing selection rules.

`fetch::all` queues only the allowed number of downloads. it shares progress and counters across them. a locked 404 or 410 may re-resolve its recorded source once per batch; replacement URLs must still produce the old pinned bytes.

### adding a command

1. add the argument type and `Command` variant in [args.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/args.rs), including its `name()` match arm. put its copyable examples and on-disk effects in the `examples` constants and connect them with `after_help`; `help::examples_first` moves the examples up below the usage lines when the help prints.
2. declare the handler module and add dispatch in [cli.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli.rs). pass shared settings through `Context`, render through `Output`, and return a serializable report through `finish` so errors and JSON keep the same envelope.
3. check `fetchloom help <command>`, `fetchloom help --json` and unknown-topic suggestions. [help.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/help.rs) discovers command topics and the JSON command list from clap; update its prose only when the concepts change.
4. add argument tests and real-binary scenarios, declaring any new test module in [tests/cli/main.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/main.rs). snapshot help and human output and check JSON, exit codes, offline behavior, cancellation and disk effects where applicable.
5. extend the command list in [unattended.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/unattended.rs) so a pipe left open on stdin cannot make it wait for input. cover choices separately if the command is interactive.
6. add its page under [docs/user/commands](../../user/commands/README.md), link it from that index, the [user guide](../../user/README.md) and the book's [SUMMARY.md](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/SUMMARY.md), and update [llms.txt](https://github.com/nikhil-yn-aix/fetchloom/blob/main/llms.txt). document its result and failure payloads in the [JSON reference](../../user/reference/json-output.md).
7. run the owning tests, [review snapshots](../engineering/building-and-testing.md#update-snapshots), then run the full gate.

every command has one name. a name people bring from other tools, such as `install` or `auth status`, is a usage error whose help names the command meant; `habit` in [usage.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/usage.rs) holds them.

a guess at a mistyped command, option, dataset name, path or help topic goes through `meant` in [cli.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli.rs), which repeats the person's own command line with only the mistyped words changed, so a flag such as `--dry-run` stays in what they paste. a line holding a link with a query, a fragment or a login is never repeated, since any of them may carry a secret under a name `mask_query` does not know, and the help then names the command alone. `may_hold_secret` looks for such a link in a whole word, an option's value after `=`, a link attached to a short option and a link pasted wholly percent-encoded. `command_meant` in usage.rs picks a mistyped command with `name::closest`, ignoring case, rather than taking clap's suggestion, which gave `cache` for `seach`, and turns `rm`, `ls` and `version`, which other tools use and no spelling reaches, into `remove`, `list` and `--version`.

### adding an option

put parsing and conflicts in `args.rs`, then pass the value through `Context` or the command's argument type to its owner. keep presentation in `Output`. update the affected [command reference](../../user/commands/README.md) and [environment reference](../../user/reference/environment.md) when behavior changes.

## invariants and traps

- stdout carries answers or one JSON document. diagnostic text belongs on stderr through `Output`.
- a successful argument parse does not permit overwriting a hand edit. placement makes that decision later.
- do not initialize TLS or source credentials on help, version or local-only paths.
- the first ctrl-c goes through `heard_first`, which holds `Output::alone` while `first_ctrl_c` asks `Stop::hear` what it does and tells it, so every other line waits meanwhile. a run that called `Stop::finish`, or whose downloads all have every byte in while the disk keeps them, as `fetch` counts through `Stop::expect`, `arrived` and `ended`, finishes and prints `Finishing`; `push` calls it once every file has its name, before it flushes the mirror's folders, so a ctrl-c during a slow flush prints `Finishing` alone; for any other, `Stop::hear` asks the stop, under the watch's lock so a mark cannot come between the choice and the ask, and only then does `Alone::stopping` print `Stopping`. so once that line shows, the work sees the stop, which a test that waits for it relies on, and no line the stopping work prints comes first. a second ctrl-c clears the bars and prints `Quit at once` through `Output::quit`, runs what `Stop::undo_at_quit` registered, as a first add's `.fetchloom` before it writes, and exits 130 without waiting for the work. `interrupts` in cli.rs holds that order.
- `blocking` runs a command on a thread of its own. a stop returns at once from one that only reads, as `status`, `list` and `export` to stdout do, and the process ends without it. one that writes, `push` and `export -o`, is waited for until it has seen the stop and removed what it started, since ending first left a stopped push's unfinished copies in the mirror's `tmp/`. prompts read keys raw, so at a question ctrl-c is a key that cancels it rather than a signal.
- no or ctrl-c at a download question is a cancel, and `Error::unchanged` makes its line say that nothing changed once the caller knows so: `get` places and `add` writes nothing before every file is in, and `Loop::as_left` says it until the run writes data.lock, which `sync` does before it downloads what placing needs. a stop before `sync` places a file says the same through `Loop::before_placing`, or that the data folder is as it was once data.lock is written.
- a single failing source can stop dataset resolution. catalog search instead retains successful catalogs unless all fail. failed batches can carry partial results in their error report.

## tests

| files | cargo filter |
|---|---|
| [args.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/args.rs), [cli.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli.rs) | `cargo test --locked --lib cli::args::`; `cargo test --locked --lib cli::tests::` |
| [basics.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/basics.rs), [caps.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/caps.rs) | `cargo test --locked --test cli basics::`; `cargo test --locked --test cli caps::` |
| [add.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/add.rs), [remove.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/remove.rs), [path.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/path.rs) | `cargo test --locked --test cli add::`; `cargo test --locked --test cli remove::`; `cargo test --locked --test cli path::` |
| [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/project.rs), [get.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/get.rs) | `cargo test --locked --test cli project::`; `cargo test --locked --test cli get::` |
| [search.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/search.rs), [source search tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/search/tests.rs) | `cargo test --locked --test cli search::`; `cargo test --locked --lib source::search::` |
| [list.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/list.rs), [CLI project tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/project.rs) | `cargo test --locked --lib cli::list::`; `cargo test --locked --test cli project::list_shows_each_dataset_with_its_source_and_size -- --exact` |
| [export.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/export.rs), [CLI export tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/export.rs) | `cargo test --locked --lib cli::export::`; `cargo test --locked --test cli export::` |
| [picker.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/picker.rs) | `cargo test --locked --lib output::picker::` |
| [unattended.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/unattended.rs), [hints.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/hints.rs) | `cargo test --locked --test cli unattended::`; `cargo test --locked --test cli hints::` |

## related

[sync loop](sync-loop.md), [output](output.md), [network](network.md), [building and testing](../engineering/building-and-testing.md), [exit codes](../../user/reference/exit-codes.md).
