# output

`Output` keeps human progress on stderr and machine-readable answers on stdout.

## ownership

command handlers provide facts and completion verbs; `Output` owns formatting, prompts and the final JSON envelope. `Work` counts operations independently of rendering, so tests can detect repeated work without timing the terminal.

## files

| file | what it holds | types and functions |
|---|---|---|
| [output.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output.rs) | human lines, prompts, errors and JSON | `Output`, `Level`, `Document`, `Problem`, `step`, `done`, `warn`, `answer`, `report`, `document`, `verb_line` |
| [output/progress.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/progress.rs) | byte bars, waiting, steps before a command's work and item progress | `Bar`, `Wait`, `Setup`, `Ticker`, `start`, `advance`, `sized` |
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

step and completion verbs are bold and right-aligned in 11 columns without status glyphs; a dry run's verb puts `Would` in the column and its action after it, as `      Would remove`. the subject before ` · ` is bold and its following facts are dim; a single-word subject is also bold. completed verbs include `Added`, `Fetched`, `Synced` and `Ok`. numbered search choices use the accent.

errors use bold red `error:`, indented dim details and optional bold cyan `help:`. one error for several failed files or links, from `fetch::together` and `get`'s `some_failed`, tells failures alike once through `fetch::alike`: those of one kind with one help whose messages match once their own file or link and every address are taken out share the first one's line, the others listed under it, each named once, then their help. the first ten are named and the rest counted, while `failures` in `--json` keeps every one. a detail holds each SHA-256 whole, which `--json` keeps. `readable` shows a detail that compares two, as `expected sha256 <a>, got <b>`, on two lines with every digit, one value under the other, since a person checks them against a published checksum, and cuts any other word of 64 hex digits to 12, or to just past the first digit where it differs from another hash on the line, so hashes that differ late never read as equal; warnings use bold yellow `warning:`. a cancel the person chose and a stop they asked for are not errors: `Output::error` shows a cancel as a `Cancelled` line naming what was cancelled, and an `interrupted` error as a `Stopped` line made from its message, which starts with `stopped`, with its details dim under it. both use the warning tone and show their help as a hint; `--json` reports them as errors. hints are dim, with backticked commands in bold cyan. backticks remain when color is off. welcome and help headings are bold, with commands and options cyan.

interactive progress shows filename, bytes, rate and estimated time left once the rate settles. bytes an earlier run kept count toward the bytes and the bar, never toward the rate or the time left: indicatif counts only what moves in this run, from 0 at each start, and the bar adds the kept bytes back when it draws. the rate and time left are the bar's own, over this transfer's last 10 to 20 s of bytes and time, from a second after it starts, so the bytes that arrive at once as a transfer begins, such as those a server sent while the run checked what an earlier one kept, read as no speed: indicatif's estimate gave such a resume `47.1 MiB/s` from a server sending 38.1. the time left rounds up to whole seconds, as a wait for the next try does, so a transfer under way never reads `0 s left`, and one too long to count in a duration reads as the longest. a lone download that finds such bytes says `Checking <file> · <size> kept from an earlier run` until its server answers. the bar for a batch fills to the sum of its files' sizes only once every size is known, from the source, a server's answer or a download that ended, so it shows no time left while one is missing, and it says how many files are done, as in `Downloading 12 files · 3 done`, while others may still be under way. the rate settles after 3 s, and the time left shows while its estimate reaches no further than 60 times the time watched, until 30 s have passed. a download that receives nothing for a second drops its time left, which no longer holds, and after 2 s shows `no data for 12 s` instead of a fading rate, or `waiting for the disk · 3 s` when its writer has every slot full, as `Event::Behind` says, since the disk then holds it rather than the network; one waiting to try again after a failed try shows which try comes next, of how many, and when, as in `try 3 of 5 · next in 2 s`, or `waiting for 127.0.0.1's rate limit · try 6 of 15 · next in 3 s`, until a try gets an answer; while one with every byte in shows `waiting for the disk` as the file is flushed into the cache, and how long since its last byte once that passes 2 s. once the first ctrl-c asks for a stop, `Output::stopping` sets a flag every bar shares and redraws each bar at once, so each shows `stopping` after its counts from that moment, not from its next tick, and a stop that waits for a slow disk does not look like a network stall. a first ctrl-c that lets the run finish prints `Finishing` through `Output::finishing` or `finishing_downloads` instead. a line told at a ctrl-c clears its line first at a terminal, and every line there starts with a return to the start of its line, since the terminal echoes `^C` where its cursor waits, which shifted the verb column of the next line outside a bar. `Output::done` keeps the last closing line, and `Output::retell` tells it again at the end of a run that a finishing ctrl-c reached after it. a narrow terminal keeps facts before decoration: the bar gives way first, then the verb, then the filename shortens in the middle down to 16 columns, and only then do the counters drop from the end, the time left before the speed and the speed before the size; a bar counts how far it got in the unit of its total all the way, with two decimals below one of it, as `0.98/2.0 GiB` rather than `1002.0 MiB/2.0 GiB` before `1.0/2.0 GiB`, and a narrow terminal shortens that fraction to the bytes done with that unit, `0.98 GiB`. colored bars use `━` with a dim remainder; uncolored terminal bars use `#` and `-`. waits show elapsed time without a spinner. `MultiProgress` coordinates lines and bars so they do not overwrite each other. a question clears the bars and hides them behind a hidden draw target instead of suspending them, since a suspension holds `MultiProgress`'s lock until the answer and every other download's progress update would wait on it; lines told meanwhile are held and written after the answer, and one question waits for another. prompts read one key at a time with `read_key_raw`, so ctrl-c cancels them on every system and a hidden answer never leaves echo off. a yes or no question reads a line the same way: y, yes, n or no in any case, or enter alone for the default; any other line asks it again, so a q typed to quit never counts as the default.

