# output

`Output` keeps human progress on stderr and machine-readable answers on stdout.

## ownership

command handlers provide facts and completion verbs; `Output` owns formatting, prompts and the final JSON envelope. `Work` counts operations independently of rendering, so tests can detect repeated work without timing the terminal.

## files

| file | what it holds | types and functions |
|---|---|---|
| [output.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output.rs) | human lines, prompts, errors and JSON | `Output`, `Level`, `Document`, `Problem`, `step`, `done`, `warn`, `answer`, `document`, `verb_line` |
| [output/progress.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/progress.rs) | byte bars, waiting and item progress | `Bar`, `Wait`, `Ticker`, `start`, `advance`, `sized` |
| [output/style.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/style.rs) | terminal and color decisions | `Style`, `ColorChoice`, `detect`, `stdout_color` |
| [output/picker.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/picker.rs) | dataset-choice drawing and key handling | `choose`, `selection_key`, `row` |
| [work.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/work.rs) | atomic work counters and snapshots | `Work`, `Counts`, `add`, `counts` |
| [error.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/error.rs) | stable kinds and structured explanations | `Error`, `Kind`, `FileFailure`, `exit_code`, `with_result` |
| [cli/logs.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/logs.rs) | debug output and timeline recording | `Logs`, `start`, `on_stderr` |
| [units.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/units.rs) | sizes, rates, durations and clipping | `size`, `rate`, `speed`, `duration`, `count`, `clip` |

## how it works

### lines and bars

each color has one meaning, named by a `Tone` in [style.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/style.rs):

| tone | meaning | ansi theme | dark theme | light theme |
|---|---|---|---|---|
| `done` | finished work | green | `#8fc086` | `#3c7a37` |
| `work` | work in progress and its bars | cyan | `#ee7a4c` | `#b44f26` |
| `warn` | attention or a difference | yellow | `#e8b65a` | `#8f6200` |
| `fail` | failure | red | `#f25c6a` | `#bf213a` |
| `accent` | anything the user types, and choice numbers | cyan | `#ee7a4c` | `#b44f26` |

`Theme::detect` picks the theme: full-color terminals (`COLORTERM=truecolor` or `24bit`, `WT_SESSION`, or `TERM_PROGRAM` of vscode, iTerm.app, WezTerm or ghostty) get dark, or light when `COLORFGBG` ends in 7 or 15; every other terminal gets ansi, its own sixteen colors. `theme=` in `FETCHLOOM_COLORS` overrides the choice.

subjects are bold and supporting facts are dim. `FETCHLOOM_COLORS` replaces any tone's color for the run. it is read once, when `Output` starts; an entry it cannot read keeps the default and becomes one warning. color code asks for a tone through `toned`, `tinted` or `accent`, never a fixed color.

the welcome outside a project prints the wordmark in the accent when stdout is a terminal wider than 98 columns.

step and completion verbs are bold and right-aligned in 11 columns without status glyphs. the subject before ` · ` is bold and its following facts are dim; a single-word subject is also bold. completed verbs include `Added`, `Fetched`, `Synced` and `Ok`. numbered search choices use the accent.

errors use bold red `error:`, indented dim details and optional bold cyan `help:`; warnings use bold yellow `warning:`. hints are dim, with backticked commands in bold cyan. backticks remain when color is off. welcome and help headings are bold, with commands and options cyan.

interactive progress shows filename, bytes, rate and estimated time left once the rate settles. narrow terminals drop counters, the bar and then the verb before shortening the filename in the middle; a fraction that shares one unit, such as `1.0/4.0 MiB`, shortens to the bytes done with that unit, `1.0 MiB`. colored bars use `━` with a dim remainder; uncolored terminal bars use `#` and `-`. waits show elapsed time without a spinner. `MultiProgress` coordinates lines and bars so they do not overwrite each other. a question clears the bars and hides them behind a hidden draw target instead of suspending them, since a suspension holds `MultiProgress`'s lock until the answer and every other download's progress update would wait on it; lines told meanwhile are held and written after the answer, and one question waits for another. prompts read one key at a time with `read_key_raw`, so ctrl-c cancels them on every system and a hidden answer never leaves echo off.

non-terminal stderr has no bars, and lists such as search rows stay on stdout. a command run below its project's folder prints a dim `using the project in ...` line for a person at a terminal. long batches report progress after two seconds, then at ten-second intervals from the start. a watched successful command can give a `next:` suggestion; quiet, JSON and redirected stdout omit it.

### terminal capabilities

stdout and stderr detect color separately. explicit `--color always` or `--color never` overrides environment settings. `auto` uses this order:

1. nonempty `NO_COLOR` disables color.
2. nonempty `CLICOLOR_FORCE` other than `0` enables it, even into a pipe or file, as the [CLICOLOR convention](https://bixense.com/clicolors/) asks.
3. a destination stream that is not a terminal gets no color.
4. `CLICOLOR=0`, `TERM=dumb`, nonempty `CI` or a coding agent's variable, below, disables it.
5. otherwise, color is enabled.

`--color always` and `CLICOLOR_FORCE` are the two ways to force ANSI into a pipe or file. labels retain their meaning without color. forcing color does not enable interactivity.

bars and questions need terminal stderr outside CI, outside `TERM=dumb` and outside a coding agent. `by_agent` in [output/style.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/style.rs) checks `AGENT_VARIABLES`, such as `AI_AGENT`, `VSCODE_AGENT` and `CLAUDECODE`; any value other than empty, `0` or `false` marks an agent, which also turns off automatic color. add a variable there only if agents set it and people's own terminals do not. questions also require terminal stdin. JSON, quiet mode and stderr debug logs suppress bars; quiet does not waive a question. the dataset picker follows [selection rules](../../user/commands/search.md#choosing-a-dataset), including refusal to let `--yes` choose.

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
- a panic is fetchloom's own bug: `main` installs a hook that says so and where to report it, then hands over to rust's default report. normal errors use `Error` and `Kind`.

## tests

| files | cargo filter |
|---|---|
| [output.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output.rs), [progress.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/progress.rs), [style.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/style.rs) | `cargo test --locked --lib output::` |
| [logs.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/logs.rs), [error.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/error.rs), [units.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/units.rs) | `cargo test --locked --lib cli::logs::`; `cargo test --locked --lib error::`; `cargo test --locked --lib units::` |
| [progress.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/progress.rs), [hints.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/hints.rs), [basics.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/basics.rs) | `cargo test --locked --test cli progress::`; `cargo test --locked --test cli hints::`; `cargo test --locked --test cli basics::` |
| [get.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/get.rs), [snapshots](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/cli/snapshots) | `cargo test --locked --test cli get::` |

## related

[CLI](cli.md), [style](../style.md), [profiling](../engineering/profiling.md), [JSON output](../../user/reference/json-output.md), [exit codes](../../user/reference/exit-codes.md).
