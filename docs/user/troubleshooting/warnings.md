# Warnings and unexpected results

| what you see | fix | see |
|---|---|---|
| `fl` runs another program, or an installer said it left `fl` alone | type `fetchloom`, which always works. the installers never replace or hide another `fl`, so for the short name add `alias fl=fetchloom` to your shell's startup file; in powershell, `fl` is the alias of `Format-List` | [short name](../guides/windows.md#short-name) |
| sync succeeds but a file is still edited | sync keeps conflicts: inspect `status` and JSON `placement.kept`, and save edits before choosing `--force` | [local changes](../commands/sync.md#local-changes) |
| `warning: put back data/pets/images/x.png, which was deleted or moved, since data.lock pins it` | to leave it out for good, run `fetchloom drop <path>`, which takes a folder too; keep files of your own outside the data folder | [local changes](../commands/sync.md#local-changes) |
| `warning: exclude "images/x.png" in datasets.pets names a file inside pets.zip, which exclude does not reach` | run the `fetchloom drop` the help names, which writes the archive's members table | [drop](../commands/drop.md) |
| `warning: <path> was put back as pinned: it is linked to the cache's copy, which was changed. the changed copy is kept as <path>.changed` | nothing: sync downloaded the file again, and the edit is in the `.changed` file. use `copy = true` for files you edit | [local changes](../commands/sync.md#local-changes) |
| `warning: <path> was edited, and the file is shared with other projects, so they see the edit too` | a hardlink shared by several projects changed after the cache lost its own copy: save the edit, then run `fetchloom sync --force` in each project, and use `copy = true` for files you edit | [local changes](../commands/sync.md#local-changes) |
| `warning: <path> is a link where data.lock places a file` | move what you need, then run `fetchloom sync --force`, which removes the link itself, never what it points to | [local changes](../commands/sync.md#local-changes) |
| `warning: hf:datasets/mozilla-foundation/common_voice_17_0 holds only .gitattributes and README.md, no data files` | the owner moved the data and left a readme saying where: read the dataset's README.md, and if the data is gone, run `fetchloom remove <name>` | [hugging face](../sources/hugging-face.md) |
| `warning: data/pets still holds 2 files fetchloom did not place, such as data/pets/notes.txt, so the folder stays` | keep what you need and delete the rest; `fetchloom status` lists them as extra | [folders left behind](../commands/sync.md#folders-left-behind) |
| `warning: data still holds 2 files fetchloom placed before data_dir changed to inputs/raw, such as data/iris/iris.csv` | delete the old folder when you no longer need what it holds. files the warning counts as not placed by fetchloom are yours: move them out first | [folders left behind](../commands/sync.md#folders-left-behind) |
| `warning: iris was taken, so this one is iris-2` | keep it, or rename `[datasets.iris-2]` in data.toml, and the next sync keeps its pin. `fetchloom remove iris-2`, then an add with `--as <name>`, resolves it again, which may pin newer data | [dataset names](../reference/data-toml.md#dataset-names) |
| status exits 0 with differences | inspect every status array, or ask for `status --check` | [status](../commands/status.md#exit-codes) |
| an input is read-only | set `copy = true` on the dataset and sync | [placement](../concepts/cache-and-placement.md#placement) |
| another account on the same machine downloads the same data again | each account has its own cache, readable by its owner only: share one among people who trust each other | [one cache for a whole lab](../guides/clusters.md#one-cache-for-a-whole-lab) |
| a glob matches no files | quote it and check it against the listed paths; a file inside an archive needs a [members table](../reference/data-toml.md#files-inside-archives) | [patterns](../reference/data-toml.md#patterns) |
| adding the same reference rejects different filters | edit its entry in `data.toml`, or pass an unused `--as` name | [add](../commands/add.md#what-happens) |
| an archive remains packed | its extension or bytes are not ones fetchloom unpacks, or extraction is off | [archives](../reference/archives.md) |
| `warning: sample/sample.rar stays packed, since fetchloom does not unpack rar`, or for a rar under another name, `warning: sample/sample.zip stays packed: its bytes are a rar, which fetchloom does not unpack` | run the command in the `help:` line, which unpacks it into `sample-unpacked` beside it, inside the data folder git ignores; `extract = false` on the dataset keeps it packed without the warning | [archives](../reference/archives.md) |
| unar prints `Failed! (Attempted to read more data than was available)` for each file of a rar | use unrar, unar 1.10.8 or later, or 7-Zip | [archives](../reference/archives.md) |
| `bsdtar: RAR solid archive support unavailable`, or `Unsupported block header size` | use unrar, which reads the rars libarchive refused | [archives](../reference/archives.md) |
| `error: the archive is cut short: a zip lists its files at its end, and that list is missing`, or for a tar, `error: the archive is damaged: it ends in the middle of a file` | ask the dataset's authors for a whole copy; until then `extract = false` on the dataset, or `--no-extract`, keeps the bytes as they are | [archives](../reference/archives.md) |
| an archive has no dataset folder | it holds no regular files: set `extract = false` and sync to keep it packed | [archives](../reference/archives.md) |
| a source refuses access | [sign in](../guides/sign-in.md) where the source takes a key, or check its page under [sources](../sources/README.md) | [signing in](../guides/sign-in.md) |
| sync warns it could not add the project to the cache's list of projects | fix the cache folder's permissions and sync again before relying on `cache gc` to keep the project's files | [gc](../commands/cache.md#gc) |
| `cache gc` exits 50 after partial cleanup | read the causes in `result.not_removed`, fix access or storage, and run it again | [gc](../commands/cache.md#gc) |
| `cache verify` exits 30 | the changed bytes download again at the next online sync; then sync the affected projects | [verify](../commands/cache.md#verify) |
| `cache verify` exits 50 without changed bytes | read the causes in `result.unreadable`, fix access or storage, and verify again | [verify](../commands/cache.md#verify) |
| a windows file cannot be replaced or deleted | close the program that holds it open, or fix its permissions, and retry | [paths and open files](../guides/windows.md#paths-and-open-files) |
| a long path fails | follow the hint for a shorter project path, and use shorter project and dataset folders, since a filesystem or another program may have a shorter limit | [paths and open files](../guides/windows.md#paths-and-open-files) |
| plain-word add or get refuses to guess | without a terminal nothing is chosen: rerun with a returned id; `--yes` never selects one | [choosing a dataset](../commands/search.md#choosing-a-dataset) |
| an option is rejected before a command | put a command's own options after it: `fetchloom add --as flowers zenodo:3608990`, not `fetchloom --as flowers add zenodo:3608990` | [global options](../commands/global-options.md) |
| a download warns that it is a web page | the link names a page rather than data: find the source's download or raw link | [URLs and DOIs](../sources/urls-and-dois.md#accepted-references) |
| a download warns that it came compressed with gzip | the server compressed it on its way, with `Content-Encoding`, and fetchloom keeps the bytes as sent, which a published SHA-256 matches: decompress it with `gzip -dc`, or read it as gzip, as pandas does with `compression="gzip"` | [one file](../commands/get.md#one-file) |

## common mistakes

### lock

| symptom | cause | fix |
|---|---|---|
| `data/` has old files | lock does not place files | run `fetchloom sync` |
| an explicit version did not advance | the reference still names that version | edit `ref` deliberately, then lock |
| `--check` passes despite edited data | it checks specs, not contents | use `status`; use `cache verify` for cached objects |
| rebuilding a broken lock changes inputs | fresh resolution can choose newer data | restore the original lock from git when those pins matter |

### sync

| symptom | cause | fix |
|---|---|---|
| the lock changed unexpectedly | sync updates stale locks | use `sync --locked` in a job that requires committed pins |
| `warning: exclude "images/x.png" in datasets.pets names a file inside pets.zip, which exclude does not reach` | a dataset's `exclude` chooses among the files its source lists, and the archive is one of them. sync warns when it lists the source again and the pattern matches none of those files | run the [`fetchloom drop`](../commands/drop.md) the help names, which writes the archive's [members table](../reference/data-toml.md#files-inside-archives); a dry run warns the same way |

### status

| symptom | cause | fix |
|---|---|---|
| `warning: data.lock no longer names data.lock.d/clean.450ae405bf79.tsv.zst, so nothing reads it` | a list a build or a big dataset wrote is left after `git checkout` or a reset put an older data.lock back | run the delete or `git rm` its help names; never `git add data.lock.d` before that |
| `warning: data.lock names data.lock.d/clean.450ae405bf79.tsv.zst, which git does not have` | a build or a big dataset wrote a new list, and `git commit -a` left it out | run `git add data.lock.d`, then commit |
| `warning: git does not have prep.py, code of clean` | a recipe's code was never added to git, so `git commit -a` left it out | run the `git add` the help names, then commit |

### path

| symptom | cause | fix |
|---|---|---|
| an empty archive has no dataset folder | extraction produced no regular files | set `extract = false` and sync to keep the packed archive |
| a notebook needs a filesystem object | CLI output is text | use the [python helper](../reference/python-helper.md) for `pathlib.Path` |

### get

| symptom | cause | fix |
|---|---|---|
| the downloaded archive stayed packed | its name or bytes are unsupported | check [archive rules](../reference/archives.md) |

### cache

| symptom | cause | fix |
|---|---|---|
| `get` data is collected | one-off downloads do not register a project | use a project for inputs whose cache retention matters |
| `gc --inputs` keeps raw data with `no_copy` or `no_mirror` | the mirror lacks it, or gc was not told of the mirror | run `fetchloom push --inputs <recipe> --to <folder>`, then name that folder with `--mirror` or the `mirrors` setting |

### push

| symptom | cause | fix |
|---|---|---|
| `holds a damaged copy of <file>, so it was not used` | a mirror file has the right size but other bytes, which push skips as already there | delete that file under the mirror's `objects/`, then push again |
