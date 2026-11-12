# add

`add` puts a dataset in your manifest, pins its files and syncs the whole project.

```sh
fetchloom add iris    # at a terminal, pick one from a list
fetchloom add https://huggingface.co/datasets/scikit-learn/iris
fetchloom add https://zenodo.org/records/3608990 --as flowers
fetchloom add hf:datasets/scikit-learn/iris --select "*.csv" --dry-run
fetchloom add ./flowers    # files on this machine, pinned by their bytes
fetchloom add -i links.txt    # a dataset for each link in a list
```

## usage

```sh
fetchloom add [options] <link>
fetchloom add [options] <path>
fetchloom add [options] -i <file>
```

| option | effect |
|---|---|
| `--as <name>` | choose the dataset name and its default folder under `data/` |
| `--select <glob>` | keep files matching any supplied pattern; repeat for more patterns |
| `--exclude <glob>` | drop matching files; repeat for more exclusions |
| `--no-extract` | keep archives packed and record `extract = false`; webdataset loaders read `.tar` shards packed |
| `-i <file>`, `--input-file <file>` | add a dataset for each link in a file, one per line, each with an optional SHA-256, or in stdin for `-`; `--as`, `--select` and `--exclude` do not apply |
| `-n`, `--dry-run` | plan the addition without applying project or cache changes |

