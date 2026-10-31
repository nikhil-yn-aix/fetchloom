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
| [work.rs](../../../src/work.rs) | atomic work counters and snapshots | `Work`, `Counts`, `add`, `counts` |
| [error.rs](../../../src/error.rs) | stable kinds and structured explanations | `Error`, `Kind`, `FileFailure`, `exit_code`, `with_result` |
| [cli/logs.rs](../../../src/cli/logs.rs) | debug output and timeline recording | `Logs`, `start`, `on_stderr` |
| [units.rs](../../../src/units.rs) | sizes, rates, durations and clipping | `size`, `rate`, `speed`, `duration`, `count`, `clip` |

## how it works

### lines and bars

step and completion verbs are right-aligned in 11 columns without status glyphs. completed work is bold green, ongoing work cyan, attention yellow and errors red. details after `·` are dim. errors use `error:`, indented details and optional `help:`; warnings use `warning:`.

progress keeps the filename when space is tight. colored bars use `━` with a dim remainder; uncolored terminal bars use `#` and `-`. `MultiProgress` coordinates lines, bars and prompts so they do not overwrite each other. pipes have no bars.

### decide style once

explicit `--color` takes precedence. automatic color first requires a terminal, then checks `NO_COLOR`, `CLICOLOR_FORCE`, `CLICOLOR`, `TERM` and `CI`. forcing color with the flag can color a pipe; `CLICOLOR_FORCE` alone does not bypass the automatic terminal check.

interactive bars and questions require terminal stderr outside CI and outside a dumb terminal. JSON, quiet mode and debug logging on stderr suppress bars. windows unicode support also considers whether the terminal can render it. stdout color is decided for stdout separately, since help and answers may be redirected independently.

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
