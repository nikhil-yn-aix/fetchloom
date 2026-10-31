# output

`Output` keeps human progress on stderr and machine-readable answers on stdout.

## ownership

command handlers provide facts and completion verbs; `Output` owns formatting, prompts and the final JSON envelope. `Work` counts operations independently of rendering, so tests can detect repeated work without timing the terminal.

## files

| file | what it holds | types and functions |
|---|---|---|
| [output.rs](../../../src/output.rs) | human lines, prompts, errors and JSON | `Output`, `Level`, `Document`, `Problem`, `step`, `done`, `warn`, `answer`, `document`, `verb_line` |
| [output/progress.rs](../../../src/output/progress.rs) | byte bars, waiting and item progress | `Bar`, `Wait`, `Ticker`, `start`, `advance`, `sized` |
| [output/style.rs](../../../src/output/style.rs) | terminal and color decisions | `Style`, `ColorChoice`, `detect`, `stdout_color` |
| [output/picker.rs](../../../src/output/picker.rs) | dataset-choice drawing and key handling | `choose`, `selection_key`, `row` |
| [work.rs](../../../src/work.rs) | atomic work counters and snapshots | `Work`, `Counts`, `add`, `counts` |
| [error.rs](../../../src/error.rs) | stable kinds and structured explanations | `Error`, `Kind`, `FileFailure`, `exit_code`, `with_result` |
| [cli/logs.rs](../../../src/cli/logs.rs) | debug output and timeline recording | `Logs`, `start`, `on_stderr` |
| [units.rs](../../../src/units.rs) | sizes, rates, durations and clipping | `size`, `rate`, `speed`, `duration`, `count`, `clip` |

## how it works

### lines and bars

the palette gives each color one meaning:

| meaning | rendering |
|---|---|
| finished work | green |
| work in progress and anything the user types | cyan |
| attention or a difference | yellow |
| failure | red |
| subject | bold |
| supporting facts | dim |

step and completion verbs are bold and right-aligned in 11 columns without status glyphs. the subject before ` ? ` is bold and its following facts are dim; a single-word subject is also bold. completed verbs include `Added`, `Fetched`, `Synced` and `Ok`. numbered search choices use cyan.

errors use bold red `error:`, indented dim details and optional bold cyan `help:`; warnings use bold yellow `warning:`. hints are dim, with backticked commands in bold cyan. backticks remain when color is off. welcome and help headings are bold, with commands and options cyan.

interactive progress shows filename, bytes, rate and estimated time left once the rate settles. narrow terminals drop counters, the bar and then the verb before shortening the filename in the middle. colored bars use `?` with a dim remainder; uncolored terminal bars use `#` and `-`. waits show elapsed time without a spinner. `MultiProgress` coordinates lines, bars and prompts so they do not overwrite each other.

non-terminal stderr has no bars. long batches report progress after two seconds, then at ten-second intervals from the start. a watched successful command can give a `next:` suggestion; quiet, JSON and redirected stdout omit it.

### terminal capabilities

stdout and stderr detect color separately. explicit `--color always` or `--color never` overrides environment settings. `auto` first requires the destination stream to be a terminal, then uses this order:

1. nonempty `NO_COLOR` disables color.
2. nonempty `CLICOLOR_FORCE` other than `0` enables it.
3. `CLICOLOR=0`, `TERM=dumb` or nonempty `CI` disables it.
4. otherwise, color is enabled.

only `--color always` forces ANSI into a pipe or file. labels retain their meaning without color. forcing color does not enable interactivity.

bars and questions need terminal stderr outside CI and outside `TERM=dumb`. questions also require terminal stdin. JSON, quiet mode and stderr debug logs suppress bars; quiet does not waive a question. the dataset picker follows [selection rules](../../user/commands/search.md#choosing-a-dataset), including refusal to let `--yes` choose.

`TERM=dumb` disables unicode choices. a windows terminal enables them only when `WT_SESSION`, `TERM_PROGRAM` or `ConEmuANSI` is present. other destinations keep unicode by default. fallbacks include `...` for clipping and `>` with worded key hints in the picker. this is not an ASCII-only output mode: text from filenames and sources remains intact.

### reports and counters

JSON is one completed document, not a stream of progress events. it holds schema, command, status, result, warnings, errors and work. an error can carry a partial result and per-file failures. warning collection remains independent of whether human warnings are visible.

`Counts` snapshots atomic counters for requests, downloaded bytes, retries, resumes, object writes, cache hits, reflinks, hardlinks and copies. [JSON output](../../user/reference/json-output.md) defines the fields and counting contract. elapsed time is reported, but tests use deterministic counts rather than wall-time assertions.

`-v` prints work summaries; `-vv` enables debug events. `FETCHLOOM_TRACE` writes a Chrome trace through a guard that must live until command work ends. trace output is separate from the JSON contract.

## invariants and traps

- do not print directly from command handlers. renderer calls preserve stream routing and progress synchronization.
- never depend on color alone to convey an error or completion.
- snapshot filters remove paths, ports, times and rates. inspect unfiltered output when changing a value those filters can hide.
- literal error kinds are stable script interfaces. prose can improve without renaming them.
- masked display URLs do not sanitize raw URL values stored elsewhere.
- there is no custom panic report; normal errors use `Error` and `Kind`.

## tests

| files | cargo filter |
|---|---|
| [output.rs](../../../src/output.rs), [progress.rs](../../../src/output/progress.rs), [style.rs](../../../src/output/style.rs) | `cargo test --locked --lib output::` |
| [logs.rs](../../../src/cli/logs.rs), [error.rs](../../../src/error.rs), [units.rs](../../../src/units.rs) | `cargo test --locked --lib cli::logs::`; `cargo test --locked --lib error::`; `cargo test --locked --lib units::` |
| [progress.rs](../../../tests/cli/progress.rs), [hints.rs](../../../tests/cli/hints.rs), [basics.rs](../../../tests/cli/basics.rs) | `cargo test --locked --test cli progress::`; `cargo test --locked --test cli hints::`; `cargo test --locked --test cli basics::` |
| [get.rs](../../../tests/cli/get.rs), [snapshots](../../../tests/cli/snapshots) | `cargo test --locked --test cli get::` |

## related

[CLI](cli.md), [style](../style.md), [profiling](../engineering/profiling.md), [JSON output](../../user/reference/json-output.md), [exit codes](../../user/reference/exit-codes.md).
