# add

`add` puts a dataset in `data.toml`, pins its files in `data.lock` and syncs the whole project.

```sh
fetchloom add iris    # at a terminal, pick one from a list
fetchloom add https://huggingface.co/datasets/scikit-learn/iris
fetchloom add https://zenodo.org/records/3608990 --as flowers
fetchloom add hf:datasets/scikit-learn/iris --select "*.csv" --dry-run
fetchloom add ./flowers    # files on this machine, pinned by their bytes
fetchloom add zenodo:3608990 hf:datasets/scikit-learn/iris    # a dataset for each link
fetchloom add -i links.txt    # a dataset for each link in a list
```

## usage

```sh
fetchloom add [options] <link>...
fetchloom add [options] <path>
fetchloom add [options] -i <file>
```

| option | effect |
|---|---|
| `--as <name>` | choose the dataset name and its default folder under `data/`; one link only |
| `--select <glob>` | keep files matching any supplied pattern; repeat for more patterns |
| `--exclude <glob>` | drop matching files; repeat for more exclusions |
| `--no-extract` | keep archives packed and record `extract = false`; webdataset loaders read `.tar` shards packed |
| `-i <file>`, `--input-file <file>` | add a dataset for each link in a file, one per line, each with an optional SHA-256, or in stdin for `-`; `--as`, `--select` and `--exclude` do not apply |
| `-n`, `--dry-run` | plan the addition without applying project or cache changes |

