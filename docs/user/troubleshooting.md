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

credentials are logged by host and header, not by value. URL logins and recognized secret fields show as `***`; arbitrary secret fields cannot be identified reliably. the log also names local paths, such as the project folder, dataset references, and every link fetchloom asked for with its query, search words included, so read the file before sharing it. [what leaves your machine](concepts.md#what-leaves-your-machine) lists what fetchloom itself sends. `FETCHLOOM_LOG=fetchloom::net=debug` limits the log to one part of fetchloom when the full log is too long.

## errors

| symptom on stderr | cause | fix |
|---|---|---|
| `fetchloom: command not found`, or `fetchloom is not recognized` on windows | the terminal started before rust added its folder to PATH | open a new terminal; if that fails, check `~/.cargo/bin` (`%USERPROFILE%\.cargo\bin` on windows) holds fetchloom |
| `error: there is no command called "init"` | no setup command is needed | run `fetchloom add <link>` in the project folder |
| `error: data.toml has an unfinished git merge conflict`, or `data.lock has ...` | a git merge left both sides of a change between its `<<<<<<<`, `=======` and `>>>>>>>` lines | run `fetchloom lock`, which merges both files without downloading; where both sides changed one dataset, it asks for `--ours <name>` or `--theirs <name>`, as [after a git merge](commands/lock.md#after-a-git-merge) says |
| `error: there is no data.toml in this folder or any folder above it inside the git repository at /home/ada/thesis/experiment` | the folder is in a git repository of its own, and the search for a project stops at its top | run `fetchloom add <link>` to start a project there, or name the folder of the project above it with `-C`, as in `fetchloom -C .. sync`; see [finding the project](reference/project-folder.md#finding-the-project) |
| `error: <link> is missing` or `error: <link>... is missing` | `add` or `get` needs a reference | pass a dataset link, or a list to `get -i`; use `fetchloom get --help` for syntax |
| `error: offline, and 1 file data.lock names is not in the cache` | an offline project needs bytes neither the cache nor the data folder holds | sync online first; remove `--offline` and unset `FETCHLOOM_OFFLINE` if it is `1`. a CI job can restore a cache that holds them, as [CI](ci.md) shows |
| `error: 1 file data.lock names is on no mirror, and mirrors_only lets fetchloom ask its mirrors alone` | [mirror-only mode](mirrors.md#mirror-only-mode) is on, and no mirror holds the file, as the details say for each mirror | ask whoever keeps the mirror to add it; a fetchloom on a machine that may ask sources can sync the project and fill the mirror with [`fetchloom push`](commands/push.md) |
| `error: gave up on huggingface.co after 5 tries` | the network or the site stayed down longer than the retries wait | run the same command when the network is back; downloaded bytes are kept and the next run goes on from them where the server allows it, as the line under the error says. on a network that drops for minutes, set `FETCHLOOM_RETRIES=10` |
| `Restarting big.bin · the 2.0 MiB an earlier run kept cannot be continued` | the server sends no parts of a file, as python's `http.server` sends none, or nothing that proves a part belongs with the bytes kept, or the file changed since | nothing; the file downloads again from its first byte. a stop against such a server says so, as `the server cannot continue from ... so the next run starts over` |
| `error: HTTPS_PROXY is "htp:/proxy:80", which is not a proxy address` | the variable holds a typo, or a scheme other than http, https or socks, which the network layer would pass over and connect without the proxy | set it to the proxy's address, as `http://proxy.example.org:8080`, or unset it on a network without a proxy; [environment](reference/environment.md) lists the proxy variables |
| `error: could not reach the proxy at http://127.0.0.1:3128, which HTTPS_PROXY names: it refused the connection` | the variable names a proxy that is not running, a closed port, or a name that does not resolve; fetchloom stops at once rather than trying again | fix the variable, start the proxy, or unset the variable on a network that needs no proxy. `fetchloom help environment` lists the proxy variables |
| `error: the proxy at http://proxy.example.org:8080 asked for a login before it would reach huggingface.co` | the proxy answered 407: it wants a login before it carries any request. one that asks for your windows sign-in, by NTLM or Kerberos, wants what fetchloom cannot send | for a windows sign-in, run a local relay that signs in for you, as [company proxies](#company-proxies) shows; for a name and password, put them in the proxy's address, as the help says |
| `error: there is no data.lock, so nothing pins the files' bytes yet` | the project's data.toml was committed without its data.lock, so a clone has no pins | ask whoever shares the project to commit data.lock, so you get the bytes they pinned; `fetchloom lock` instead pins what the sources serve today. `status` says `data.lock does not exist` for each dataset |
| `error: data.lock names data.lock.d/clean.450ae405bf79.tsv.zst, which is not there` | the commit you cloned has data.lock but not the new list in data.lock.d it names, as `git commit -a` leaves new files out | ask whoever shares the project for the commit that adds data.lock.d, made with `git add data.lock.d`, then pull it. deleting data.lock and running `fetchloom lock` instead pins what the sources serve now and drops every recipe's pin |
| `error: iris/iris.csv is not the file data.lock records` | the source now serves other bytes than data.lock pins | keep the pinned bytes from a [folder mirror](mirrors.md) that holds them; upgrade only if the change is intended, as below |
| `error: downloading huge.bin needs 2.0 GiB, and fetchloom cannot ask you here` | a large download needs a yes, and no one is at a terminal to answer | check the size, then pass `--yes` if you want it |
| `error: SSL_CERT_FILE names /etc/ssl/corp.pem, which holds no certificates fetchloom can read` | the variable names a missing file, or one without PEM certificates | unset it to use the system's certificates, or set it to a file of PEM certificates |
| `error: this sync needs at least 170.0 GiB on the disk holding the cache at /home/ada/.cache/fetchloom, which has 150.0 GiB free`, or `error: downloading iris.zip needs at least 7.9 KiB on the disk holding the cache at ...` part way through a file whose server did not say its size | the downloads, and the archives they unpack, need more than the cache's disk has free | free space there, or set `FETCHLOOM_CACHE` to a folder on a bigger disk. downloads already in the cache are kept. `--yes` goes on anyway, as on a disk that compresses |
| `error: the cache at /media/usb/fetchloom is on a FAT32 disk, which holds no file of 4 GiB or more, and scans.zip is 5.2 GiB` | FAT32 keeps a file's size in 32 bits, so the file cannot be stored there; fetchloom refuses before downloading, copying or unpacking it | set `FETCHLOOM_CACHE` to a folder on a disk formatted as exFAT, NTFS, APFS or ext4. a project on a FAT32 disk gets the same refusal for its data folder: move the project |
| `error: iris/iris.csv already exists with other contents` | `get` found conflicting local bytes | move the file somewhere safe or choose another folder with `--dir` |
| `error: data.csv already exists and holds other bytes than the link serves now` | `get -o` never replaces a file without being asked | choose another name with `-o`, or add `--force` to replace it |
| `error: huggingface.co has no dataset scikit-learn/irs, or it is private` | the name has a typo, or the dataset is private: the hub answers both alike | check the name at the page the help gives. only for a private dataset does signing in help, with an account that can read it; at a terminal the offer to sign in defaults to no |
| `error: huggingface.co refused the key in HF_TOKEN for hf:datasets/bigcode/the-stack` | `HF_TOKEN` holds an old or wrong key, which goes with every request to huggingface.co | set `HF_TOKEN` to a current key from https://huggingface.co/settings/tokens, or unset it to use the key `fetchloom auth login hf` saves. a refused key in the token file gets `sign in again with fetchloom auth login hf` instead |
| `error: huggingface.co has no public model scikit-learn/iris, but it has a dataset of that name` | a link without `/datasets/` names a model | use the dataset link the help gives, as `https://huggingface.co/datasets/scikit-learn/iris` |
| `error: dataset has no file or folder called not-a-file.csv` | the requested member is absent | use one of the dataset paths named in the error details |
| `error: kaggle:epa/epa-historical-air-quality cannot be fetched through kaggle's api` | kaggle lists the dataset, but its api sends none of its files, which a later try does not change | open the dataset's page to see how kaggle offers it, or choose another dataset |

for a size mismatch, the full stderr answer can be:

```text
error: iris/iris.csv is not the file data.lock records
  expected 32 bytes, got 15
  help: to keep the pinned bytes, someone who has them runs `fetchloom push --to <folder>`, then you run `fetchloom sync --mirror <folder>`. if the new bytes are intended, run `fetchloom lock --upgrade iris`
```

fetchloom places nothing it cannot check, so the clone stays without the file. to keep the pinned bytes, someone who has them, such as whoever pinned them, runs `fetchloom push --to <folder>` into a folder you can read, then `fetchloom sync --mirror <folder>` takes them from there, checked against data.lock. if the new input is intended, run the suggested upgrade, sync and commit the lock. with `--json` the error's `fix` is null, since only a person may choose other bytes than data.lock pins.

## warnings and unexpected results

| symptom | cause | fix |
|---|---|---|
| `fl` runs another program, or an installer said it left `fl` alone | another program here is already called `fl`, and the installers never replace or hide it. in powershell, `fl` is the alias of `Format-List`, which runs before any program | type `fetchloom`, which always works. for the short name in your own shell, add `alias fl=fetchloom` to its startup file; in powershell, [short name](windows.md#short-name) shows the alias |
| sync succeeds but a file is still edited | conflicts are preserved by default | inspect `status` and JSON `placement.kept`; save edits before choosing `--force` |
| `warning: put back data/pets/images/x.png, which was deleted or moved, since data.lock pins it` | you deleted or moved a file fetchloom placed, and data.lock still pins it | to leave it out for good, run [`fetchloom drop <path>`](commands/drop.md); keep files of your own outside the data folder |
| `warning: exclude "images/x.png" in datasets.pets names a file inside pets.zip, which exclude does not reach` | a dataset's `exclude` chooses among the files its source lists, and the archive is one file to it | run the `fetchloom drop` command the help names; it writes the archive's [members table](reference/data-toml.md#files-inside-archives) |
| `warning: <path> was put back as pinned: it is linked to the cache's copy, which was changed. the changed copy is kept as <path>.changed` | an edit made in place through a hardlink changed the cache's copy, which every project using the file shares | nothing; sync downloaded the file again, and the edit is in the `.changed` file. use `copy = true` for files you edit |
| `warning: <path> was edited, and the file is shared with other projects, so they see the edit too` | a hardlink shared by several projects changed after the cache lost its own copy | save the edit, then run `fetchloom sync --force` in each project; use `copy = true` for files you edit |
| `warning: <path> is a link where data.lock places a file` | a link, or another entry, stands where a pinned file belongs | move what you need, then run `fetchloom sync --force`; it removes the link itself, never what it points to |
| `warning: hf:datasets/mozilla-foundation/common_voice_17_0 holds only .gitattributes and README.md, no data files` | the owner moved the data elsewhere and left a readme saying where | read the dataset's README.md; if the data is gone, run `fetchloom remove <name>` |
| `warning: data/pets still holds 2 files fetchloom did not place, such as data/pets/notes.txt, so the folder stays` | the dataset left that folder, by `remove` or a changed `path`, and fetchloom deletes only unchanged files it placed | keep what you need and delete the rest; `fetchloom status` lists them as extra |
| `warning: data still holds 2 files fetchloom placed before data_dir changed to inputs/raw, such as data/iris/iris.csv` | `data_dir` changed, and sync never deletes outside the data folder | delete the old folder when you no longer need what it holds; sync says this once. when the warning also counts files it did not place, those are yours: move them out first |
| `warning: iris was taken, so this one is iris-2` | the project already has a dataset with that name | keep it, or rename it: change `[datasets.iris-2]` in data.toml to the name you want, and the next sync keeps its pin, as [dataset names](reference/data-toml.md#dataset-names) says. `fetchloom remove iris-2`, then an add with `--as <name>`, resolves it again, which may pin newer data |
| status exits 0 with differences | differences are a successful result | inspect every status array |
| an input is read-only | default placement protects shared bytes | set `copy = true` on the dataset and sync |
| another account on the same machine downloads the same data again | each account has its own cache, readable by its owner only | share one cache among people who trust each other, as [one cache for a whole lab](hpc.md#one-cache-for-a-whole-lab) shows |
| a glob matches no files | it differs from listed paths, or names a file inside an archive, which only a members table reaches | quote the glob and check [pattern rules](reference/data-toml.md#patterns); for files inside an archive, see [files inside archives](reference/data-toml.md#files-inside-archives) |
| adding the same reference rejects different filters | the existing entry already owns that reference and settings | edit `data.toml` or pass an unused `--as` name |
| an archive remains packed | the extension or bytes are unsupported, or extraction is disabled | check [archive rules](concepts.md#archives) |
| `warning: sample/sample.rar stays packed, since fetchloom does not unpack rar`, or for a rar under another name, `warning: sample/sample.zip stays packed: its bytes are a rar, which fetchloom does not unpack` | fetchloom reads zip, 7z and tar, not rar, and the bytes choose the decoder | run the command in the `help:` line under it. it unpacks the archive into `sample-unpacked` beside it, inside the data folder git ignores, with unrar on linux, unar on macos or 7-Zip on windows. `extract = false` on the dataset keeps it packed without the warning |
| unar prints `Failed! (Attempted to read more data than was available)` for each file of a rar | unar 1.10.1, which ubuntu 24.04 packages, failed this way on a solid rar5 that unrar 7.00, unar 1.10.8 and 7-Zip 26.06 unpacked to the same bytes | use unrar, as the help on linux says, unar 1.10.8 or later, or 7-Zip from [7-zip.org](https://www.7-zip.org/download.html): `7zz x data/sample/sample.rar -odata/sample/sample-unpacked` |
| `bsdtar: RAR solid archive support unavailable`, or `Unsupported block header size` | libarchive, behind `bsdtar` and macos's `tar`, reads only some rar archives: version 3.7.2 refused a solid rar4, a solid rar2, rar5 files with delta or x86 filters and a rar5 split in volumes | use unrar, which read them all |
| `error: the archive is cut short: a zip lists its files at its end, and that list is missing` | the zip the source serves ends early, as an upload stopped part way leaves one | ask the dataset's authors for a whole copy; until then `extract = false` on the dataset, or `--no-extract`, keeps the bytes as they are |
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
| a download warns that it came compressed with gzip | the server compressed the file on its way, with `Content-Encoding`, and fetchloom keeps the bytes as sent, which is what a published SHA-256 matches | decompress it with `gzip -dc`, or read it as gzip, as pandas does with `compression="gzip"` |

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

restore a broken lock from git when you need its exact pins. deleting it and running `fetchloom lock` can resolve newer inputs. a lock with a higher `version` comes from a newer fetchloom and needs one.

a failed `add` attempts to restore both `data.toml` and `data.lock`. if either restoration fails, restore the metadata from the named recovery file under `.fetchloom/`. it holds the original bytes, not a JSON string; do not paste its contents into logs, since they may contain credentials. cached downloads and files already placed can remain.

if `remove` changed metadata but could not delete files, fix the reported local error and run sync to finish removal.

an interrupted project command can leave an owner record. only delete the exact owner path named by a waiting command after confirming that its owning process has ended. records from another operating system are never reclaimed by age or pid. [project state](reference/project-folder.md#owner-record) explains the locking rules.

### the system cannot start another worker

local placement, cache verification and archive work fall back to the calling thread when the OS refuses a worker. before starting the async runtime, fetchloom probes whether a thread can be created. a refused probe reports `local.io` (exit 50), including in JSON. the probe does not reserve resources for later runtime workers. reduce `FETCHLOOM_THREADS` or raise the process or container's thread limit, then retry. this is a resource failure, not a corrupt cache.
