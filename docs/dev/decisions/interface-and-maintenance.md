# Interface and maintenance

| what | why | contract |
|---|---|---|
| use flags, environment variables and one settings file | a shared machine or a big scratch disk needs settings that last without exported variables. one order for every value, variable then file then machine then default, with `fetchloom settings` showing where each came from, keeps a second source from becoming a precedence puzzle | [settings file](../../user/reference/environment.md#settings-file) |
| require an explicit dataset choice | public catalogs can return several plausible matches; `--yes` only approves downloads | [search selection](../../user/commands/search.md#choosing-a-dataset) |
| give errors stable kinds | scripts can act without parsing prose | [exit codes](../../user/reference/exit-codes.md) |
| hand no command as a JSON `fix` that deletes, overwrites or repins what a person keeps, runs the project's code or says `--yes` | a script or an agent runs `fix` unread: one ran `fetchloom remove notes`, the fix of an add whose name data.toml had, and the dataset was gone. `args::Command::harmless` says which commands and options a script may run, as a match the compiler holds to every command, and `output::fix` stops at the first command a help names that is not one, since what follows it is often its next step; a test walks every command and option against a table, so a new one is no fix until someone decides it is safe | [JSON output](../../user/reference/json-output.md) |
| leave `fix` empty in a `--locked` run for any command that may write data.lock | `--locked` promises CI that data.lock stays as committed, and a script or agent runs `fix` unread, so a `fetchloom lock` there would pin what the sources serve now and undo the promise. the rule sits in `output::fix`, which `judged` in `cli/typed.rs` tells, through `Command::writes_lock`, which commands write data.lock, so a new error a locked run ends with cannot slip one through; `help` keeps every way on for a person | [JSON output](../../user/reference/json-output.md) |
| keep terminal formatting in one renderer | commands share stream routing, prompts, aligned verbs and progress behavior | [output](../architecture/output.md) |
| test behavior and work counts without coverage quotas | extra requests, downloads and copies can be detected independently of runner speed | [engineering](../engineering/README.md), [performance](../performance/README.md) |
| fuzz parsers in their modules | parsers can remain private without a separate fuzzing API | [fuzzing](../engineering/fuzzing.md) |
| ship the same executable in wheels | published binary wheels will let pip users receive the same implementation without rust; current source installation still needs it | [releasing](../engineering/releasing.md) |
| test and release native arm64 linux and windows | those artifacts need the same behavioral gate as x86_64 | [CI](../engineering/ci.md), [release targets](../engineering/releasing.md#workflow) |
| test macos on apple silicon and release it for x86_64 and arm64 | apfs clones files, ignores case and keeps unicode composition, so placement and status need their own gate; the intel build runs its installed-artifact journey before merge as well as before release | [CI](../engineering/ci.md), [release targets](../engineering/releasing.md#workflow) |
| keep reasons near the implementation and update docs with behavior | contributors can check the constraint before changing it | [style](../style.md) |

## apache-2.0

| what | why | contract |
|---|---|---|
| use Apache-2.0 | permit use, modification and redistribution with explicit patent terms | [LICENSE](https://github.com/nikhil-yn-aix/fetchloom/blob/main/LICENSE) |
| distribute NOTICE | preserve attribution and the project-name notice; the license grants no trademark rights beyond describing origin | [NOTICE](https://github.com/nikhil-yn-aix/fetchloom/blob/main/NOTICE) |
| ship THIRD-PARTY-LICENSES with every release package | every license deny.toml allows but Zlib asks that its text or notice travel with copies, binary ones included; cargo-about collects them over exactly that list | [releasing](../engineering/releasing.md#artifacts), [about.toml](https://github.com/nikhil-yn-aix/fetchloom/blob/main/about.toml) |
