# environment

environment variables choose the cache, network limits, credentials and terminal behavior.

```powershell
$env:FETCHLOOM_CACHE = 'D:\fetchloom-cache'
$env:FETCHLOOM_JOBS = '4'
$env:FETCHLOOM_RATE = '2.5M'
```

[CLI options](../commands/README.md#global-options) set per-run behavior. the [settings file](#settings-file) keeps the [resources level](../resources.md), the cache, job, retry, timeout, approval, memory, file-count and archive settings, the allowed origins, the S3 buckets your keys may sign for, the cloud storage buckets your google credentials may go to, the mirrors and the folder `push` writes to, for every run; a variable wins over it. [`fetchloom settings`](../commands/settings.md) shows each value and where it came from.

a variable one typo from one of these, such as `FETCHLOM_CACHE`, changes nothing, so every command but `help` and `completions` warns about it, naming the variable it is near and never its value. other unknown names are left alone.

| variable | effect |
|---|---|
| `FETCHLOOM_CACHE` | nonempty cache root override |
| `FETCHLOOM_OFFLINE` | `true` or `1` enables offline mode, as `--offline` does. `false` or `0` turns it off; unset or empty means false. any other value, such as `yes`, fails a named command with `usage`, exit 2, before any request. offline recovery hints name the flag, variable or both, whichever enabled it |
| `FETCHLOOM_RESOURCES` | `lean`, `default` or `max`: one word that trades speed against memory by setting what no variable or settings file key sets. `lean` runs 2 requests at once and 2 to one host, gives download buffers 128 MiB, or the default budget when that is smaller, and caps local workers at 2, or 1 on one processor, unless `FETCHLOOM_THREADS` is set. `max` runs 32 requests at once and 16 to one host. unset or empty means `default`, which keeps the defaults on this page. no level changes `FETCHLOOM_MOST_FILES`, the archive limits or any other setting. any other value fails with `usage`, exit 2. see [resources](../resources.md) |
| `FETCHLOOM_JOBS` | integer from 1 to 256; how many requests run at once. unset or empty means 8. `FETCHLOOM_HOST_JOBS` caps those to one host. requests waiting for a retry, host quota or source spacing release both slots and retake them before sending |
| `FETCHLOOM_RETRIES` | integer from 0 to 20; how many more times a request is tried after it fails and makes no progress. unset or empty means 4. each wait is a random part of a limit that doubles from 1 s up to 1 min, so 10 waits about 2 minutes in all and 20 about 8: raise it on a network that drops for minutes at a time. a try that moves the download on earns the count back, up to 50 tries in all |
| `FETCHLOOM_HOST_JOBS` | integer from 1 to 256; how many requests may go to one host at once, the streams of a split download included, counting a redirected request by the host it was sent to first. unset or empty means 4. raise it for a host that serves many connections well, such as your own mirror; lower it for one that limits each client |
| `FETCHLOOM_TIMEOUT` | integer seconds from 1 to 3600; how long a server may send nothing in a response body before the try fails and is retried. unset or empty means 30. time held back on this machine by the rate cap, the writer or a question does not count. connecting may take 45 s, or this long when it is longer, and the response headers must come within the connect time plus this |
| `FETCHLOOM_ASK_ABOVE` | a size such as `500M` or `20G`, with the units of `FETCHLOOM_RATE`; a download bigger than it needs [approval](../concepts.md#approval). unset or empty means 1 GiB |
| `FETCHLOOM_MEMORY` | a size such as `512M` or `8G`, with the units of `FETCHLOOM_RATE`; the memory download buffers aim to stay within. the downloads running at once share it for the data waiting to be hashed, up to 16 MiB each, and each of the at most four archives unpacking at once may decompress in a quarter of it, never less than 256 MiB. unset or empty means, on linux, a quarter of the memory available, inside the limit of the process's cgroup, from 256 MiB to 4 GiB and never more than half of what is available. on macos and windows it means 256 MiB. metadata reused within one run has an allowance of one sixteenth of this value, at most 64 MiB |
| `FETCHLOOM_MOST_FILES` | integer from 1 to 1,000,000,000; the most files one dataset may list, counted before select patterns apply. unset or empty means the memory budget at about 2 KiB a file, so `FETCHLOOM_MEMORY` raises it too: a 4 GiB budget allows 2,097,152. it is never less than 100,000. a listing past it stops with `source.invalid`, exit 10, naming this setting |
| `FETCHLOOM_ARCHIVE_FILES` | integer from 1 to 1,000,000,000,000; the most entries, files and folders, one archive may unpack to. unset or empty means 10,000,000 |
| `FETCHLOOM_ARCHIVE_SIZE` | a size such as `16T`, with the units of `FETCHLOOM_RATE`; the most bytes one archive may unpack to. unset or empty means 16 TiB |
| `FETCHLOOM_ARCHIVE_RATIO` | integer from 1 to 1,000,000,000; the most bytes one archive may unpack to for each compressed byte read, counted once 4 MiB are out. unset or empty means 1000, which nearly all zip data stays under while compression bombs pass it. these three guard against archives built to fill a disk, so only this environment and the settings file move them, never `data.toml`. an archive past one stops with `source.invalid`, exit 10, naming it |
| `FETCHLOOM_ALLOWED_ORIGINS` | origins on this machine or its network that a host written in data.toml, data.lock or a `get -i` list may reach, separated by commas, such as `http://lab.local:8080,http://10.0.0.5:9000`: each a scheme, a host and a port when it is not the usual one, without a path. unset or empty allows none. otherwise such a host on a private, loopback or link-local address is refused with `network.refused`, exit 20, before any request, since a cloned project could point it at your network. a link you type or a mirror you name reaches its own origin for that run, and a refusal quotes the value that lets it through |
| `FETCHLOOM_S3_BUCKETS` | the private S3 buckets your [AWS keys](#aws-keys) may sign requests for, separated by commas: `s3://<bucket>`, or the bare name, for a bucket on AWS, and `s3:https://<host>[:<port>]/<bucket>` for one at another service that speaks S3's API, such as `s3://lab-raw,s3:https://minio.lab.example:9000/scans`. unset or empty signs for none. see [private S3 buckets](../sources/buckets.md#private-s3-buckets) |
| `FETCHLOOM_GCS_BUCKETS` | the private cloud storage buckets your [google application default credentials](#google-application-default-credentials) may sign in to, separated by commas: `gs://<bucket>`, or the bare name, such as `gs://lab-private,lab-scans`. unset or empty sends them to none. see [private cloud storage buckets](../sources/buckets.md#private-cloud-storage-buckets) |
| `FETCHLOOM_AZURE_CONTAINERS` | the private azure containers a [SAS, an account key or a sign-in](#azure-credentials) may go to, separated by commas: `azure:<account>/<container>`, or the container's link, `https://<account>.blob.core.windows.net/<container>`, such as `azure:lab/raw,azure:lab/scans`. in the settings file a link may carry the container's own SAS, as the azure portal gives it. unset or empty sends credentials to none. see [private containers](../sources/azure.md#private-containers) |
| `FETCHLOOM_DRIVE_FOLDERS` | the private google drive folders and files your [google application default credentials](#google-application-default-credentials) may read, separated by commas: each by its id or by the link drive gives when you share it, such as `https://drive.google.com/drive/folders/1AbC...`. a folder covers the files google's own listing finds inside it. unset or empty sends them to none. see [private files and folders](../sources/google-drive.md#private-files-and-folders) |
| `FETCHLOOM_MIRRORS` | [mirrors](../mirrors.md) asked for pinned files before their sources, in order, separated by commas: folders, and `http://` or `https://` addresses, such as `/mnt/lab/fetchloom,https://data.lab.example/fetchloom`. text holding `://` is an address and anything else a folder, so `C:\lab` is a folder. a relative folder is taken from the working folder, after `-C`, and a folder whose name holds a comma goes in the settings file's list. `--mirror` adds mirrors before these. unset or empty names none. another scheme, `file://` included, or an address holding a login, a query or a fragment fails with `usage`, exit 2 |
| `FETCHLOOM_MIRRORS_ONLY` | `true` or `1` sends requests to the mirrors alone, never to a source, a search site or a key check: [mirror-only mode](../mirrors.md#mirror-only-mode). `false` or `0` turns it off; unset or empty means false. it needs a mirror, from `FETCHLOOM_MIRRORS`, the settings file or `--mirror`; without one every named command fails with `usage`, exit 2. a file no mirror holds fails with `network.refused`, exit 20 |
| `FETCHLOOM_PUSH_TO` | the folder [`fetchloom push`](../commands/push.md) copies pinned files to when `--to` names none, such as `/mnt/lab/fetchloom`. a relative folder is taken from the working folder, after `-C`. unset or empty names none, and push then needs `--to`. push writes folders only, so a web address, such as `https://data.lab.example/fetchloom`, fails with `usage`, exit 2: copy the folder to a server or a bucket with a tool such as rclone |
| `FETCHLOOM_BUILD_FLOOR` | a size such as `20G`: the least free space a [recipe](../recipes.md)'s step may leave on the disk holding the cache. `fetchloom build` looks at that disk as the step runs and stops it once less is free, with `local.io`, exit 50, leaving `data.lock` and the data folder as they were. a disk with less free than that before any step runs stops the build there, with `--yes` too. unset or empty means the larger of 1 GiB and 1% of that disk. it is never read from `data.toml` |
| `FETCHLOOM_SETTINGS` | nonempty path of the [settings file](#settings-file) to read instead of the default one; the file must exist |
| `FETCHLOOM_NO_CONFIG` | `true` or `1` leaves the [settings file](#settings-file) unread for the run, as `--no-config` does, also one that would stop it. `false` or `0` reads it; unset or empty means false. any other value fails with `usage`, exit 2. data.toml, data.lock and keys are read as always |
| `FETCHLOOM_RATE` | one shared response-body byte budget for metadata, downloads and retries. positive bytes per second, optionally with an uppercase binary suffix `K`, `M`, `G` or `T`, such as `500K` or `2.5M`; unset or empty means no cap |
| `FETCHLOOM_THREADS` | integer from 1 to 256; caps [local worker groups and async runtime workers](../../dev/architecture/primitives.md#worker-budgets), not total OS threads. unset or empty keeps the defaults; surrounding whitespace is accepted |
| `FETCHLOOM_SPLIT` | `false` or `0` disables splitting; `true` or `1` starts eligible split downloads from the first answer. unset or empty uses the adaptive trigger: under 1 MB/s over 2 s, with at least 16 MiB left. any other value fails a named command with `usage`, exit 2. files must be at least 64 MiB and pass the [range checks](../../dev/architecture/network.md#the-1-mbs-handover). a `FETCHLOOM_RATE` cap turns off the adaptive trigger; `true` and a resumed download whose later ranges are kept still split |
| `FETCHLOOM_LOG` | tracing target filter, such as `fetchloom=debug`; overrides the `-vv` default filter; events include uptime timestamps |
| `FETCHLOOM_TRACE` | nonempty path for a chrome trace, including one `download` span per file; relative to the starting directory, before `-C`; the file is created or truncated even with `--dry-run`, so choose a separate output path; open it in [perfetto](../../dev/engineering/profiling.md#timeline) |
| `FETCHLOOM_BUILDING` | set by `fetchloom build` for each step it runs, to the project folder it builds in. a fetchloom inside the step that would lock that project, as `sync` there does, stops at once with kind `usage`, exit 2, since the build holds the project until the step ends |
| `HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY` | the proxy for https links, for http links, and for both when the first two are unset, as `http://proxy.example.org:8080`. a proxy that wants a login takes it in its address, as `http://name:password@proxy.example.org:8080`; messages name a proxy without its login. the lowercase names, such as `https_proxy`, work too, and the uppercase name wins when both are set. an empty value means no proxy. a value that is no proxy address, such as `htp:/proxy:80`, stops a command at its first request with `usage`, exit 2, naming the variable, rather than sending the request without the proxy. a proxy that cannot be reached, as one at a closed port or a name that does not resolve, stops the command with `network.unreachable`, exit 20, naming the proxy without its login and the variable that set it; no other try is made, since the proxy will not come back by itself. [company proxies](../troubleshooting.md#company-proxies) shows a local relay for a proxy that wants a windows sign-in |
| `NO_PROXY` | hosts reached without the proxy, separated by commas, as `localhost,lab.example.org,192.168.0.0/16`: a name covers the names below it too, and an address may carry a mask; `*` reaches every host without it. `no_proxy` works too |
| `SSL_CERT_FILE`, `SSL_CERT_DIR` | on linux, a file of PEM certificates and folders of them to trust instead of the system's store. one that holds no certificate fetchloom can read stops a network command with kind `usage`, exit 2, and other roots never stand in for it. with neither set and no store, as in a container built without `ca-certificates`, fetchloom trusts the mozilla roots it carries. macos and windows ask the system's own verifier, which reads neither |
| `NO_COLOR` | nonempty value disables automatic color |
| `CLICOLOR_FORCE` | nonempty value other than `0` enables automatic color even into a pipe or file, unless `NO_COLOR` wins |
| `CLICOLOR` | `0` disables automatic color |
| `FETCHLOOM_COLORS` | changes colors, as `done=blue:accent=#d787ff`. tones are `done`, `work`, `warn`, `fail` and `accent`; colors are a name such as `magenta` or `bright-blue`, a 256-color number or `#rrggbb`. `theme=ansi`, `theme=dark` or `theme=light` picks the base the other entries change. without one, a terminal that draws full color (`COLORTERM=truecolor` or `24bit`, windows terminal, vs code, iTerm2, WezTerm, ghostty) gets a warm dark theme, or a light one when `COLORFGBG` says the background is white, and any other terminal keeps its own sixteen colors. an entry it cannot read keeps its default and prints one warning |
| `FETCHLOOM_NO_HINTS` | `true` or `1` drops the `hint:` and `next:` lines that suggest a next step. warnings, errors and their `help:` stay. `false`, `0`, unset or empty keeps them, and so does any other value, which a `settings.switch_unread` warning names instead of stopping the command |
| `FETCHLOOM_ACCESSIBLE` | `true` or `1` at a terminal, draws nothing over again, as a screen reader needs: progress comes as plain lines instead of bars, a question such as whether to download a large file is a plain line you answer with y or n, and a choice of dataset is a numbered list you answer by typing a number. it asks with `TERM=dumb` too. under `CI` or a coding agent's variable, with `--json`, or without a terminal, nothing asks, as before. `false`, `0`, unset or empty leaves it off, and so does any other value, which a `settings.switch_unread` warning names instead of stopping the command |
| `TERM` | `dumb` disables automatic color, unicode symbols, bars and questions. with `FETCHLOOM_ACCESSIBLE`, the questions come back as plain lines |
| `CI` | a value other than empty, `false` or `0` disables automatic color and interactivity; a CI service's own word, such as `CI=drone`, counts too |
| `AI_AGENT`, `VSCODE_AGENT`, `CLAUDECODE`, `CODEX_SANDBOX`, `CODEX_THREAD_ID`, `GEMINI_CLI`, `CURSOR_AGENT`, `OPENCODE`, `AUGMENT_AGENT`, `ANTIGRAVITY_AGENT` | a value other than empty, `false` or `0` marks a coding agent's command: no questions, automatic color or animated progress, as under `CI` |
| `WT_SESSION`, `TERM_PROGRAM`, `ConEmuANSI` | presence enables unicode in a windows terminal |
| `COLORTERM`, `COLORFGBG` | terminal-reported color depth and background select the base palette unless `theme=` overrides it |
| `VTE_VERSION`, `KONSOLE_VERSION`, `TMUX`, `STY` | terminal or multiplexer indicators used for [clickable-link detection](../../dev/architecture/output.md#terminal-capabilities), alongside `WT_SESSION`, `TERM_PROGRAM` and `TERM` |
| `LC_ALL`, `LC_CTYPE`, `LANG` | outside a windows terminal, the first nonempty value decides unicode fallbacks: it must contain `utf-8` or `utf8`, ignoring case. with none set, unicode stays enabled; `TERM=dumb` still disables it |

[terminal output](../../dev/architecture/output.md#terminal-capabilities) owns color precedence, stream detection and unicode fallbacks.

the shared rate limit permits a burst of one second's bytes or 1 MiB, whichever is larger. `2.5M` allows 2,621,440 bytes per second.

`FETCHLOOM_THREADS` does not change `FETCHLOOM_JOBS`. a cap of 1 also keeps a required MD5 and SHA-256 on one hash worker; with spare CPU capacity, a large download may compute them on separate workers. network writers and the original hash workers remain separate from the local worker-group cap. archive decoding uses the existing writer budget to overlap storage of large members; with zero or one writer it keeps large-member storage on the reader, without the extra buffers.

sizes and rates take a decimal fraction with digits on both sides of the point, and the suffixes multiply by powers of 1024, rounding down to whole bytes. signs, exponent notation, lowercase suffixes and suffixes such as `MB` are refused. numbers may have spaces around them.

a variable that turns something on or off, such as `FETCHLOOM_OFFLINE`, takes `true` or `1` for on and `false` or `0` for off, in any case and with spaces around them.

an invalid value fails a named command with kind `usage` and exit 2, even one that downloads nothing, naming the variable, or the key and the settings file, except in `FETCHLOOM_ACCESSIBLE` and `FETCHLOOM_NO_HINTS`, which change only what you see; help, version and completions still run, and so does the welcome unless `FETCHLOOM_THREADS` is the bad one. an invalid log filter falls back to `-vv`'s default. logging you asked for writes stderr in quiet or JSON mode too, and turns off progress bars.

synced-folder detection reads `OneDrive`, `OneDriveConsumer`, `OneDriveCommercial`, dropbox's `info.json` under `APPDATA`, `LOCALAPPDATA` or the home folder, the icloud drive folder on windows, on macos `~/Library/Mobile Documents` and the onedrive and dropbox folders in `~/Library/CloudStorage`, and on linux a google drive mount of type `fuse.drive` in `/proc/self/mounts`, as google colab mounts it. the home folder is `HOME`, else `USERPROFILE`. on windows and macos, the system's proxy settings fill in when `HTTPS_PROXY` or `HTTP_PROXY` is unset or empty.

## settings file

a settings file keeps settings for every run, so a shared machine or a big scratch disk needs no exported variables. fetchloom reads `settings.toml` in its own folder, beside saved credentials:

| system | settings file |
|---|---|
| linux | `$XDG_CONFIG_HOME/fetchloom/settings.toml` when that variable holds an absolute path, else `~/.config/fetchloom/settings.toml` |
| macos | `~/Library/Application Support/fetchloom/settings.toml` |
| windows | `%APPDATA%\fetchloom\settings.toml` |

`FETCHLOOM_SETTINGS` names another file. no file at the default place is fine; a file the variable names must exist. `--no-config`, or `FETCHLOOM_NO_CONFIG=1`, leaves the file unread for one run, also a file that would stop it, and `fetchloom settings` then says it was bypassed.

```toml
cache = "/scratch/lab/fetchloom-cache"
jobs = 32
host_jobs = 8
timeout = 120
ask_above = "50G"
memory = "8G"
allowed_origins = ["http://lab.local:8080"]
s3_buckets = ["s3://lab-raw"]
gcs_buckets = ["gs://lab-private"]
azure_containers = ["azure:lab/raw"]
drive_folders = ["https://drive.google.com/drive/folders/1AbCdEfGhIjKlMnOpQrStUvWxYz012345"]
mirrors = ["/mnt/lab/fetchloom", "https://data.lab.example/fetchloom"]
push_to = "/mnt/lab/fetchloom"
```

the keys are `cache`, `resources`, `jobs`, `host_jobs`, `retries`, `timeout`, `ask_above`, `memory`, `most_files`, `archive_files`, `archive_size`, `archive_ratio`, `allowed_origins`, `s3_buckets`, `gcs_buckets`, `azure_containers`, `drive_folders`, `mirrors`, `mirrors_only`, `push_to` and `build_floor`. each takes what its `FETCHLOOM_*` variable takes, as a whole number or text in quotes; `allowed_origins`, `s3_buckets`, `gcs_buckets`, `azure_containers`, `drive_folders` and `mirrors` also take a list of text, and `mirrors_only` takes `true` or `false`. a relative `cache`, a relative folder in `mirrors` or a relative `push_to` is taken from the settings file's folder. an unknown key, a value of another type or a file that is not TOML fails every named command with kind `usage` and exit 2, naming the file; help, version and completions still run.

each value comes from the first of these that sets it: its variable, the settings file, the [resources level](../resources.md), this machine, fetchloom's default. so `FETCHLOOM_JOBS=4 fetchloom sync` changes one run without editing the file. an empty variable sets nothing, so the file's value stands, but for `FETCHLOOM_ALLOWED_ORIGINS`, `FETCHLOOM_S3_BUCKETS`, `FETCHLOOM_GCS_BUCKETS`, `FETCHLOOM_AZURE_CONTAINERS`, `FETCHLOOM_DRIVE_FOLDERS`, `FETCHLOOM_MIRRORS`, `FETCHLOOM_MIRRORS_ONLY` and `FETCHLOOM_PUSH_TO`: an empty one clears what the file names, so one run can turn off an origin, a bucket, a container, a drive folder, a mirror or a push folder the file sets. [`fetchloom settings`](../commands/settings.md) shows each value and where it came from. the other variables on this page have no key in the file.

## credentials

fetchloom reads each source's key where the source's own tools keep it, so a key made for them works here too. [signing in](../signing-in.md) is the place to start; this section gives the order fetchloom looks in. the first match wins; empty values and blank files are skipped.

| account | where fetchloom looks, in order |
|---|---|
| hugging face | `HF_TOKEN`, then one token file: the file `HF_TOKEN_PATH` names, else `token` in `HF_HOME`, else `huggingface/token` in `XDG_CACHE_HOME`, else `~/.cache/huggingface/token` |
| kaggle | `KAGGLE_API_TOKEN`, a key or a file holding one; `access_token` or `access_token.txt` in `~/.kaggle`, as kagglesdk reads it; then a username and key: `KAGGLE_USERNAME` and `KAGGLE_KEY`, each over the same field of `kaggle.json` in `KAGGLE_CONFIG_DIR`, else `~/.kaggle`, else on linux `kaggle` in `XDG_CONFIG_HOME`. a `KAGGLE_API_TOKEN` that is set but unusable skips the `~/.kaggle` token files, as kagglesdk does |
| zenodo, figshare, osf, dryad | `ZENODO_TOKEN`, `FIGSHARE_TOKEN`, `OSF_TOKEN`, `DRYAD_TOKEN`; then a key saved by `fetchloom auth login` |
| github | `GH_TOKEN`, then `GITHUB_TOKEN`, as the gh command reads them; then a key saved by `fetchloom auth login github` |
| gitlab | `GITLAB_TOKEN`; then a key saved by `fetchloom auth login gitlab` |
| dataverse | `DATAVERSE_API_TOKEN` for the installation `DATAVERSE_URL` names, as pyDataverse reads them; then a key saved for that host |
| a host's login | the `machine` entry for that host in `.netrc`, or the file `NETRC` names; on windows `_netrc` when `.netrc` does not exist. `default` entries are ignored |

| variable | effect |
|---|---|
| `FETCHLOOM_KEYRING` | `false` or `0` keeps keys out of the system's credential store; `auth login` then asks before using the [credential-file fallback](../commands/auth.md#signing-in). `true`, `1`, unset or empty uses the store where the system has one. any other value fails a named command with `usage`, exit 2 |
| `NETRC` | the `.netrc` file to read and to save logins in |

[where a key goes](../signing-in.md#where-a-key-goes) lists the hosts each key is sent to.

### AWS keys

AWS keys sign requests only for the buckets `s3_buckets` lists, in the order [private S3 buckets](../sources/buckets.md#private-s3-buckets) gives. they are read where the AWS tools keep them:

| variable | effect |
|---|---|
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | a key and its secret, which come before any profile; one without the other fails with `auth.required` |
| `AWS_SESSION_TOKEN` | the session of temporary keys, sent with them |
| `AWS_PROFILE` | the profile to read, `default` when unset; one neither file has fails with `usage` |
| `AWS_SHARED_CREDENTIALS_FILE`, `AWS_CONFIG_FILE` | the credentials and config files, `~/.aws/credentials` and `~/.aws/config` when unset |
| `AWS_REGION`, `AWS_DEFAULT_REGION` | the region to sign for when neither the host nor S3 names one, before the profile's `region` |
| `AWS_ENDPOINT_URL_S3`, `AWS_ENDPOINT_URL` | the S3-compatible service the keys above sign for, instead of AWS; a profile's `endpoint_url` does the same |
| `AWS_IGNORE_CONFIGURED_ENDPOINT_URLS` | `true` ignores the endpoint settings |
| `PATH` | the folders that hold the AWS CLI and a `credential_process` named without its folder; only folders named by their full path are searched |

a profile with `credential_process` gets its keys from that program, and one that signs in to IAM identity center or takes on a role from the AWS CLI, version 2.9 or later, as [keys from a program](../sources/buckets.md#keys-from-a-program) describes.

### google application default credentials

google's application default credentials get a token only for the buckets `gcs_buckets` lists and the drive folders and files `drive_folders` lists, as [private cloud storage buckets](../sources/buckets.md#private-cloud-storage-buckets) and [private drive files and folders](../sources/google-drive.md#private-files-and-folders) explain. they are read where google's own libraries read them, the first that is set:

| variable | effect |
|---|---|
| `GOOGLE_APPLICATION_CREDENTIALS` | a service account's JSON key file, which comes before a login; one that cannot be read fails with `usage` rather than leave fetchloom to use another identity |
| `CLOUDSDK_CONFIG` | the gcloud folder whose `application_default_credentials.json`, the login `gcloud auth application-default login` saves, is read, `~/.config/gcloud` when unset, or `%APPDATA%\gcloud` on windows |

fetchloom asks only `https://oauth2.googleapis.com/token` for tokens. it refuses with `auth.required` a file that names another token endpoint, and credentials for workload identity federation or service account impersonation, rather than use another identity, and it never asks a metadata server for the identity of the machine it runs on, so on a google cloud virtual machine use a key file or a login. a service account asks for the scope it needs, `devstorage.read_only` for cloud storage and `drive.readonly` for drive; a login's token carries the scopes it was granted, and `gcloud auth application-default login` grants no drive scope unless it signs in through an oauth client of your own, as [with your own login](../sources/google-drive.md#with-your-own-login) shows.

### azure credentials

a SAS, an account key or a sign-in's token goes only to the containers `azure_containers` lists, as [private containers](../sources/azure.md#private-containers) explains. a container's own entry in the settings file may carry its SAS link; otherwise each is read where the az cli reads it:

| variable | effect |
|---|---|
| `AZURE_STORAGE_SAS_TOKEN` | a SAS, the query the azure portal or `az storage container generate-sas` gives, with or without its leading `?`; one without `sv` and `sig` fails with `auth.required` once a listed container needs it, without repeating it |
| `AZURE_STORAGE_ACCOUNT` | the storage account that SAS or key is for, so it goes only to listed containers in that account. unset, the listed containers without a SAS of their own must all be in one account: in two or more, a request that needs the SAS fails with `auth.required` and names this variable, before the SAS goes anywhere |
| `AZURE_STORAGE_KEY` | one of that account's access keys, in base64 as the azure portal or `az storage account keys list` gives it, which signs the requests of its listed containers and is never sent. it needs `AZURE_STORAGE_ACCOUNT`; set without it, or holding no key fetchloom can read, it fails with `auth.required` once a listed container needs it, without repeating it. a SAS for the same account comes first |
| `AZURE_TOKEN_CREDENTIALS` | how to sign in for a listed container no SAS or key covers, as azure's own libraries read it, in any case: unset, `AzureCliCredential` or `dev` take the az cli's login, through `az account get-access-token`; `ManagedIdentityCredential` or `prod` take this machine's managed identity, from its instance metadata service, which nothing else lets fetchloom ask; `prod` beside `AZURE_CLIENT_SECRET`, `AZURE_CLIENT_CERTIFICATE_PATH` or `AZURE_FEDERATED_TOKEN_FILE`, or any other value, fails with `auth.required` once a container needs a token |
| `AZURE_CLIENT_ID` | the client id of the user-assigned managed identity to sign in as; unset takes the machine's own |
| `AZURE_CONFIG_DIR` | the az cli's folder, `~/.azure` when unset, where [`fetchloom auth`](../commands/auth.md#status) reads whom `az login` signed in, without running the az cli |
| `PATH` | the folders that hold the az cli, `az`, or `az.cmd` on windows; only folders named by their full path are searched, and unset `AZURE_TOKEN_CREDENTIALS` with no az cli there means no sign-in |