non-terminal stderr has no bars, and lists such as search rows stay on stdout. `Output::report` writes one row of a report, such as a file a status finds missing, to stdout in the same verb column and the warning color `mark` uses, whatever its verb, colored as stdout's own color decision says, so a pipe or a file gets the rows while `mark`, `done` and `hint` keep the story on stderr; quiet and JSON print no rows. `status` prints each difference it finds this way, a `-v` row under its row as a row with no verb: every file into a pipe or a file, and at a terminal, where `Output::answer_width` is known, at most 20 rows of each kind, a folder that holds no file now as one row. a command run below its project's folder prints a dim `using the project in ...` line for a person at a terminal. the steps before a command's own work, taking the project lock, opening the cache and reading the data folder, go through `Output::setup`, which logs a `-vv` debug line as each begins and, at a terminal, shows a live line such as `Locking the project · 3 s` once one lasts a second, since a slow disk or a network filesystem can make them take seconds with nothing else on the screen; a wait for another fetchloom replaces the locking line with its own. work that is usually quick but grows with its size goes through `Output::lasting` the same way, with the plain lines a long batch tells where no bar shows: `Loop::apply` says `Placing 20,000 files` while `Placing::apply` checks and places them. long batches report progress after two seconds, then at ten-second intervals from the start. `--json` silences those lines unless `--progress` keeps them, for a script whose person still watches stderr, and `-q` silences them either way; `Output::ticker` makes that choice for every batch and wait. a download batch with every byte in reports `Waiting for the disk to finish writing <what>` instead of its byte count, and its closing `Downloaded` line takes its rate from `Fetched::transfer`'s time until the last byte arrived, before the flush, and says the whole time when the flush took a second or more, naming that part: `1.0 GiB in 30.2 s, 12.3 s of it waiting for the disk · 57.2 MiB/s`. an unpack batch that has read every byte of its archives reports `Finishing <what>`, as its bar says, while their files are written and put in the cache. a download batch starts at its first download, so a run that only waited for another fetchloom's download prints a `Waiting` line for that file and no download line. a watched successful command can give a `next:` suggestion; quiet, JSON and redirected stdout omit it.

### terminal capabilities

stdout and stderr detect color separately. explicit `--color always` or `--color never` overrides environment settings. `auto` uses this order:

