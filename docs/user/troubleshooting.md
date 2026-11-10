# troubleshooting

error text, exit codes and local status distinguish missing inputs from changed bytes and access problems.

```sh
fetchloom status
fetchloom -v sync
```

verbose output adds error kinds and work counters. use `--json` for [structured failures](reference/json-output.md), including every failed file in a download batch.

## collect a debug report

run the failing command again with `-vv` and save stderr, then add the version line:

```sh
fetchloom -vv sync 2> fetchloom-debug.txt
fetchloom --version -v >> fetchloom-debug.txt
```

on windows, run these lines in cmd or powershell 7; windows powershell 5.1 rewrites redirected stderr. attach `fetchloom-debug.txt`, the exit code, and the `data.toml` and `data.lock` entries for the dataset. debug lines say whether each file came from the cache or a download, why a placed file was hashed again, which placement method was used and why a better one was given up, which host a stored credential was sent to, and each retry and redirect.

credentials are logged by host and header, not by value. URL logins and recognized secret fields show as `***`; arbitrary secret fields cannot be identified reliably. the log also names local paths, such as the project folder, dataset references, and every link fetchloom asked for with its query, search words included, so read the file before sharing it. `FETCHLOOM_LOG=fetchloom::net=debug` limits the log to one part of fetchloom when the full log is too long.

## errors

| symptom on stderr | cause | fix |
|---|---|---|
| `fetchloom: command not found`, or `fetchloom is not recognized` on windows | the terminal started before rust added its folder to PATH | open a new terminal; if that fails, check `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on windows) holds fetchloom |
| `error: there is no command called "init"` | no setup command is needed | run `fetchloom add <link>` in the project folder |
| `error: data.toml has an unfinished git merge conflict`, or `data.lock has ...` | a git merge left both sides of a change between its `<<<<<<<`, `=======` and `>>>>>>>` lines | keep the datasets from both sides and delete those lines by hand, then run `fetchloom lock`; where both sides changed one dataset, keep one side |
| `error: there is no data.toml in this folder or any folder above it inside the git repository at /home/ada/thesis/experiment` | the folder is in a git repository of its own, and the search for a project stops at its top | run `fetchloom add <link>` to start a project there, or name the folder of the project above it with `-C`, as in `fetchloom -C .. sync`; see [finding the project](reference/project-folder.md#finding-the-project) |
| `error: <link> is missing` or `error: <link>... is missing` | `add` or `get` needs a reference | pass a dataset link, or a list to `get -i`; use `fetchloom get --help` for syntax |
| `error: offline, and 1 file data.lock names is not in the cache` | an offline project needs uncached bytes | sync online first; remove `--offline` and unset `FETCHLOOM_OFFLINE` if it is `1`. a CI job can restore a cache that holds them, as [CI](ci.md) shows |
| `error: gave up on huggingface.co after 5 tries` | the network or the site stayed down longer than the retries wait | run the same command when the network is back; downloaded bytes are kept and the next run goes on from them. on a network that drops for minutes, set `FETCHLOOM_RETRIES=10` |
| `error: gave up on huggingface.co after 5 tries: could not connect to huggingface.co`, on a network whose proxy asks for your windows sign-in | the proxy wants NTLM or Kerberos, which fetchloom cannot send | run a local relay that signs in for you, as [company proxies](#company-proxies) shows |
| `error: iris/iris.csv is not the file data.lock records` | downloaded bytes differ from the pin | investigate the source; upgrade only if the change is intended |
| `error: downloading huge.bin needs 2.0 GiB, and fetchloom cannot ask you here` | a large download needs a yes, and no one is at a terminal to answer | check the size, then pass `--yes` if you want it |
| `error: SSL_CERT_FILE names /etc/ssl/corp.pem, which holds no certificates fetchloom can read` | the variable names a missing file, or one without PEM certificates | unset it to use the system's certificates, or set it to a file of PEM certificates |
| `error: this sync needs at least 170.0 GiB on the disk holding the cache at /home/ada/.cache/fetchloom, which has 150.0 GiB free` | the downloads, and the archives they unpack, need more than the cache's disk has free | free space there, or set `FETCHLOOM_CACHE` to a folder on a bigger disk. downloads already in the cache are kept. `--yes` goes on anyway, as on a disk that compresses |
| `error: could not write to the cache: the disk holding /media/usb/cache can neither link files nor rename one without replacing a file already at its new name, ...` | the cache's disk has no hardlinks and no rename that refuses a name already taken, so saving a file there could replace one another fetchloom run had just saved | set `FETCHLOOM_CACHE` to a folder on another disk |
| `error: iris/iris.csv already exists with other contents` | `get` found conflicting local bytes | move the file somewhere safe or choose another folder with `-o` |
| `error: dataset has no file or folder called not-a-file.csv` | the requested member is absent | use one of the dataset paths named in the error details |

for a size mismatch, the full stderr answer can be:

```text
error: iris/iris.csv is not the file data.lock records
  expected 32 bytes, got 15
  help: if the new version is intended, run `fetchloom lock --upgrade iris`
