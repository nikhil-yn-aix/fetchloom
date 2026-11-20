# add

`add` puts a dataset in `data.toml`, pins its files in `data.lock` and places them in the data folder. the project's other datasets stay as they are until `fetchloom sync`.

```sh
fetchloom add iris    # at a terminal, pick one from a list
fetchloom add https://huggingface.co/datasets/scikit-learn/iris
fetchloom add https://zenodo.org/records/3608990 --as flowers
fetchloom add hf:datasets/scikit-learn/iris --select "*.csv" --dry-run
fetchloom add ./flowers    # files on this machine, pinned by their bytes
fetchloom add zenodo:3608990 hf:datasets/scikit-learn/iris    # a dataset for each link
fetchloom add https://physionet.org/files/mitdb/1.0.0/SHA256SUMS.txt    # every file a checksum list names, one dataset
fetchloom add -i links.txt --as era5    # the files a list in the project names, one dataset
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
| `--exclude <glob>` | drop matching files; repeat for more exclusions. with one link, a web server's folder that a pattern leaves out whole, as `big/**` does `big`, is not listed |
| `--no-extract` | keep archives packed and record `extract = false`; webdataset loaders read `.tar` shards packed |
| `--decompress` | place each single `.gz`, `.bz2`, `.xz` or `.zst` file decompressed, under its name without the ending, and record `decompress = true`; a compressed tar such as `.tar.gz` stays extract's |
| `--web-page` | pin the web page a link answers with as the data and record `web_page = true`; without it such a page is refused, see [web pages as data](../sources/urls-and-dois.md#web-pages-as-data) |
| `--no-sync` | download nothing: data.lock records each file by the size and SHA-256 or MD5 its source states, and a later `fetchloom sync` downloads and checks it. a link data.toml has already is pinned the same way when data.lock does not pin it as data.toml asks, and data.toml stays as it is; a pin data.lock holds under the name the dataset was [renamed](../reference/data-toml.md#dataset-names) from counts, as sync carries it, and the add says `Ok flowers is already in data.toml, and data.lock pins it as iris`. refused when the source states neither for some file, and for a path on this machine |
| `-i <file>`, `--input-file <file>` | add the files a list in the project names as one dataset, a file's link a line with its MD5 or SHA-256 if known, as [a list of files](#a-list-of-files) says; data.toml names the list by its path |
| `-n`, `--dry-run` | plan the addition without applying project or cache changes |

[global options](global-options.md) also apply, and [sources](../sources/README.md) lists the links and references `add` reads. plain words [search](search.md#choosing-a-dataset) hugging face, kaggle, zenodo, GEO and openneuro; several are one search, quoted or not, as in `fetchloom add fashion mnist`, when none is a link, an id, a DOI or a path. for ENA or dandi, find the reference with `fetchloom search <words> --source ena` or `--source dandi`. data.toml records the id you chose, not the search words.

## what happens

1. find the nearest project, or start one in the current folder, which a closing `Started a project in <folder>` line names. in your home folder or at the root of a disk, where a project would take in every folder below it, `add` asks first; without a terminal it refuses unless `--yes` is given. its help suggests a folder of its own, `mkdir my-project` and `cd my-project`, and repeats your command for that folder, each relative path one folder further up, as `fetchloom add ../stuff` for `fetchloom add ./stuff`.
2. resolve the reference, filter its files and choose a dataset name. a `Resolved` line names what the reference resolved to, and counts the files kept, as `1 of 4 files`, when a file link, `--select` or `--exclude` keeps fewer than the source lists. [webdataset shards](../reference/archives.md#webdataset-shards) get a hint before anything downloads.
3. download the new dataset's files the cache lacks, with the project still as it was.
4. write the entry in `data.toml`, pin its files in `data.lock` and place them. entries are kept in name order, so two people adding different datasets rarely change the same lines. every other dataset keeps its pin and files as they are, even one whose entry changed or whose files are not here, as in a fresh clone, so a link of another dataset that no longer answers never stops the add: `fetchloom sync` does the rest.
5. add the data folder to `.gitignore` if needed: in a git repository, and outside one when the add begins the project, starting it or pinning the first datasets of a hand-written data.toml, so a later `git init` leaves the data out.

below a project's folder, `add` works on that project, says `using the project in <folder>` once at a terminal, and gives the dataset's folder from where you stand, such as `../data/iris`. the search for a project stops at a git repository, so in a repository with no `data.toml` of its own, `add` starts a project there, as [finding the project](../reference/project-folder.md#finding-the-project) explains. there is no `init` command: `fetchloom init` is a usage error, exit 2, whose help says `add` starts a project.

stderr for a single-file addition in a new project, from a test snapshot, where `[time]` stands for the duration and `[sandbox]` for the test's folder:

```text
 Downloaded iris.csv · 32 B in [time]
    Started a project in [sandbox]/work
    Ignored data/ in .gitignore, since its files come from data.lock
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

a typed link to this machine or your network, such as a lab server, is trusted for that run alone. a later run that downloads from its origin, the scheme, host and port, here or in a clone, is refused with `network.refused`, exit 10, until the [`allowed_origins`](../reference/environment.md#settings-file) setting lists it. so the add warns, under `-q` too, with the line to write, and its closing line asks anyone who clones to list the origin too, even when your settings list it, since each machine has its own. the last lines of a real run in a git repository, on stderr:

```text
      Added cows, its files are in data/cows
  hint: scripts can find it with `fetchloom path cows`
warning: a later run downloads from http://127.0.0.1:19450, a private address, only once allowed_origins lists it
  help: save it with `fetchloom settings set allowed_origins http://127.0.0.1:19450`, or put `FETCHLOOM_ALLOWED_ORIGINS=http://127.0.0.1:19450` before the command for that run alone
  next: commit data.toml, data.lock and .gitignore; anyone who clones runs `fetchloom settings set allowed_origins http://127.0.0.1:19450`, with any origins they list already, then `fetchloom sync`
```

a new link with nothing at it, or an id its source does not know, is fixed on the command line, since data.toml has no entry for it yet. from a real run in an empty folder:

```text
error: 127.0.0.1 has nothing at http://127.0.0.1:19450/files/missing.csv (404)
  help: check the link in a browser. if it moved, run `fetchloom add <new link>`
```

for a link data.toml already has, the help names its `ref` there instead, whether you add it again or a sync reaches it.

adding a reference data.toml has, with the same patterns and extraction, syncs that dataset alone without another entry, and so does the same add with the `--as` name it already has, so a setup script can run twice. other settings, or another link under a name data.toml has, as in `data.toml already has a dataset called notes`, fail with `usage`, exit 2, and change nothing: give an unused `--as` name, edit `data.toml` to change the entry, or run `fetchloom remove <name>` first to replace it. removing is your choice, so the JSON `fix` is null.

a failed add puts `data.toml` and `data.lock` back as they were, or removes them if it made them, and removes the files it placed; what it downloaded stays cached for the next try. before it changes either file it saves a copy under `.fetchloom/`, readable only by you on unix. if putting a file back fails, the error names that copy, which stays, without printing it, since it may hold credentials.

## files on this machine

a path, such as `./flowers`, `../shared/flowers`, `/data/flowers`, `~/flowers`, `C:\data\flowers` or a `file:` link, imports the files there: every plain file in a folder, or one file. data.toml gets `ref = "local"`, never the path, which would mean nothing on a teammate's machine and would say where your files are. data.lock pins each file by its path below the folder, its size and its SHA-256, with an empty `url`. a plain word that names a folder here, such as `iris` beside an `iris` folder, is neither imported nor searched: `"iris" is a folder here, so fetchloom did not search for it` exits 2, and its help offers `fetchloom add ./iris` and `fetchloom search iris`. a word written as a path that names nothing here, such as `flowerz/` or `visits.csv`, is never searched either: it exits 2 with the near name, as [search](search.md#choosing-a-dataset) shows.

the files are copied into the cache and hashed as they are copied, as many at once as [`FETCHLOOM_THREADS`](../reference/environment.md) allows. on linux, a disk that clones files, such as btrfs or xfs, gets a clone that shares the file's blocks; elsewhere the cache holds a copy, so on ext4 the files take their room twice until you remove the folder. your files are only read, never changed, moved or linked, since a link to the cache's copy would make them read-only. an import bigger than the free space on the cache's disk asks first, as a big download does. everyone who can read the cache can read what you import, which is only you unless you made it [one cache for a whole lab](../guides/clusters.md#one-cache-for-a-whole-lab). stderr from a project whose data.toml already existed:

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

a folder of many files takes a while to read before anything is copied, so a terminal shows `Reading ./flowers` once the command has run a second, and output that is no terminal says it once the reading has lasted two. at a terminal, an import shows its copying, and the wait while the disk keeps the copies, as bars once the command has run a second, so a small folder's add shows its closing lines alone. a folder of many files takes a moment to pin and place after `Imported`, so a terminal shows `Pinning flowers` and then `Placing 20,000 files`, each with its time once it lasts a second.

below the folder, `.git`, `.hg`, `.svn` and `.fetchloom` are left out unseen. files a system writes by itself are left out too, and one `Skipped` line names them: finder's `.DS_Store`, `._` files and `__MACOSX`, the `.Spotlight-V100`, `.Trashes`, `.fseventsd`, `.TemporaryItems` and `.Trash-<uid>` folders of removable disks, and windows' `Thumbs.db`, `ehthumbs.db`, `desktop.ini`, `$RECYCLE.BIN` and `System Volume Information`. links and special files, such as named pipes, are left out with a warning and never followed. names are checked as an archive's members are, before anything is copied: a name with a control character or a `\`, one longer than 4096 bytes, one this machine cannot store, or two names that are one on disks that ignore case or unicode composition, stop the import with `source.invalid`, exit 10. `--select` and `--exclude` match the paths below the folder, and a select pattern that keeps nothing is a usage error. a folder holding more files than [`most_files`](../reference/environment.md) is refused before it is all read. archives in the folder unpack as a downloaded dataset's do.

a folder that holds the project, sits in its data folder, or holds or sits in the cache, is refused with `usage`, exit 2. data a project keeps in its data folder already, such as cookiecutter's `data/raw`, is refused the same way whether it is typed `data/raw` or `./data/raw`, and the help gives the commands that move it out and add it back, or the `data_dir` that keeps `data/` as it is. stderr from a first run in a folder that holds `data/raw`:

```text
error: data/raw is in the data folder, where fetchloom places what data.lock pins
  help: move it out first, with `mv data/raw raw`, then run `fetchloom add ./raw` to place its files in data/raw, or keep data/ as it is by setting data_dir under [project] in data.toml to another folder
```

the same files imported again under their own name change nothing and say `<name> is already in data.toml`. other files under the name of an import data.toml has, as a folder that gained a file, are refused with `usage`, exit 2, rather than given the next number, which would leave a second copy beside the folder your scripts read. the help gives both ways on, as stderr after `flowers` gained `b.csv`:

```text
error: flowers pins other files than ./flowers holds
  b.csv is not in data.lock
  help: to pin these instead, run `fetchloom remove flowers`, then `fetchloom add ./flowers`; to keep both, run `fetchloom add ./flowers --as flowers-2`
```

paths and sizes are compared before anything is copied, so a dry run is refused the same way; the same paths and sizes holding other bytes are refused once they are read. files under the name of a dataset that is not an import take the next number, as a link does.

no source can list an import again. `fetchloom lock --upgrade <name>` refuses it, plain `lock --upgrade` skips it and says so, and a changed `select` or `exclude` in data.toml is refused with `reference.unresolved`, exit 10. to pin other files, run `fetchloom remove <name>`, then add them again. a switch of `extract` keeps the pinned files and asks no source, but once a cached archive is unpacked its own bytes leave the cache, so a switch to `extract = false` then needs them supplied again.

### a clone without the files

a teammate's clone has data.toml and data.lock but not the files, which only your cache holds. sync then stops before any download, exit 10, naming up to ten of the files it lacks. with `--json`, `fix` is null, since sync fails again until the files are there:

```text
error: flowers was imported from files, and this machine's cache does not hold them
  flowers/petals/red.csv
  flowers/sepals.csv
  help: copy them to data/flowers by hand, then run `fetchloom sync`, or run `fetchloom add <folder> --as flowers` where they are
```

a [mirror](../guides/mirrors.md) that `fetchloom push` filled gives them instead; when the mirrors asked lack them or hold damaged copies, the error says `neither this machine's cache nor a mirror holds them`, and whoever keeps the mirror can push them again. files copied to `data/flowers` by hand are taken when they hold the pinned bytes.

one error names every import the run cannot supply, each import's first file before any second one in the details, so a teammate supplies them all in one round. the run first downloads and places every other dataset it can get, and the details name those it placed and those that wait; stderr from a real run whose mirror held only `flowers`:

```text
     Copied 2 files · 15 B from ../mirror
     Placed data/flowers · 2 files linked to the cache
error: a-very-long-dataset-name-for-testing and tiny were imported from files, and neither this machine's cache nor a mirror holds them
  a-very-long-dataset-name-for-testing/l.csv: not there on ../mirror
  tiny/t.csv: not there on ../mirror
  placed flowers, whose files are all on this machine
  a-very-long-dataset-name-for-testing, tiny wait for files this run could not get
  help: copy each one's files to its folder by hand, then run `fetchloom sync`, or run `fetchloom add <folder> --as <name>` where they are
```

`add <folder> --as <name>`, naming an import data.toml has, supplies its files instead of adding a dataset. the paths and sizes are compared with data.lock first, then the bytes as they are copied; other files stop it with `local.io`, exit 50, as `<folder> does not hold the files data.lock pins for <name>`, naming the first difference, and leave data.toml and data.lock as they were. supply the folder the files came from, or remove the dataset and import the new files. matching files change neither:

```text
   Supplied flowers · 2 files · 32 B, copied into the cache; ../flowers is unchanged
         Ok flowers is in data.toml and data.lock as it was, its files are in data/flowers
```

a supply takes the patterns and `extract` data.toml gives, so `--select`, `--exclude` and `--no-extract` are refused with it. when data.lock holds no pin of the import, as after a lost data.lock, a supply pins the files again.

## a list of files

a list of files is one dataset: a checksum list such as `SHA256SUMS` or `MD5SUMS`, a pooch registry, or a list of links, such as earthaccess or ESGF tools write. one reader takes every line form, told apart by which field is a hash:

| form | a line | the file |
|---|---|---|
| GNU, as `sha256sum` writes | `<hash>  <path>`, or `<hash> *<path>` | `<path>` below the list's folder |
| BSD, as `md5` and `sha256sum --tag` write | `SHA256 (<path>) = <hash>`, `MD5 (<path>) = <hash>` | `<path>` below the list's folder |
| pooch registry | `<path> <hash>`, or `<path> <hash> <link>` | its link when the line gives one, else `<path>` below the list's folder |
| a list of links | `<link>`, or `<link> <hash>` | its link, named by its last part |

a hash is an MD5 or a SHA-256, bare, where its length tells which, or after `md5:` or `sha256:`. a stated MD5 is checked on the first download, and data.lock pins the SHA-256 that download has. any other hash is refused by its name, as `line 1 of SHA1SUMS gives a SHA-1 hash, and fetchloom checks only MD5 and SHA-256`. blank lines and lines starting with `#` are skipped.

a link to a list whose name says it holds checksums, as `SHA256SUMS`, `MD5SUMS`, `SHA256SUMS.txt`, `md5sums.txt` or NCBI's `md5checksums.txt` do, adds every file it lists, each at the list's folder plus its path. `list:` and a link adds a list of any other name, as `fetchloom add list:https://example.org/data/registry.txt`. the dataset is named after the list's folder for a checksum list, as `mitdb-1-0-0` for physionet's, and after the list for any other.

`-i <file>` adds a list in the project, which gives each file's link, since a list on this machine has no folder for a path below it:

```text
# inputs for the paper
https://example.org/iris.csv sha256:d144ff83495e88cf892b8415db97343ff575bf11e47b6d08727499c4f32ffa9d
https://example.org/tips.csv md5:4c29e6fc9e1a9a9aa4a0e6b3b5a0fb36
```

data.toml names the list by its path in the project, as `ref = "list:links.txt"`, never by copying it, so commit the list with data.toml: a clone syncs from data.lock alone, and reads the list again only when the dataset is locked again, as after an edit of its `select` or a `lock --upgrade`. a list from stdin or from outside the project could not be found again, so it is refused with `usage`, exit 2. `--as`, `--select` and `--exclude` apply as to any dataset, and the name is the list's own without its type, as `links` for `links.txt`.

a list is text anyone could have written, so it is read whole and checked before any file is asked for: a line in no form above, a path that climbs out of the list's folder or starts at the root of a disk, a link carrying a secret data.lock would keep, or a list larger than 64 MiB, stops the add, naming the line. a list in the project is read only from a real file there, never through a link. the hosts a list names are trusted no more than data.toml's, so a server on this machine or your network needs the [`allowed_origins` setting](../reference/environment.md#settings-file) first; the link of a list typed on the command line reaches its own origin for that run, as any typed link does. a file with nothing at its link fails the add, whose help leaves it out with `--exclude` while the other files arrived, and otherwise says to fix its line of a list in the project, or that only the owner of a list at a link can.

### several links at once

several links on the command line add a dataset each, each trusted as a typed link is, so a lab server on a private address gets the warning above. search words go alone, all of them one search, and a path on this machine is added one at a time. `--as`, `--select` and `--exclude` are each for one dataset, so with several links they are usage errors, exit 2:

```text
error: --as names one dataset, and 2 links add 2
  help: add the link to name on its own, as in `fetchloom add <link> --as <name>`
```

## stopping an add

ctrl-c leaves `data.toml` and `data.lock` as they were: `add` writes them only once every file is in the cache, and one stopped while it places files puts both back. the add ends with `Stopped, and nothing was added`, exit 130. what arrived stays in the cache, so the same add again continues where it stopped. a ctrl-c after the last byte arrived lets the add finish, as [interruptions](../concepts/questions-and-interruptions.md#interruptions) explains.

## editing during an add

an add reads `data.toml` and `data.lock` when it starts and writes them only after its downloads, which can take hours. if either changed in the meantime, as when you edit `data.toml` or switch git branches, the add writes neither, keeps your change and stops with `project.changed`, exit 50. it checks again before each write, and a `data.lock` changed between them stays as you changed it while the add puts `data.toml` back. what it downloaded stays in the cache, so the same add again adds the dataset to the files as they are now.

a first add that ends without adding anything, because it failed, was stopped or was cancelled, leaves its folder as it was: it removes the `.fetchloom` folder it made, and the data folder it made when that holds no file. search words are answered before the project is touched, so q or ctrl-c at the list of results changes nothing. a first add that was killed can leave `.fetchloom` behind, and the next command in that folder takes it over.

## dry run

`add --dry-run`, or `-n`, prints what adding would do without writing `data.toml`, `data.lock`, `.gitignore`, `data/` or the cache, or downloading files for placement. it still resolves the link, so a source may be asked what the link holds, as [kaggle](../sources/kaggle.md) is for a competition, and a link whose file only its server names is asked for that file's first byte. stderr from a git repository with no manifest yet, from a test snapshot, where `[port]` stands for the test server's port:

```text
      Would pin iris, new in data.lock: http://127.0.0.1:[port]/files/iris.csv · 1 file · 32 B
      Would fetch iris/iris.csv · 32 B
      Would place data/iris · 1 file
      Would add data/ to .gitignore, since its files come from data.lock
      Would add iris to data.toml, its files in data/iris
         Ok dry run, nothing was changed
```

`add <path> --dry-run` reads no file's bytes: it names the files it would import, their count and size, and the dataset it would add, and lists their places in `changes.place`. it cannot tell yet whether the same files are in data.toml already.

the plan covers the datasets the add adds, as the real addition does. a file whose hash only a download can tell counts as one to place, or as one to check, under `Would check`, when a file already stands where it goes. an archive not yet unpacked here is listed with `Would open`, without its members. a link whose file name only the server gives, as a plain link's, is asked for the file's first byte, so the dry run names the dataset and its file as the add will, with the size the server states, and fails where the add would, as when nothing is at the link; with `--select` or `--exclude`, whose patterns match the link's own name, it keeps naming the file after the link. downloads the cache's disk lacks room for get a `space.short` warning, as the question the add would ask before them says. a web page in place of a file fails the dry run as it fails the add, as [URLs and DOIs](../sources/urls-and-dois.md#dry-runs) shows. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## generated names

the [dataset naming rules](../reference/data-toml.md#dataset-names) choose a name from a file, title or reference and handle collisions; `--as` chooses your own. several links take no `--as`, so when a link's name is taken its warning says to rename the dataset's table in data.toml afterwards, and the next sync moves its pin to the new name.

## exit codes

0 means the add succeeded, also when sync kept local conflicts, which JSON lists in `result.placement.kept`. 1 means a question went unanswered or was answered no: a large download, or a project in the home folder or at a disk's root. 2 means invalid usage, a path that names no file or folder among it. 10 includes an import's files this machine's cache lacks, and 50 a supplied folder holding other files than data.lock pins. the others are the [shared exit codes](../reference/exit-codes.md).
