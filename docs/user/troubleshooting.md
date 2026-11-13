# troubleshooting

find the error or warning you see in the tables below, with its cause and fix.

```sh
fetchloom status
fetchloom -v sync
```

verbose output adds [what the run spends](resources.md#see-what-a-run-uses), error kinds and work counters. use `--json` for [structured failures](reference/json-output.md), including every failed file in a download batch.

## collect a debug report

run the failing command again with `-vv` and save stderr, then add the version line:

```sh
fetchloom -vv sync 2> fetchloom-debug.txt
fetchloom --version -v >> fetchloom-debug.txt
```

the version line names the commit fetchloom was built from, or says `commit unknown, built outside git`. on windows, run these lines in cmd or powershell 7, since windows powershell 5.1 rewrites redirected stderr. attach `fetchloom-debug.txt`, the exit code, and the `data.toml` and `data.lock` entries for the dataset. the debug lines say where each file came from, why a file was hashed again, how it was placed, which host a key was sent to, and each retry and redirect.

keys are never logged. a link shows its login as `***`, and each value in its query or fragment too, unless the value's name is known to be harmless, such as `dl`, `version`, `page` or dropbox's `rlkey`, so a token under a name fetchloom does not know stays out of the log. the log also names local paths and every link fetchloom asked for, search words included, so read the file before sharing it. `FETCHLOOM_LOG=fetchloom::net=debug` limits the log to one part of fetchloom when the full log is too long.

## errors

| message on stderr | fix | see |
|---|---|---|
| `fetchloom: command not found`, or `fetchloom is not recognized` on windows | open a new terminal, since one started before the install lacks its folder on PATH; if that fails, check that `~/.cargo/bin`, or `%USERPROFILE%\.cargo\bin` on windows, holds fetchloom | [install](getting-started.md#install) |
| `error: there is no command called "init"` | none is needed: run `fetchloom add <link>` in the project folder | [add](commands/add.md#what-happens) |
| `error: data.toml has an unfinished git merge conflict`, or `data.lock has ...` | run `fetchloom lock`, which merges both files without downloading; a dataset both sides changed waits for `--ours <name>` or `--theirs <name>` | [after a git merge](commands/lock.md#after-a-git-merge) |
| `error: there is no data.toml in this folder or any folder above it inside the git repository at /home/ada/thesis/experiment` | the search for a project stops at the top of the folder's own git repository: run `fetchloom add <link>` to start one there, or name the project above it with `-C`, as in `fetchloom -C .. sync` | [finding the project](reference/project-folder.md#finding-the-project) |
| `error: add needs <link>, <path> or -i <file>`, or `error: get needs <link>` | pass a dataset link, or a list with `-i`; `fetchloom add --help` shows the forms | [add](commands/add.md#usage) |
| `error: offline, and 1 file data.lock names is not in the cache` | sync online first, or remove `--offline` and unset `FETCHLOOM_OFFLINE` if it is `1` or `true`; a CI job can restore a cache that holds the files, as [CI](ci.md) shows | [offline](concepts.md#offline) |
| `error: 1 file data.lock names is on no mirror, and mirrors_only lets fetchloom ask its mirrors alone` | ask whoever keeps the mirror to add the file, as a fetchloom that may ask sources can with [`fetchloom push`](commands/push.md); the details say what each mirror answered | [mirror-only mode](mirrors.md#mirror-only-mode) |
| `error: gave up on huggingface.co after 5 tries` | run the same command once the network is back; it goes on from the bytes kept, where the server allows it, as the line under the error says. on a network that drops for minutes, set `FETCHLOOM_RETRIES=10` | [recovery](concepts.md#recovery) |
| `error: export.arxiv.org refused one request after another because it is busy, so this run asks it nothing more` | run the same command later. a source that asks for its requests to start apart, as arXiv does, is asked less often after each answer that it is busy, and after four in a row not at all | [arXiv's terms](sources/arxiv.md#arxivs-terms) |
| `error: 127.0.0.1 sent a reply that is not valid HTTP` | check the link's `http` or `https` and its port; if the link is right, only whoever runs the server can fix it | [URLs and DOIs](sources/urls-and-dois.md#accepted-references) |
| `Restarting big.bin · the 2.0 MiB an earlier run kept cannot be continued` | nothing: the file downloads again from its first byte, as from a server that sends no parts of a file, such as python's `http.server`, or after the file changed. a stop against such a server says so, as `the server cannot continue from ... so the next run starts over` | [recovery](concepts.md#recovery) |
| `error: HTTPS_PROXY is "htp:/proxy:80", which is not a proxy address` | set it to an http, https or socks proxy's address, as `http://proxy.example.org:8080`, or unset it on a network without a proxy; fetchloom never connects around a proxy it cannot read | [environment](reference/environment.md) |
| `error: could not reach the proxy at http://127.0.0.1:3128, which HTTPS_PROXY names: it refused the connection` | fix the variable, start the proxy, or unset the variable on a network that needs no proxy; fetchloom stops at once rather than trying again. `fetchloom help environment` lists the proxy variables | [environment](reference/environment.md) |
| `error: the proxy at http://proxy.example.org:8080 asked for a login before it would reach huggingface.co` | the proxy answered 407, wanting a login before it carries any request: for a name and password, put them in the proxy's address, as the help says; a proxy that wants your windows sign-in, by NTLM or Kerberos, needs a local relay that signs in for you | [company proxies](#company-proxies) |
| `error: there is no data.lock, so nothing pins the files' bytes yet` | data.toml was committed without data.lock: ask whoever shares the project to commit it, so you get the bytes they pinned; `fetchloom lock` instead pins what the sources serve today. `status` says `data.lock does not exist` for each dataset | [status](commands/status.md#categories) |
| `error: data.lock names data.lock.d/clean.450ae405bf79.tsv.zst, which is not there` | ask whoever shares the project for the commit that adds data.lock.d, made with `git add data.lock.d`, then pull it. deleting data.lock and running `fetchloom lock` instead pins what the sources serve now and drops every recipe's pin | [big datasets](reference/data-lock.md#big-datasets) |
| `error: iris/iris.csv is not the file data.lock records` | the source serves other bytes now: someone who has the pinned ones, such as whoever pinned them, runs `fetchloom push --to <folder>` into a folder you can read, and `fetchloom sync --mirror <folder>` takes them. if the new bytes are intended, run the `fetchloom lock --upgrade` the help names, sync and commit the lock. the JSON `fix` is null | [sync](commands/sync.md#what-happens) |
| `error: downloading huge.bin needs 2.0 GiB, and fetchloom cannot ask you here` | check the size, then pass `--yes` if you want it. at a terminal with `TERM=dumb`, `FETCHLOOM_ACCESSIBLE=1` asks instead | [approval](concepts.md#approval) |
| `error: SSL_CERT_FILE names /etc/ssl/corp.pem, which holds no certificates fetchloom can read` | unset it to use the system's certificates, or set it to a file of PEM certificates | [environment](reference/environment.md) |
| `error: this sync needs at least 170.0 GiB on the disk holding the cache at /home/ada/.cache/fetchloom, which has 150.0 GiB free`, or `error: downloading iris.zip needs at least 7.9 KiB on the disk holding the cache at ...` part way through a file whose server did not say its size | free space there, or set `FETCHLOOM_CACHE` to a folder on a bigger disk. downloads already in the cache are kept. `--yes` goes on anyway, as on a disk that compresses | [approval](concepts.md#approval) |
| `error: the cache at /media/usb/fetchloom is on a FAT32 disk, which holds no file of 4 GiB or more, and scans.zip is 5.2 GiB` | set `FETCHLOOM_CACHE` to a folder on a disk formatted as exFAT, NTFS, APFS or ext4. a project on a FAT32 disk gets the same refusal for its data folder: move the project | [limits](limits.md) |
| `error: iris/iris.csv already exists with other contents` | move the file somewhere safe, or choose another folder with `--dir` | [existing folders](commands/get.md#existing-folders) |
| `error: data.csv already exists and holds other bytes than the link serves now` | choose another name with `-o`, or add `--force` to replace it | [a file already there](commands/get.md#a-file-already-there) |
| `error: huggingface.co has no dataset scikit-learn/irsi, or it is private`, or signed in, `error: hf:datasets/scikit-learn/irsi does not exist, or is not public` | use the command the help gives for a public dataset a typo away, or check the name at the page it gives; only for a private dataset does signing in help | [when a download is refused](commands/auth.md#when-a-download-is-refused) |
| `error: data.example.org refused the token saved for it (401); it may have expired`, or `data.example.org sent a web page, such as its sign-in page, where a file was expected, so the cookies saved for it may have expired` | run the `fetchloom auth login` the help names, which saves a current key or session for the site, then the command again. a page you want as the data takes `--web-page` | [any website](signing-in.md#any-website) |
| `error: huggingface.co refused the key in HF_TOKEN for hf:datasets/bigcode/the-stack` | set `HF_TOKEN` to a current key from https://huggingface.co/settings/tokens, or unset it to use the key `fetchloom auth login hf` saves. a refused key in the token file gets ``the key may be old or wrong: `fetchloom auth` shows where it comes from and whose it is`` instead | [when a download is refused](commands/auth.md#when-a-download-is-refused) |
| `error: huggingface.co has no public model scikit-learn/iris, but it has a dataset of that name` | use the dataset link the help gives, as `https://huggingface.co/datasets/scikit-learn/iris` | [hugging face](sources/hugging-face.md#references-and-pins) |
| `error: iris has no file or folder called not-a-file.csv` | use one of the paths the error names | [path](commands/path.md#what-happens) |
| `error: kaggle:epa/epa-historical-air-quality cannot be fetched through kaggle's api` | open the dataset's page to see how kaggle offers it, or choose another dataset; a later try gets the same answer | [kaggle](sources/kaggle.md) |

## warnings and unexpected results

| what you see | fix | see |
|---|---|---|
| `fl` runs another program, or an installer said it left `fl` alone | type `fetchloom`, which always works. the installers never replace or hide another `fl`, so for the short name add `alias fl=fetchloom` to your shell's startup file; in powershell, `fl` is the alias of `Format-List` | [short name](windows.md#short-name) |
| sync succeeds but a file is still edited | sync keeps conflicts: inspect `status` and JSON `placement.kept`, and save edits before choosing `--force` | [local changes](commands/sync.md#local-changes) |
| `warning: put back data/pets/images/x.png, which was deleted or moved, since data.lock pins it` | to leave it out for good, run `fetchloom drop <path>`, which takes a folder too; keep files of your own outside the data folder | [local changes](commands/sync.md#local-changes) |
| `warning: exclude "images/x.png" in datasets.pets names a file inside pets.zip, which exclude does not reach` | run the `fetchloom drop` the help names, which writes the archive's members table | [drop](commands/drop.md) |
| `warning: <path> was put back as pinned: it is linked to the cache's copy, which was changed. the changed copy is kept as <path>.changed` | nothing: sync downloaded the file again, and the edit is in the `.changed` file. use `copy = true` for files you edit | [local changes](commands/sync.md#local-changes) |
| `warning: <path> was edited, and the file is shared with other projects, so they see the edit too` | a hardlink shared by several projects changed after the cache lost its own copy: save the edit, then run `fetchloom sync --force` in each project, and use `copy = true` for files you edit | [local changes](commands/sync.md#local-changes) |
| `warning: <path> is a link where data.lock places a file` | move what you need, then run `fetchloom sync --force`, which removes the link itself, never what it points to | [local changes](commands/sync.md#local-changes) |
| `warning: hf:datasets/mozilla-foundation/common_voice_17_0 holds only .gitattributes and README.md, no data files` | the owner moved the data and left a readme saying where: read the dataset's README.md, and if the data is gone, run `fetchloom remove <name>` | [hugging face](sources/hugging-face.md) |
| `warning: data/pets still holds 2 files fetchloom did not place, such as data/pets/notes.txt, so the folder stays` | keep what you need and delete the rest; `fetchloom status` lists them as extra | [folders left behind](commands/sync.md#folders-left-behind) |
| `warning: data still holds 2 files fetchloom placed before data_dir changed to inputs/raw, such as data/iris/iris.csv` | delete the old folder when you no longer need what it holds. files the warning counts as not placed by fetchloom are yours: move them out first | [folders left behind](commands/sync.md#folders-left-behind) |
| `warning: iris was taken, so this one is iris-2` | keep it, or rename `[datasets.iris-2]` in data.toml, and the next sync keeps its pin. `fetchloom remove iris-2`, then an add with `--as <name>`, resolves it again, which may pin newer data | [dataset names](reference/data-toml.md#dataset-names) |
| status exits 0 with differences | inspect every status array, or ask for `status --check` | [status](commands/status.md#exit-codes) |
| an input is read-only | set `copy = true` on the dataset and sync | [placement](concepts.md#placement) |
| another account on the same machine downloads the same data again | each account has its own cache, readable by its owner only: share one among people who trust each other | [one cache for a whole lab](hpc.md#one-cache-for-a-whole-lab) |
| a glob matches no files | quote it and check it against the listed paths; a file inside an archive needs a [members table](reference/data-toml.md#files-inside-archives) | [patterns](reference/data-toml.md#patterns) |
| adding the same reference rejects different filters | edit its entry in `data.toml`, or pass an unused `--as` name | [add](commands/add.md#what-happens) |
| an archive remains packed | its extension or bytes are not ones fetchloom unpacks, or extraction is off | [archives](concepts.md#archives) |
| `warning: sample/sample.rar stays packed, since fetchloom does not unpack rar`, or for a rar under another name, `warning: sample/sample.zip stays packed: its bytes are a rar, which fetchloom does not unpack` | run the command in the `help:` line, which unpacks it into `sample-unpacked` beside it, inside the data folder git ignores; `extract = false` on the dataset keeps it packed without the warning | [archives](reference/compatibility.md#archives) |
| unar prints `Failed! (Attempted to read more data than was available)` for each file of a rar | use unrar, unar 1.10.8 or later, or 7-Zip | [archives](reference/compatibility.md#archives) |
| `bsdtar: RAR solid archive support unavailable`, or `Unsupported block header size` | use unrar, which reads the rars libarchive refused | [archives](reference/compatibility.md#archives) |
| `error: the archive is cut short: a zip lists its files at its end, and that list is missing`, or for a tar, `error: the archive is damaged: it ends in the middle of a file` | ask the dataset's authors for a whole copy; until then `extract = false` on the dataset, or `--no-extract`, keeps the bytes as they are | [archives](concepts.md#archives) |
| an archive has no dataset folder | it holds no regular files: set `extract = false` and sync to keep it packed | [archives](concepts.md#archives) |
| a source refuses access | [sign in](signing-in.md) where the source takes a key, or check its page under [sources](sources/README.md) | [signing in](signing-in.md) |
| sync warns it could not add the project to the cache's list of projects | fix the cache folder's permissions and sync again before relying on `cache gc` to keep the project's files | [gc](commands/cache.md#gc) |
| `cache gc` exits 50 after partial cleanup | read the causes in `result.not_removed`, fix access or storage, and run it again | [gc](commands/cache.md#gc) |
| `cache verify` exits 30 | the changed bytes download again at the next online sync; then sync the affected projects | [verify](commands/cache.md#verify) |
| `cache verify` exits 50 without changed bytes | read the causes in `result.unreadable`, fix access or storage, and verify again | [verify](commands/cache.md#verify) |
| a windows file cannot be replaced or deleted | close the program that holds it open, or fix its permissions, and retry | [paths and open files](windows.md#paths-and-open-files) |
| a long path fails | follow the hint for a shorter project path, and use shorter project and dataset folders, since a filesystem or another program may have a shorter limit | [paths and open files](windows.md#paths-and-open-files) |
| plain-word add or get refuses to guess | without a terminal nothing is chosen: rerun with a returned id; `--yes` never selects one | [choosing a dataset](commands/search.md#choosing-a-dataset) |
| an option is rejected before a command | put a command's own options after it: `fetchloom add --as flowers zenodo:3608990`, not `fetchloom --as flowers add zenodo:3608990` | [global options](commands/README.md#global-options) |
| a download warns that it is a web page | the link names a page rather than data: find the source's download or raw link | [URLs and DOIs](sources/urls-and-dois.md#accepted-references) |
| a download warns that it came compressed with gzip | the server compressed it on its way, with `Content-Encoding`, and fetchloom keeps the bytes as sent, which a published SHA-256 matches: decompress it with `gzip -dc`, or read it as gzip, as pandas does with `compression="gzip"` | [one file](commands/get.md#one-file) |

## company proxies

fetchloom sends its requests through the proxy that `HTTPS_PROXY` or `HTTP_PROXY` names, or `ALL_PROXY` for both; [environment](reference/environment.md) lists them. on windows and macos, the system's proxy settings fill in when those variables are unset. a proxy that cannot be reached stops the run within seconds, naming its address and the variable that set it, rather than being tried again. a proxy that inspects https needs its certificate in the system's certificate store, as the certificate error says.

a proxy that asks for your windows sign-in, by NTLM or Kerberos, refuses every request until it gets that sign-in, which fetchloom cannot send. fetchloom then stops at the first answer, with `the proxy at <proxy> asked for a login before it would reach <host>`, exit 20, `network.unreachable`, since no other try would get further. `curl -v -x http://proxy.example.org:8080 https://huggingface.co`, with your proxy's address, shows `407 Proxy Authentication Required` and a `Proxy-Authenticate` header naming `NTLM` or `Negotiate`. a local relay can sign in for you: [px](https://github.com/genotrance/px) uses your windows sign-in, and [cntlm](https://github.com/versat/cntlm) the user name and password in its settings. both take requests from this machine on port 3128 by default, so point fetchloom there:

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

- restore a broken lock from git when you need its exact pins; deleting it and running `fetchloom lock` can resolve newer inputs. a lock with a higher `version` comes from a newer fetchloom and needs one.
- a failed `add` puts `data.toml` and `data.lock` back. if that fails, copy them back from the recovery file the error names under `.fetchloom/`, which holds their bytes as they were; never paste it into a log, since it may hold credentials.
- a `remove` that changed `data.toml` and `data.lock` but could not delete files finishes with `fetchloom sync` once you fix the reported error.
- a command that was killed can leave a record that it holds the project. delete only the exact path a waiting command names, and only once the process it names has ended, as [owner record](reference/project-folder.md#owner-record) explains.

### a project too big for its container

in a container with a memory limit, fetchloom compares what reading a project's files takes, about 650 bytes a file data.lock pins, with what the limit leaves, and stops before reading them when that is too little, with `local.io`, exit 50. in a container limited to 64 MiB, the status of a project of 100,000 files says:

```text
error: reading this project's 100,000 files takes about 70.0 MiB of memory, more than the 60.6 MiB this container leaves fetchloom
  help: raise the container's memory limit by 9.3 MiB or more, or name one dataset, as in `fetchloom path <name>`, which reads only its files
```

`fetchloom path <name>` and a sync of named datasets already in place read only those datasets' pins and records, so they still answer there. memory a system has available outside a container is not checked: past it the system pages memory out rather than ending the run.

### the system cannot start another worker

before it starts, fetchloom checks that the system lets it start a thread, and stops with `local.io`, exit 50, when it does not. lower `FETCHLOOM_THREADS`, or raise the process's or container's thread limit, then retry. this is a resource limit, not a damaged cache.