```

if the new input is intended, run the suggested upgrade, sync and commit the lock. otherwise preserve the pin and ask the source owner for the original bytes.

## warnings and unexpected results

| symptom | cause | fix |
|---|---|---|
| sync succeeds but a file is still edited | conflicts are preserved by default | inspect `status` and JSON `placement.kept`; save edits before choosing `--force` |
| `warning: <path> was put back as pinned: it is linked to the cache's copy, which was changed. the changed copy is kept as <path>.changed` | an edit made in place through a hardlink changed the cache's copy, which every project using the file shares | nothing; sync downloaded the file again, and the edit is in the `.changed` file. use `copy = true` for files you edit |
| `warning: <path> was edited, and the file is shared with other projects, so they see the edit too` | a hardlink shared by several projects changed after the cache lost its own copy | save the edit, then run `fetchloom sync --force` in each project; use `copy = true` for files you edit |
| `warning: <path> is a link where data.lock places a file` | a link, or another entry, stands where a pinned file belongs | move what you need, then run `fetchloom sync --force`; it removes the link itself, never what it points to |
| `warning: hf:datasets/mozilla-foundation/common_voice_17_0 holds only .gitattributes and README.md, no data files` | the owner moved the data elsewhere and left a readme saying where | read the dataset's README.md; if the data is gone, run `fetchloom remove <name>` |
| `warning: iris was taken, so this one is iris-2` | the project already has a dataset with that name | keep it, or run `fetchloom remove iris-2` and add again with `--as <name>` |
| status exits 0 with differences | differences are a successful result | inspect every status array |
| an input is read-only | default placement protects shared bytes | set `copy = true` on the dataset and sync |
| another account on the same machine downloads the same data again | each account has its own cache, readable by its owner only | share one cache among people who trust each other, as [one cache for a whole lab](hpc.md#one-cache-for-a-whole-lab) shows |
| a glob matches no files | it differs from listed paths or targets archive members | quote the glob and check [pattern rules](reference/data-toml.md#patterns) |
| adding the same reference rejects different filters | the existing entry already owns that reference and settings | edit `data.toml` or pass an unused `--as` name |
| an archive remains packed | the extension or bytes are unsupported, or extraction is disabled | check [archive rules](concepts.md#archives) |
| an archive has no dataset folder | it contains no regular files | use `extract = false` and sync to keep the packed archive |
| a source returns an access refusal | access is unavailable or unsupported | check the source's [credentials and limits](sources/README.md) |
| sync warns it could not add the project to the cache's list of projects | the cache folder could not be written | fix the cache folder's permissions and sync again before relying on `cache gc` to keep the project's files |
| `cache gc` exits 50 after partial cleanup | some paths could not be removed or inspected | inspect `result.not_removed` causes; fix access or storage before retrying |
| `cache verify` exits 30 | cached bytes changed | allow a later online sync to replace damaged objects, then sync affected projects |
| `cache verify` exits 50 without changed bytes | objects, metadata or directory entries could not be read | inspect `result.unreadable` causes, fix access or storage and verify again |
| a windows file cannot be replaced or deleted | a program holds it open, or permissions block access | close the program, check permissions and retry |
| a long path fails | the filesystem or another program may have a shorter limit | follow the shorter-project-path hint; use shorter project and dataset folders |
| plain-word add or get refuses to guess | no interactive dataset choice is possible | choose a returned canonical id; `--yes` never selects one |
| an option is rejected before a command | command-specific options belong after that command | use `fetchloom add --as flowers zenodo:3608990`, not `fetchloom --as flowers add zenodo:3608990` |
| a download warns that it is a web page | the URL may name a page rather than data | find the source's download or raw link |

to make iris writable, edit its existing entry in `data.toml` and sync:

```toml
[datasets.iris]
ref = "zenodo:3608990"
copy = true
```

```sh
fetchloom sync
```

copied placement inside onedrive can report:

```text
warning: iris is inside onedrive, which breaks links, so the file was copied
  help: a folder outside onedrive lets fetchloom try links
```

move the project outside synced storage if you want fetchloom to try hardlinks. reflinks may still work inside a synced folder. [windows](windows.md) covers drive choice and Defender scanning.

## company proxies

fetchloom sends its requests through the proxy that `HTTPS_PROXY` or `HTTP_PROXY` names. on windows and macos, the system's proxy settings fill in when those variables are unset. a proxy that inspects https needs its certificate in the system's certificate store, as the certificate error says.

a proxy that asks for your windows sign-in, by NTLM or Kerberos, refuses every request until it gets that sign-in, which fetchloom cannot send. an https link then fails as `could not connect to <host>` after every retry, and a plain http link as `<host> answered 407`. `curl -v -x http://proxy.example.org:8080 https://huggingface.co`, with your proxy's address, shows `407 Proxy Authentication Required` and a `Proxy-Authenticate` header naming `NTLM` or `Negotiate`. a local relay can sign in for you: [px](https://github.com/genotrance/px) uses your windows sign-in, and [cntlm](https://github.com/versat/cntlm) the user name and password in its settings. both take requests from this machine on port 3128 by default, so point fetchloom there:

```powershell
$env:HTTPS_PROXY = 'http://127.0.0.1:3128'
$env:HTTP_PROXY = 'http://127.0.0.1:3128'
```

```sh
export HTTPS_PROXY=http://127.0.0.1:3128
export HTTP_PROXY=http://127.0.0.1:3128
```

the variables win over the system's settings, and fetchloom reaches a proxy on a private or loopback address, such as a relay on your own machine. to keep the variables, put the lines in your shell's profile, as the next section shows.

## slow or metered links

`FETCHLOOM_RATE` caps the bytes fetchloom receives, all downloads together, such as `500K` for 500 KiB a second; [environment](reference/environment.md) gives the units. the settings file has no key for it, so to keep a cap in every session, set it in your shell's profile: `~/.bashrc` for bash, `~/.zshrc` for zsh, or the file `$PROFILE` names in powershell.

```sh
export FETCHLOOM_RATE=500K
```

```powershell
$env:FETCHLOOM_RATE = '500K'
```

in a new terminal, `fetchloom settings` shows the `rate` row coming from the environment.

## project recovery

restore a broken lock from git when you need its exact pins. deleting it and running `fetchloom lock` can resolve newer inputs. a lock with a higher `version` comes from a newer fetchloom and needs one.

a failed `add` attempts to restore both `data.toml` and `data.lock`. if either restoration fails, restore the metadata from the named recovery file under `.fetchloom/`. it holds the original bytes, not a JSON string; do not paste its contents into logs, since they may contain credentials. cached downloads and files already placed can remain.

if `remove` changed metadata but could not delete files, fix the reported local error and run sync to finish removal.

an interrupted project command can leave an owner record. only delete the exact owner path named by a waiting command after confirming that its owning process has ended. records from another operating system are never reclaimed by age or pid. [project state](reference/project-folder.md#owner-record) explains the locking rules.

### the system cannot start another worker

local placement, cache verification and archive work fall back to the calling thread when the OS refuses a worker. before starting the async runtime, fetchloom probes whether a thread can be created. a refused probe reports `local.io` (exit 50), including in JSON. the probe does not reserve resources for later runtime workers. reduce `FETCHLOOM_THREADS` or raise the process or container's thread limit, then retry. this is a resource failure, not a corrupt cache.
