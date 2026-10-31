# environment

environment variables choose the cache, network limits, credentials and terminal behavior.

```powershell
$env:FETCHLOOM_CACHE = 'D:\fetchloom-cache'
$env:FETCHLOOM_JOBS = '4'
$env:FETCHLOOM_RATE = '2.5M'
```

there is no config file. [CLI options](../commands/README.md#global-options) set per-run behavior.

| variable | effect |
|---|---|
| `FETCHLOOM_CACHE` | nonempty cache root override |
| `FETCHLOOM_OFFLINE` | exactly `1` enables offline mode; `--offline` also enables it. offline recovery hints name the flag, variable or both, whichever enabled it |
| `FETCHLOOM_JOBS` | integer from 1 to 256; unset or empty means 8; surrounding whitespace is accepted |
| `FETCHLOOM_RATE` | one shared response-body byte budget for metadata, downloads and retries. positive bytes per second, optionally with an uppercase binary suffix `K`, `M`, `G` or `T`, such as `500K` or `2.5M`; unset or empty means no cap |
| `FETCHLOOM_THREADS` | integer from 1 to 256; caps [local worker groups and async runtime workers](../../dev/architecture/primitives.md#worker-budgets), not total OS threads. unset or empty keeps the defaults; surrounding whitespace is accepted |
| `FETCHLOOM_SPLIT` | exactly `0` disables splitting; exactly `1` starts eligible split downloads from the first answer. unset, empty or any other value uses the adaptive trigger: under 1 MB/s over 2 s, with at least 16 MiB left. files must be at least 64 MiB and pass the [range checks](../../dev/architecture/network.md#adaptive-split-downloads). a `FETCHLOOM_RATE` cap prevents splitting, including forced splitting |
| `FETCHLOOM_LOG` | tracing target filter, such as `fetchloom=debug`; overrides the `-vv` default filter |
| `FETCHLOOM_TRACE` | nonempty path for a chrome trace; relative to the starting directory, before `-C` |
| `NO_COLOR` | nonempty value disables automatic color |
| `CLICOLOR_FORCE` | nonempty value other than `0` enables automatic color on a terminal unless `NO_COLOR` wins |
| `CLICOLOR` | `0` disables automatic color |
| `TERM` | `dumb` disables automatic color, unicode symbols and interactivity |
| `CI` | nonempty value disables automatic color and interactivity |
| `WT_SESSION`, `TERM_PROGRAM`, `ConEmuANSI` | presence enables unicode in a windows terminal |

[terminal output](../../dev/architecture/output.md#terminal-capabilities) owns color precedence, stream detection and unicode fallbacks.

the shared rate limit permits a burst of one second's bytes or 1 MiB, whichever is larger. `2.5M` allows 2,621,440 bytes per second.

`FETCHLOOM_THREADS` does not change `FETCHLOOM_JOBS`.

rate values accept surrounding whitespace and a decimal fraction with digits on both sides of the point. suffixes multiply by powers of 1024; fractions round down to whole bytes per second. the resulting rate must be from 1 to 18,446,744,073,709,551,615. signs, exponent notation, lowercase suffixes and suffixes such as `MB` are refused.

invalid jobs and rate values fail a named command with kind `usage` and exit 2, even when it does not download. welcome, bare-project status, help and version bypass those checks. the thread setting is also checked for welcome and bare-project status, since the latter may hash files; help and version bypass it. an invalid thread value also fails with kind `usage` and exit 2. an invalid log filter falls back to `-vv`'s default, if requested. explicit logging can write stderr in quiet or JSON mode and disables progress bars.

synced-folder detection reads `OneDrive`, `OneDriveConsumer`, `OneDriveCommercial`, dropbox's `info.json` under `APPDATA`, `LOCALAPPDATA` or the home folder, and the icloud drive folder on windows. system proxy settings and proxy environment variables are read by the HTTP stack.

## credentials

hugging face credentials use this precedence, ignoring empty environment values:

1. `HF_TOKEN`, used directly.
2. the file named by `HF_TOKEN_PATH`.
3. `token` inside `HF_HOME`.
4. `huggingface/token` inside `XDG_CACHE_HOME`.
5. `~/.cache/huggingface/token`.

the chosen value or file contents are trimmed. an unreadable file or blank token yields no credential; fetchloom does not try lower-priority locations after choosing one.

other credentials are read from where each source already keeps them: `KAGGLE_API_TOKEN` or `~/.kaggle/access_token`, else `KAGGLE_USERNAME` and `KAGGLE_KEY` or `kaggle.json` in `KAGGLE_CONFIG_DIR` or `~/.kaggle`, `ZENODO_TOKEN`, `FIGSHARE_TOKEN`, `OSF_TOKEN`. a token never appears in `data.toml`, `data.lock`, logs or `--json` output.

`DATAVERSE_TOKEN` is not read. use public dataverse links; an access refusal needs a public copy from the dataset's owner.