[global options](README.md#global-options) also apply. [sources](../sources/README.md) lists accepted links and references. plain words use [catalog search and selection](search.md#choosing-a-dataset); quote several words, as in `fetchloom add "fashion mnist"`. the manifest stores the chosen canonical id, not the search words.

## what happens

1. find the nearest project manifest. without one, the current folder becomes a new project, and a `Started a project in <folder>` line says so at the end. an add that waited while another add started the project there adds to that project, without the line. in your home folder or at the root of a disk, where a project would take in every folder below it, `add` asks first; without a terminal it refuses unless `--yes` is given.
2. resolve the reference, filter its listed files and choose a dataset name. a `Resolved` line on stderr names what the reference resolved to and counts the files kept, as `1 of 4 files` with their size, when a pasted file link, `--select` or `--exclude` keeps fewer files than the source lists. when the files that would unpack include at least 8 numbered `.tar` shards in one folder, a hint suggests `--no-extract` before anything downloads; [webdataset shards](../concepts.md#webdataset-shards) explains why.
3. download every file the whole project lacks into the cache and learn each hash, with the project still as it was.
4. write the entry in `data.toml`, pin selected files in `data.lock` and place the project's files from the cache. the entry goes before the dataset whose name follows its own, as `data.lock` keeps them in name order, so two people adding different datasets rarely change the same lines.
5. in a git repository, add the data folder to `.gitignore` if needed.

run in a folder below a project's folder, `add` works on that project. a person at a terminal sees `using the project in <folder>` once, and the folder the dataset's files are in is given from where you stand, such as `../data/iris`. the search stops at a git repository, so inside a repository with no `data.toml` of its own, `add` starts a project in the current folder even when a folder above the repository holds one; [finding the project](../reference/project-folder.md#finding-the-project) says why.

there is no `init` command; `add` starts a project. `fetchloom init` is a usage error, exit 2, whose help says no init is needed and names `fetchloom add <link>`.

stderr for a single-file addition in a new project, matching a test snapshot. `[time]` replaces the measured duration and `[sandbox]` the test's folder:

```text
 Downloaded iris.csv · 32 B in [time]
    Started a project in [sandbox]/work
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

commit `data.toml` and `data.lock`, not `data/`. names follow the [manifest rules](../reference/data-toml.md). a zenodo file-page link selects that file unless you supply `--select`.

a link you type may be on this machine or your network, such as a lab server: `add` reaches its origin, the scheme, host and port, for that run alone. a later run that downloads from it, here or in a clone, is refused with `network.refused`, exit 20, until the [`allowed_origins`](../reference/environment.md#settings-file) setting lists that origin, so the add ends with a hint that quotes the line to write. from a real run, on stderr, with the settings file's folder shortened to `~`:

```text
  hint: a later run downloads from http://127.0.0.1:19291, a private address, only once allowed_origins lists it: put `allowed_origins = ["http://127.0.0.1:19291"]` in ~/.config/fetchloom/settings.toml, or set `FETCHLOOM_ALLOWED_ORIGINS=http://127.0.0.1:19291` for one run
```

adding an existing reference with identical filters and extraction settings syncs it without another entry, and in a git repository adds the data folder to `.gitignore` if needed. a resolved reference from an old lock identifies that entry only while its manifest reference is unchanged; editing `ref` does not make the old source count as already added. different settings fail unless you supply an unused `--as` name. edit `data.toml` to change an existing entry.

if a new addition fails to sync, fetchloom attempts to restore both `data.toml` and `data.lock`, including their absence before the command. it puts back only a file the add changed: an add that failed before changing either file reports no rollback. before changing an existing manifest or lock, fetchloom saves its exact bytes in a recovery file under `.fetchloom/`. if restoration fails, the error names that file without printing its contents, which may contain credentials. recovery files are removed after success or complete rollback and retained after incomplete rollback. on unix they are created readable only by the owner; on windows they inherit the metadata folder's permissions. cached downloads stay, so running the add again reuses them, while the files it placed for the new dataset are removed with it.

## files on this machine

a path, such as `./flowers`, `../shared/flowers`, `/data/flowers`, `~/flowers`, `C:\data\flowers` or a `file:` link, imports the files there: a folder's every plain file, or one file. data.toml gets `ref = "local"`, never the path, which would mean nothing on a teammate's machine and would say where your files are. data.lock pins each file by its path below the folder, its size and its SHA-256, with an empty `url`, since no source holds them. a plain word that names a folder here, such as `iris` beside an `iris` folder, is not imported: it exits 2 and says to write `./iris`.

the files are copied into the cache and hashed as they are copied, several at once as placing works, which [`FETCHLOOM_THREADS`](../reference/environment.md) can cap. a bar counts the bytes copied; once every file is copied, a `Finishing` line, such as `Finishing 20,000 files`, stays until the cache holds them all, since writing many small files out to the disk can take seconds more. on linux, a disk that clones files, such as btrfs or xfs, gets a clone that shares the file's blocks; elsewhere the cache holds a copy, so on ext4 the files take their room twice until you remove the folder. the files themselves are only read: never changed, moved or linked, since a link to the cache's copy would make your file read-only. an import bigger than the free space on the cache's disk asks first, as a big download does, and `--yes` goes on, as on a disk that clones the files. the cache is yours alone unless you made it [one cache for a whole lab](../hpc.md#one-cache-for-a-whole-lab), where everyone who reads it can read what you import. stderr from a project whose data.toml already existed:

```text
   Imported flowers · 2 files · 32 B, copied into the cache; ../flowers is unchanged
      Added flowers, its files are in data/flowers
  hint: scripts can find it with `fetchloom path flowers`
```

below the folder, `.git`, `.hg`, `.svn` and `.fetchloom` are left out unseen. what a system writes into folders by itself is no data, so it is left out too, and one `Skipped` line names it: finder's `.DS_Store`, the `._` files a mac leaves beside each file on a disk not its own, `__MACOSX`, the `.Spotlight-V100`, `.Trashes`, `.fseventsd`, `.TemporaryItems` and `.Trash-<uid>` folders of removable disks, and windows' `Thumbs.db`, `ehthumbs.db`, `desktop.ini`, `$RECYCLE.BIN` and `System Volume Information`. a folder supplied after finder wrote more of them still matches its pins. links and special files, such as named pipes, are left out with a warning and never followed. names are checked as an archive's members are, before anything is copied: a name with a control character or a `\`, one longer than 4096 bytes, one this machine cannot store, or two names that are one on disks that ignore case or unicode composition, stop the import with `source.invalid`, exit 10. `--select` and `--exclude` match the paths below the folder, and a select pattern that keeps nothing is a usage error. a folder holding more files than `most_files`, 100,000 unless raised, is refused before it is all read; past 50,000 files data.lock keeps the list in [data.lock.d](../reference/data-lock.md#big-datasets). archives in the folder unpack as a downloaded dataset's do.

a folder that holds the project, sits in its data folder, where fetchloom places what data.lock pins, or holds or sits in the cache, is refused with `usage`, exit 2. the same files imported again under their own name change nothing and say `<name> is already in data.toml`; other files under a name data.toml has take the next number, as a link does.

no source can list an import again. `fetchloom lock --upgrade <name>` refuses it, plain `lock --upgrade` skips it and says so, and a changed `select` or `exclude` in data.toml is refused with `reference.unresolved`, exit 10. to pin other files, run `fetchloom remove <name>`, then add them again. a switch of `extract` keeps the pinned files and asks no source, though once a cached archive is unpacked its own bytes leave the cache, as a downloaded archive's do, so a switch to `extract = false` then needs them supplied again.

### a clone without the files

a teammate's clone has data.toml and data.lock but not the files, which only your cache holds. sync then stops before any download, exit 10, naming the files it lacks, up to ten. with `--json`, `fix` is null, since sync fails again until the files are there:

```text
error: flowers was imported from files, and this machine's cache does not hold them
  flowers/petals/red.csv
  flowers/sepals.csv
  help: copy them to data/flowers by hand, then run `fetchloom sync`, or run `fetchloom add <folder> --as flowers` where they are
```

a [mirror](../mirrors.md) that holds them gives them instead, as `fetchloom push` fills one. a file no mirror holds is named with what each mirror said, as in [what goes wrong](../mirrors.md#what-goes-wrong).

files copied to `data/flowers` by hand are adopted when they hold the pinned bytes, and a build or a push takes them into the cache from there. `add <folder> --as <name>`, naming an import data.toml has, supplies its files instead of adding a dataset: the paths and sizes are compared with data.lock first, then the bytes as they are copied, and other files stop it with `local.io`, exit 50, naming the first difference, with data.toml and data.lock as they were. matching files change neither file:

```text
   Supplied flowers · 2 files · 32 B, copied into the cache; ../flowers is unchanged
         Ok flowers is in data.toml and data.lock as it was, its files are in data/flowers
```

a supply takes the patterns and `extract` data.toml gives, so `--select`, `--exclude` and `--no-extract` are refused with it. when data.lock holds no pin of the import, as after a lost data.lock, supplying pins the files again.

## a list of links

`-i <file>` adds a dataset for each link in a list, as `get -i` reads one: one link a line, blank lines and lines starting with `#` skipped, and after a link, optionally, the SHA-256 of its one file, as 64 hex digits or `sha256:` and 64 hex digits:

```text
# inputs for the paper
https://example.org/iris.csv sha256:d144ff83495e88cf892b8415db97343ff575bf11e47b6d08727499c4f32ffa9d
zenodo:3608990
```

the whole list is checked before any request: a line of search words, a path on this machine, a line naming the same data as an earlier one, a link fetchloom cannot read, and a link carrying a secret data.toml and data.lock would keep, stop the add naming the line, the secret masked. the links resolve together, then each dataset gets a name as a single add would, the next number when one is taken, and every file downloads together. a line with a SHA-256 must keep one file, and the bytes must have that hash, or the add stops with `integrity.mismatch`, exit 30, naming the line. a link data.toml has already stays as it is. a link that fails later, while its file downloads, is named by its line too, in the error and in each of the JSON `failures`, and a link with nothing at it gets help to fix that line, since data.toml has no entry for it yet, as in `line 3 of links.txt: 127.0.0.1 has nothing at http://127.0.0.1:43121/files/missing.csv (404)`. data.toml and data.lock are each written once, and any failure leaves both as they were. hosts a list names are not trusted as typed ones are, so a server on this machine or your network needs the [`allowed_origins` setting](../reference/environment.md). `--as`, `--select` and `--exclude` are usage errors with `-i`; `--no-extract` applies to every dataset.

## stopping an add

ctrl-c stops an add and leaves `data.toml` and `data.lock` as they were. `add` writes them only once every file is in the cache, so an add stopped while it downloads, or ended at once by a second ctrl-c, has changed neither; one stopped while it places files puts both back, as a failed addition does. the add then ends with `Stopped, and nothing was added`, and a line under it says how much the unfinished downloads keep. what arrived stays in the cache, so running the same add again continues where it stopped. exit code 130 means interrupted. once every file is placed the add is done, and a ctrl-c while it prints its last lines no longer stops it.

## editing during an add

an add reads `data.toml` and `data.lock` when it starts and writes them only after its downloads, which can take hours. if either file changed in the meantime, as when you edit `data.toml` or switch git branches, the add writes neither, so your change stays. it stops with `project.changed`, exit 50. it checks again right before its first write, and again before it writes `data.lock` after `data.toml`: a `data.lock` changed in between stays as you changed it, and the add puts `data.toml` back and stops the same way. what it downloaded stays in the cache, and running the same add again uses it, adding the dataset to the files as they are now. a file changed after the add wrote it, while it placed files, is left as you changed it if the add then fails or is stopped; the error names the recovery copy that holds the file as it was before the add.

a first add that ends without adding anything, because it failed, was stopped or was cancelled, leaves its folder as it was: it removes the `.fetchloom` folder it made, and the data folder it made when that holds no file. search words are answered before the project is touched, so q or ctrl-c at the list of results changes nothing. a first add ended at once by a second ctrl-c or a kill can leave `.fetchloom` behind; the next command in that folder takes it over.

## dry run

`add --dry-run`, or `-n`, prints what adding would do without writing `data.toml`, `data.lock`, `.gitignore`, `data/` or the cache, or downloading files for placement. explicit `FETCHLOOM_TRACE` output is still created or truncated. it still resolves the link, so a source may be asked what the link holds; for a kaggle competition whose rules the account may not have accepted, that means reading its smallest file when its listed size is at most 4 MiB. a zip wrapper can make the response larger; the probe uses the 64 MiB API-response limit and keeps none of the bytes. stderr from a git repository with no manifest yet, matching a test snapshot, where `[port]` stands for the test server's port:

```text
  Would pin iris, new in data.lock: http://127.0.0.1:[port]/files/iris.csv · 1 file · size unknown
Would fetch iris/iris.csv · size unknown
Would place data/iris · 1 file
  Would add data/ to .gitignore, since its files come from data.lock
  Would add iris to data.toml, its files in data/iris
         Ok dry run, nothing was changed
```

`add <path> --dry-run` reads no file's bytes: it names the files it would import, their count and size, and the dataset it would add, and lists their places in `changes.place`. it cannot tell yet whether the same files are in data.toml already.

the plan covers the whole project, as the sync after a real addition would. a file whose hash only a download can tell counts as one to place; when a file already stands where it goes, it is listed under `Would check` instead, with no warning, since only the download tells whether it holds the pinned bytes. an archive not yet unpacked here is listed with `Would open`, without its members. a link whose file name only the server gives is named from the link, since learning the name takes a download; pass `--as` to fix the name. adding an existing reference reports `<name> is already in data.toml`. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## generated names

the [dataset naming rules](../reference/data-toml.md#dataset-names) choose a name from a file, title or reference and handle collisions. use `--as` to choose your own. `-i` takes no `--as`, so when a line's name is taken its warning says to rename the dataset's table in data.toml afterwards, and the next sync moves its pin to the new name.

## exit codes

0 means the command succeeded, including when sync kept local conflicts. inspect `result.placement.kept` in JSON. 1 means a question went unanswered or was answered no: approval of a large download, or of a project in the home folder or at a disk's root. 2 means invalid usage. 10 includes an import's files this machine's cache lacks, and 50 a supplied folder holding other files than data.lock pins. source, network, integrity, access and local failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| a pattern matches nothing | it targets a different source path | inspect listed paths and [pattern rules](../reference/data-toml.md#patterns) |
| `--select` matches nothing in a link to a zip | select chooses among the files a link lists, and the zip is one of them; the help says so whenever the link lists an archive | add the link without `--select`, then keep files inside the zip with a [members table](../reference/data-toml.md#files-inside-archives) or leave some out with [`fetchloom drop`](drop.md) |
| a shell expands a pattern | the glob was not quoted | use `--select "**/*.csv"` |
| the same reference rejects new filters | the existing entry has different settings | edit its manifest entry, or use an unused `--as` name |
| adding fails for inline `datasets` | `add` cannot extend an inline table | use `[datasets.name]` tables |
| a large download stops in CI | it needs approval | check the size, then pass `--yes` if approved |
| `the project folder ... is read-only` | you may not write in the project folder | make the folder writable for you, then add again; on unix this is checked before anything downloads |
| `could not find ./flower.csv: it does not exist` | the path has a typo, or the file is somewhere else | when one name in that folder is a typo away, the help line gives the command with it, as ``did you mean `fetchloom add ./flowers.csv`?``, keeping your other options; only that folder is read, and nothing is searched for |
| `"iris" is a folder here, so fetchloom did not search for it` | a plain word names a file or folder in the current folder | write it as a path, `fetchloom add ./iris`, to import it, or run `fetchloom search iris` |
| `... was imported from files, and this machine's cache does not hold them` | a clone has the pins of an import but not its files | copy them to the dataset's folder and sync, or run `fetchloom add <folder> --as <name>` where they are |
| `... was imported from files, and neither this machine's cache nor a mirror holds them` | the mirrors asked lack the files named, or hold damaged copies | ask whoever keeps the mirror to push them again, or copy them to the dataset's folder and sync |
| `... does not hold the files data.lock pins for ...` | the folder supplied is not the one imported | supply the folder the files came from, or remove the dataset and import the new files |
