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
| `FETCHLOOM_JOBS` | integer from 1 to 256; unset or empty means 8; surrounding whitespace is accepted. requests to any one host are capped at 4 at once, the streams of a split download included. that cap is fixed and `FETCHLOOM_JOBS` does not raise it, so a project whose files all come from one host downloads at most 4 at a time |
| `FETCHLOOM_RATE` | one shared response-body byte budget for metadata, downloads and retries. positive bytes per second, optionally with an uppercase binary suffix `K`, `M`, `G` or `T`, such as `500K` or `2.5M`; unset or empty means no cap |
| `FETCHLOOM_THREADS` | integer from 1 to 256; caps [local worker groups and async runtime workers](../../dev/architecture/primitives.md#worker-budgets), not total OS threads. unset or empty keeps the defaults; surrounding whitespace is accepted |
| `FETCHLOOM_SPLIT` | exactly `0` disables splitting; exactly `1` starts eligible split downloads from the first answer. unset, empty or any other value uses the adaptive trigger: under 1 MB/s over 2 s, with at least 16 MiB left. files must be at least 64 MiB and pass the [range checks](../../dev/architecture/network.md#the-1-mbs-handover). a `FETCHLOOM_RATE` cap turns off the adaptive trigger; `1` and a resumed download whose later ranges are kept still split |
| `FETCHLOOM_LOG` | tracing target filter, such as `fetchloom=debug`; overrides the `-vv` default filter; events include uptime timestamps |
| `FETCHLOOM_TRACE` | nonempty path for a chrome trace, including one `download` span per file; relative to the starting directory, before `-C`; open it in [perfetto](../../dev/engineering/profiling.md#timeline) |
| `NO_COLOR` | nonempty value disables automatic color |
| `CLICOLOR_FORCE` | nonempty value other than `0` enables automatic color even into a pipe or file, unless `NO_COLOR` wins |
| `CLICOLOR` | `0` disables automatic color |
| `FETCHLOOM_COLORS` | changes colors, as `done=blue:accent=#d787ff`. tones are `done`, `work`, `warn`, `fail` and `accent`; colors are a name such as `magenta` or `bright-blue`, a 256-color number or `#rrggbb`. `theme=ansi`, `theme=dark` or `theme=light` picks the base the other entries change. without one, a terminal that draws full color (`COLORTERM=truecolor`, windows terminal, vs code, iTerm2, WezTerm, ghostty) gets a warm dark theme, or a light one when `COLORFGBG` says the background is white, and any other terminal keeps its own sixteen colors. an entry it cannot read keeps its default and prints one warning |
| `FETCHLOOM_NO_HINTS` | exactly `1` drops the `hint:` and `next:` lines that suggest a next step. warnings, errors and their `help:` stay |
| `TERM` | `dumb` disables automatic color, unicode symbols and interactivity |
| `CI` | nonempty value disables automatic color and interactivity |
| `AI_AGENT`, `VSCODE_AGENT`, `CLAUDECODE`, `CODEX_SANDBOX`, `CODEX_THREAD_ID`, `GEMINI_CLI`, `CURSOR_AGENT`, `OPENCODE`, `AUGMENT_AGENT`, `ANTIGRAVITY_AGENT` | a value other than `0` or `false` marks a coding agent's command: no questions, automatic color or animated progress, as under `CI` |
| `WT_SESSION`, `TERM_PROGRAM`, `ConEmuANSI` | presence enables unicode in a windows terminal |

[terminal output](../../dev/architecture/output.md#terminal-capabilities) owns color precedence, stream detection and unicode fallbacks.

the shared rate limit permits a burst of one second's bytes or 1 MiB, whichever is larger. `2.5M` allows 2,621,440 bytes per second.

`FETCHLOOM_THREADS` does not change `FETCHLOOM_JOBS`.

rate values accept surrounding whitespace and a decimal fraction with digits on both sides of the point. suffixes multiply by powers of 1024; fractions of any precision round down to whole bytes per second after that multiplication. the resulting rate must be from 1 to 18,446,744,073,709,551,615. signs, exponent notation, lowercase suffixes and suffixes such as `MB` are refused.

invalid jobs and rate values fail a named command with kind `usage` and exit 2, even when it does not download. welcome, bare-project status, help and version bypass those checks. the thread setting is also checked for welcome and bare-project status, since the latter may hash files; help and version bypass it. an invalid thread value also fails with kind `usage` and exit 2. an invalid log filter falls back to `-vv`'s default, if requested. explicit logging can write stderr in quiet or JSON mode and disables progress bars.

synced-folder detection reads `OneDrive`, `OneDriveConsumer`, `OneDriveCommercial`, dropbox's `info.json` under `APPDATA`, `LOCALAPPDATA` or the home folder, the icloud drive folder on windows, and on macos `~/Library/Mobile Documents` and the onedrive and dropbox folders in `~/Library/CloudStorage`. the home folder is `HOME`, else `USERPROFILE`. system proxy settings and proxy environment variables are read by the HTTP stack.

## credentials

fetchloom reads each source's key where the source's own tools keep it, so a key made for them works here too. [`fetchloom auth`](../commands/auth.md) shows what was found and where, and signs you in. the first match wins; empty values and blank files are skipped.

| account | where fetchloom looks, in order |
|---|---|
| hugging face | `HF_TOKEN`, else `HUGGING_FACE_HUB_TOKEN` while `HF_TOKEN` is unset or empty, as `huggingface_hub` reads them; then one token file, as `huggingface_hub` picks it: the file `HF_TOKEN_PATH` names, else `token` in `HF_HOME`, else `huggingface/token` in `XDG_CACHE_HOME`, else `~/.cache/huggingface/token`; then the older `~/.huggingface/token` |
| kaggle | `KAGGLE_API_TOKEN`, a key or a file holding one; `access_token` or `access_token.txt` in `~/.kaggle`, as kagglesdk reads it; then a username and key: `KAGGLE_USERNAME` and `KAGGLE_KEY`, each over the same field of `kaggle.json` in `KAGGLE_CONFIG_DIR`, else `~/.kaggle`, else on linux `kaggle` in `XDG_CONFIG_HOME`. a `KAGGLE_API_TOKEN` that is set but unusable skips the `~/.kaggle` token files, as kagglesdk does |
| zenodo, figshare, osf | `ZENODO_TOKEN`, `FIGSHARE_TOKEN`, `OSF_TOKEN`; then a key saved by `fetchloom auth login` |
| dataverse | `DATAVERSE_API_TOKEN` for the installation `DATAVERSE_URL` names, as pyDataverse reads them; then a key saved for that host |
| a host's login | the `machine` entry for that host in `.netrc`, or the file `NETRC` names; on windows `_netrc` when `.netrc` does not exist. `default` entries are ignored |

| variable | effect |
|---|---|
| `FETCHLOOM_KEYRING` | `0` keeps keys out of the system's credential store; `auth login` then asks before saving them in a file only you can read |
| `NETRC` | the `.netrc` file to read and to save logins in |

a key goes over https only, to its own host on port 443, and never after a redirect leaves that host. a dataverse key saved for an installation on another port, as `--host dv.example.edu:8443`, goes to that port only. a `.netrc` login also answers on later hops to its own host, as curl sends it, and an osf key also reaches `osf.io` and `files.osf.io`, where osf's downloads go. no key appears in `data.toml`, `data.lock`, logs or `--json` output. recognized credential fields in URL queries and fragments are masked in output and refused in project references. a key holding a character no header can carry, such as a line break, is left out of every request; [`fetchloom auth`](../commands/auth.md#status) warns about it.

AWS and google cloud credentials, such as `AWS_ACCESS_KEY_ID`, `~/.aws` and `GOOGLE_APPLICATION_CREDENTIALS`, are not read. [public buckets](../sources/buckets.md) are listed and downloaded anonymously.