[global options](README.md#global-options) also apply, and [sources](../sources/README.md) lists the links and references `add` reads. plain words [search](search.md#choosing-a-dataset) hugging face, kaggle, zenodo, GEO and openneuro; quote several, as in `fetchloom add "fashion mnist"`. for ENA or dandi, find the reference with `fetchloom search <words> --source ena` or `--source dandi`. data.toml records the id you chose, not the search words.

## what happens

1. find the nearest project, or start one in the current folder, which a closing `Started a project in <folder>` line names. in your home folder or at the root of a disk, where a project would take in every folder below it, `add` asks first; without a terminal it refuses unless `--yes` is given.
2. resolve the reference, filter its files and choose a dataset name. a `Resolved` line names what the reference resolved to, and counts the files kept, as `1 of 4 files`, when a file link, `--select` or `--exclude` keeps fewer than the source lists. [webdataset shards](../concepts.md#webdataset-shards) get a hint before anything downloads.
3. download every file the project lacks into the cache, with the project still as it was.
4. write the entry in `data.toml`, pin its files in `data.lock` and place the project's files. entries are kept in name order, so two people adding different datasets rarely change the same lines.
5. in a git repository, add the data folder to `.gitignore` if needed.

below a project's folder, `add` works on that project, says `using the project in <folder>` once at a terminal, and gives the dataset's folder from where you stand, such as `../data/iris`. the search for a project stops at a git repository, so in a repository with no `data.toml` of its own, `add` starts a project there, as [finding the project](../reference/project-folder.md#finding-the-project) explains. there is no `init` command: `fetchloom init` is a usage error, exit 2, whose help says `add` starts a project.

stderr for a single-file addition in a new project, from a test snapshot, where `[time]` stands for the duration and `[sandbox]` for the test's folder:

```text
 Downloaded iris.csv · 32 B in [time]
    Started a project in [sandbox]/work
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

a typed link to this machine or your network, such as a lab server, is trusted for that run alone. a later run that downloads from its origin, the scheme, host and port, here or in a clone, is refused with `network.refused`, exit 20, until the [`allowed_origins`](../reference/environment.md#settings-file) setting lists it. so the add warns, under `-q` too, with the line to write, and its closing line asks anyone who clones to list the origin too, even when your settings list it, since each machine has its own. the last lines of a real run in a git repository, on stderr:

```text
      Added cows, its files are in data/cows
  hint: scripts can find it with `fetchloom path cows`
warning: a later run downloads from http://127.0.0.1:19450, a private address, only once allowed_origins lists it
  help: put `allowed_origins = ["http://127.0.0.1:19450"]` in ~/.config/fetchloom/settings.toml, or set `FETCHLOOM_ALLOWED_ORIGINS=http://127.0.0.1:19450` for one run
  next: commit data.toml, data.lock and .gitignore; anyone who clones adds `allowed_origins = ["http://127.0.0.1:19450"]` to their settings file, then runs `fetchloom sync`
```

a new link with nothing at it, or an id its source does not know, is fixed on the command line, since data.toml has no entry for it yet. from a real run in an empty folder:

```text
error: 127.0.0.1 has nothing at http://127.0.0.1:19450/files/missing.csv (404)
  help: check the link in a browser. if it moved, run `fetchloom add <new link>`
```

for a link data.toml already has, the help names its `ref` there instead, whether you add it again or another add syncs it.

adding a reference data.toml has, with the same patterns and extraction, syncs it without another entry, and so does the same add with the `--as` name it already has, so a setup script can run twice. other settings, or another link under a name data.toml has, as in `data.toml already has a dataset called notes`, fail with `usage`, exit 2, and change nothing: give an unused `--as` name, edit `data.toml` to change the entry, or run `fetchloom remove <name>` first to replace it. removing is your choice, so the JSON `fix` is null.

a failed add puts `data.toml` and `data.lock` back as they were, or removes them if it made them, and removes the files it placed; what it downloaded stays cached for the next try. before it changes either file it saves a copy under `.fetchloom/`, readable only by you on unix. if putting a file back fails, the error names that copy, which stays, without printing it, since it may hold credentials.

## files on this machine

a path, such as `./flowers`, `../shared/flowers`, `/data/flowers`, `~/flowers`, `C:\data\flowers` or a `file:` link, imports the files there: every plain file in a folder, or one file. data.toml gets `ref = "local"`, never the path, which would mean nothing on a teammate's machine and would say where your files are. data.lock pins each file by its path below the folder, its size and its SHA-256, with an empty `url`. a plain word that names a folder here, such as `iris` beside an `iris` folder, is neither imported nor searched: `"iris" is a folder here, so fetchloom did not search for it` exits 2, and its help offers `fetchloom add ./iris` and `fetchloom search iris`. a word written as a path that names nothing here, such as `flowerz/` or `visits.csv`, is never searched either: it exits 2 with the near name, as [search](search.md#choosing-a-dataset) shows.

the files are copied into the cache and hashed as they are copied, as many at once as [`FETCHLOOM_THREADS`](../reference/environment.md) allows. on linux, a disk that clones files, such as btrfs or xfs, gets a clone that shares the file's blocks; elsewhere the cache holds a copy, so on ext4 the files take their room twice until you remove the folder. your files are only read, never changed, moved or linked, since a link to the cache's copy would make them read-only. an import bigger than the free space on the cache's disk asks first, as a big download does. everyone who can read the cache can read what you import, which is only you unless you made it [one cache for a whole lab](../hpc.md#one-cache-for-a-whole-lab). stderr from a project whose data.toml already existed:

```text
   Imported flowers · 2 files · 32 B, copied into the cache; ../flowers is unchanged
      Added flowers, its files are in data/flowers
  hint: scripts can find it with `fetchloom path flowers`
```

the folder stays where it was. inside a git repository, where `git add .` would commit the raw data, maybe to a public remote, the import warns, `git.import_in_tree` in `--json`, unless .gitignore leaves the folder out already:

```text
warning: ./flowers is still in the project, and git will take it
  help: delete it, or add /flowers/ to .gitignore
```

when git tracks the folder already, the warning says `git tracks ./flowers, so a commit keeps its files in the repository` and its help starts with `git rm -r --cached ./flowers`, which takes it out of git's index and leaves its files.

a folder of many files takes a moment to pin and place after `Imported`, so a terminal shows `Pinning flowers` and then `Placing 20,000 files`, each with its time once it lasts a second.

below the folder, `.git`, `.hg`, `.svn` and `.fetchloom` are left out unseen. files a system writes by itself are left out too, and one `Skipped` line names them: finder's `.DS_Store`, `._` files and `__MACOSX`, the `.Spotlight-V100`, `.Trashes`, `.fseventsd`, `.TemporaryItems` and `.Trash-<uid>` folders of removable disks, and windows' `Thumbs.db`, `ehthumbs.db`, `desktop.ini`, `$RECYCLE.BIN` and `System Volume Information`. links and special files, such as named pipes, are left out with a warning and never followed. names are checked as an archive's members are, before anything is copied: a name with a control character or a `\`, one longer than 4096 bytes, one this machine cannot store, or two names that are one on disks that ignore case or unicode composition, stop the import with `source.invalid`, exit 10. `--select` and `--exclude` match the paths below the folder, and a select pattern that keeps nothing is a usage error. a folder holding more files than [`most_files`](../reference/environment.md) is refused before it is all read. archives in the folder unpack as a downloaded dataset's do.

a folder that holds the project, sits in its data folder, or holds or sits in the cache, is refused with `usage`, exit 2. data a project keeps in its data folder already, such as cookiecutter's `data/raw`, is refused the same way whether it is typed `data/raw` or `./data/raw`, and the help gives the commands that move it out and add it back, or the `data_dir` that keeps `data/` as it is. stderr from a first run in a folder that holds `data/raw`:

```text
error: data/raw is in the data folder, where fetchloom places what data.lock pins
  help: move it out first, with `mv data/raw raw`, then run `fetchloom add ./raw` to place its files in data/raw, or keep data/ as it is by setting data_dir under [project] in data.toml to another folder
```

the same files imported again under their own name change nothing and say `<name> is already in data.toml`; other files under a name data.toml has take the next number, as a link does.

no source can list an import again. `fetchloom lock --upgrade <name>` refuses it, plain `lock --upgrade` skips it and says so, and a changed `select` or `exclude` in data.toml is refused with `reference.unresolved`, exit 10. to pin other files, run `fetchloom remove <name>`, then add them again. a switch of `extract` keeps the pinned files and asks no source, but once a cached archive is unpacked its own bytes leave the cache, so a switch to `extract = false` then needs them supplied again.

### a clone without the files

a teammate's clone has data.toml and data.lock but not the files, which only your cache holds. sync then stops before any download, exit 10, naming up to ten of the files it lacks. with `--json`, `fix` is null, since sync fails again until the files are there:

```text
error: flowers was imported from files, and this machine's cache does not hold them
  flowers/petals/red.csv
  flowers/sepals.csv
  help: copy them to data/flowers by hand, then run `fetchloom sync`, or run `fetchloom add <folder> --as flowers` where they are
```

a [mirror](../mirrors.md) that `fetchloom push` filled gives them instead; when the mirrors asked lack them or hold damaged copies, the error says `neither this machine's cache nor a mirror holds them`, and whoever keeps the mirror can push them again. files copied to `data/flowers` by hand are taken when they hold the pinned bytes.

`add <folder> --as <name>`, naming an import data.toml has, supplies its files instead of adding a dataset. the paths and sizes are compared with data.lock first, then the bytes as they are copied; other files stop it with `local.io`, exit 50, as `<folder> does not hold the files data.lock pins for <name>`, naming the first difference, and leave data.toml and data.lock as they were. supply the folder the files came from, or remove the dataset and import the new files. matching files change neither:

```text
   Supplied flowers · 2 files · 32 B, copied into the cache; ../flowers is unchanged
         Ok flowers is in data.toml and data.lock as it was, its files are in data/flowers
```

a supply takes the patterns and `extract` data.toml gives, so `--select`, `--exclude` and `--no-extract` are refused with it. when data.lock holds no pin of the import, as after a lost data.lock, a supply pins the files again.

## a list of links

`-i <file>` adds a dataset for each link in a list, as `get -i` reads one: one link a line, blank lines and lines starting with `#` skipped, and after a link, optionally, the SHA-256 of its one file, as 64 hex digits or `sha256:` and 64 hex digits:

```text
# inputs for the paper
https://example.org/iris.csv sha256:d144ff83495e88cf892b8415db97343ff575bf11e47b6d08727499c4f32ffa9d
zenodo:3608990
```

the whole list is checked before any request. a line of search words, a path on this machine, a line naming the same data as an earlier one, a link fetchloom cannot read, or a link carrying a secret data.toml would keep, stops the add, naming the line with the secret masked. then the links resolve and download together, and each dataset gets a name as a single add would. a line with a SHA-256 must keep one file with that hash, or the add stops with `integrity.mismatch`, exit 30. a link data.toml has already stays as it is.

a link that fails while its file downloads is named by its line too, in the error and in each of the JSON `failures`, as in `line 3 of links.txt: 127.0.0.1 has nothing at http://127.0.0.1:43121/files/missing.csv (404)`. data.toml and data.lock are each written once, and any failure leaves both as they were. the add ends with one `Added` line for each dataset; `-v` adds what each one pinned and where its files went. hosts in a list are not trusted as typed ones are, so a server on this machine or your network needs the [`allowed_origins` setting](../reference/environment.md#settings-file) first. `--as`, `--select` and `--exclude` are usage errors with `-i`; `--no-extract` applies to every dataset.

### several links at once

several links on the command line add a dataset each, as the lines of a list do, but each is trusted as a typed link is, so a lab server on a private address gets the warning above. search words go alone, in quotes, and a path on this machine is added one at a time. `--as`, `--select` and `--exclude` are each for one dataset, so with several links they are usage errors, exit 2:

```text
error: --as names one dataset, and 2 links add 2
  help: add the link to name on its own, as in `fetchloom add <link> --as <name>`
```

## stopping an add

ctrl-c leaves `data.toml` and `data.lock` as they were: `add` writes them only once every file is in the cache, and one stopped while it places files puts both back. the add ends with `Stopped, and nothing was added`, exit 130. what arrived stays in the cache, so the same add again continues where it stopped. a ctrl-c after the last byte arrived lets the add finish, as [interruptions](../concepts.md#interruptions) explains.

## editing during an add

an add reads `data.toml` and `data.lock` when it starts and writes them only after its downloads, which can take hours. if either changed in the meantime, as when you edit `data.toml` or switch git branches, the add writes neither, keeps your change and stops with `project.changed`, exit 50. it checks again before each write, and a `data.lock` changed between them stays as you changed it while the add puts `data.toml` back. what it downloaded stays in the cache, so the same add again adds the dataset to the files as they are now.

a first add that ends without adding anything, because it failed, was stopped or was cancelled, leaves its folder as it was: it removes the `.fetchloom` folder it made, and the data folder it made when that holds no file. search words are answered before the project is touched, so q or ctrl-c at the list of results changes nothing. a first add that was killed can leave `.fetchloom` behind, and the next command in that folder takes it over.

## dry run

`add --dry-run`, or `-n`, prints what adding would do without writing `data.toml`, `data.lock`, `.gitignore`, `data/` or the cache, or downloading files for placement. it still resolves the link, so a source may be asked what the link holds, as [kaggle](../sources/kaggle.md) is for a competition. stderr from a git repository with no manifest yet, from a test snapshot, where `[port]` stands for the test server's port:

```text
      Would pin iris, new in data.lock: http://127.0.0.1:[port]/files/iris.csv · 1 file · size unknown
      Would fetch iris/iris.csv · size unknown
      Would place data/iris · 1 file
      Would add data/ to .gitignore, since its files come from data.lock
      Would add iris to data.toml, its files in data/iris
         Ok dry run, nothing was changed
```

`add <path> --dry-run` reads no file's bytes: it names the files it would import, their count and size, and the dataset it would add, and lists their places in `changes.place`. it cannot tell yet whether the same files are in data.toml already.

the plan covers the whole project, as the sync after a real addition would. a file whose hash only a download can tell counts as one to place, or as one to check, under `Would check`, when a file already stands where it goes. an archive not yet unpacked here is listed with `Would open`, without its members. a link whose file name only the server gives is named from the link; pass `--as` to fix the name. a plain link is asked nothing, so what only the real add finds shows then, as [URLs and DOIs](../sources/urls-and-dois.md#dry-runs) shows. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## generated names

the [dataset naming rules](../reference/data-toml.md#dataset-names) choose a name from a file, title or reference and handle collisions; `--as` chooses your own. `-i` takes no `--as`, so when a line's name is taken its warning says to rename the dataset's table in data.toml afterwards, and the next sync moves its pin to the new name.

## exit codes

0 means the add succeeded, also when sync kept local conflicts, which JSON lists in `result.placement.kept`. 1 means a question went unanswered or was answered no: a large download, or a project in the home folder or at a disk's root. 2 means invalid usage. 10 includes an import's files this machine's cache lacks, and 50 a supplied folder holding other files than data.lock pins. the others are the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| a pattern matches nothing | it targets a different source path | inspect listed paths and [pattern rules](../reference/data-toml.md#patterns) |
| `--select` matches nothing in a link to a zip | select chooses among the files a link lists, and the zip is one of them; the help says so whenever the link lists an archive | add the link without `--select`, then keep files inside the zip with a [members table](../reference/data-toml.md#files-inside-archives) or leave some out with [`fetchloom drop`](drop.md) |
| a shell expands a pattern | the glob was not quoted | use `--select "**/*.csv"` |
| adding fails for inline `datasets` | `add` cannot extend an inline table | use `[datasets.name]` tables |
| a large download stops in CI | it needs approval | check the size, then pass `--yes` if approved |
| `the project folder ... is read-only` | you may not write in the project folder | make the folder writable for you, then add again; on unix this is checked before anything downloads |
| `could not find ./flower.csv: it does not exist` | the path has a typo, or the file is somewhere else | when one name in that folder is a typo away, the help line gives the command with it, as ``did you mean `fetchloom add ./flowers.csv`?``, keeping your other options; only that folder is read, and nothing is searched for |
| `huggingface.co has no dataset scikit-learn/irsi, or it is private`, or signed in, `hf:datasets/scikit-learn/irsi does not exist, or is not public` | a typo in a hugging face or kaggle id, or a private dataset: both sources answer the two alike | when the owner lists a public dataset a typo away, which `add` asks the source once, signed in or not, and never offline, the help gives your command with the name fixed, as ``did you mean `fetchloom add hf:datasets/scikit-learn/iris`?``, or for a link with a query, a fragment or a login, which may hold a secret, the dataset alone, as `did you mean hf:datasets/scikit-learn/iris?`; no sign-in is offered, and the error is `reference.unresolved`, exit 10. otherwise see [when a download is refused](auth.md#when-a-download-is-refused) |