1. nonempty `NO_COLOR` disables color.
2. nonempty `CLICOLOR_FORCE` other than `0` enables it, even into a pipe or file, as the [CLICOLOR convention](https://bixense.com/clicolors/) asks.
3. a destination stream that is not a terminal gets no color.
4. `CLICOLOR=0`, `TERM=dumb`, nonempty `CI` or a coding agent's variable, below, disables it.
5. otherwise, color is enabled.

`--color always` and `CLICOLOR_FORCE` are the two ways to force ANSI into a pipe or file. labels retain their meaning without color. forcing color does not enable interactivity.

bars and questions need terminal stderr outside CI, outside `TERM=dumb` and outside a coding agent. `by_agent` in [output/style.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/style.rs) checks `AGENT_VARIABLES`, such as `AI_AGENT`, `VSCODE_AGENT` and `CLAUDECODE`; any value other than empty, `0` or `false` marks an agent, which also turns off automatic color. add a variable there only if agents set it and people's own terminals do not. questions also require terminal stdin. JSON, quiet mode and stderr debug logs suppress bars; quiet does not waive a question. the dataset picker follows [selection rules](../../user/commands/search.md#choosing-a-dataset), including refusal to let `--yes` choose.

`TERM=dumb` disables unicode choices. a windows terminal enables them only when `WT_SESSION`, `TERM_PROGRAM` or `ConEmuANSI` is present. outside a windows terminal, the first nonempty `LC_ALL`, `LC_CTYPE` or `LANG` enables unicode only when it contains `utf-8` or `utf8`, ignoring case. with none set, unicode stays enabled. fallbacks include `...` for clipping and `>` with worded key hints in the picker. this is not an ASCII-only output mode: text from filenames and sources remains intact.

clickable URLs use OSC 8, a terminal escape sequence for links. they require interactive stderr and one of `WT_SESSION`, a known `TERM_PROGRAM` (`vscode`, `iTerm.app`, `WezTerm` or `ghostty`), numeric `VTE_VERSION >= 5000`, `KONSOLE_VERSION` or `TERM=xterm-kitty`. the presence of `TMUX` or `STY` disables them. link detection is independent of color; other terminals get the plain address.

### reports and counters

JSON is one completed document, not a stream of progress events; `--progress` adds no events to it, only the plain lines on stderr. it holds schema, command, status, result, warnings, errors and work. an error can carry a partial result and per-file failures. warning collection remains independent of whether human warnings are visible. encoding finishes in a byte buffer before stdout is written, so an unsupported path produces a complete error envelope instead of a successful truncated prefix. text answers retain write failures until command completion; help returns them directly. a broken pipe is successful, while other stdout failures make a successful command fail with `local.io`.

`Counts` snapshots atomic counters for requests, downloaded bytes, retries, resumes, object writes, downloads skipped, reflinks, hardlinks, copies and files placed from the cache. [JSON output](../../user/reference/json-output.md) defines the fields and counting contract. elapsed time is reported, but tests use deterministic counts rather than wall-time assertions.

`-v` prints work summaries; `-vv` enables debug events. `FETCHLOOM_TRACE` writes a Chrome trace through a guard that must live until command work ends. trace output is separate from the JSON contract. retry-command validation reads literal quoting rather than splitting quoted paths at spaces; it does not interpret shell expansions or operators.

flag help initializes the same palette as the `help` command. both apply `FETCHLOOM_COLORS` and warn about unreadable entries before printing help.

## invariants and traps

- do not print directly from command handlers. renderer calls preserve stream routing and progress synchronization.
- never depend on color alone to convey an error or completion.
- bars draw only through `progress::terminal`, which puts the cursor back at the start of the last bar's line after each frame. indicatif pads every line to the last column, so the cursor would wait there to wrap, and the `^C` a terminal echoes at a stop would wrap onto a line of its own: clearing the bars then cleared that line and left the bar's last frame above `Stopping`. `a_stop_leaves_no_bar_behind` in tests/cli/progress.rs checks it in a terminal that echoes.
- snapshot filters remove paths, ports, times and rates. inspect unfiltered output when changing a value those filters can hide.
- literal error kinds are stable script interfaces. prose can improve without renaming them.
- masked display URLs do not sanitize raw URL values stored elsewhere. semantic argument errors, such as an unknown auth account, need the same masking as clap errors.
- an unhandled panic is fetchloom's own bug: `main` installs a hook that says so and where to report it, then hands over to rust's default report. normal errors use `Error` and `Kind`. a system that refuses the runtime's threads is found by a probe thread first and reported as `local.io`, while a refusal later, as tokio adds workers, still panics, and release builds abort; [worker budgets](primitives.md#worker-budgets) explains that exception.

## tests

| files | cargo filter |
|---|---|
| [output.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output.rs), [progress.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/progress.rs), [style.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/output/style.rs) | `cargo test --locked --lib output::` |
| [logs.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/logs.rs), [error.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/error.rs), [units.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/units.rs) | `cargo test --locked --lib cli::logs::`; `cargo test --locked --lib error::`; `cargo test --locked --lib units::` |
| [progress.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/progress.rs), [hints.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/hints.rs), [basics.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/basics.rs) | `cargo test --locked --test cli progress::`; `cargo test --locked --test cli hints::`; `cargo test --locked --test cli basics::` |
| [get.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/get.rs), [snapshots](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/cli/snapshots) | `cargo test --locked --test cli get::` |

## related

[CLI](cli.md), [style](../style.md), [profiling](../engineering/profiling.md), [JSON output](../../user/reference/json-output.md), [exit codes](../../user/reference/exit-codes.md).
