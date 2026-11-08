# add

`add` puts a dataset in your manifest, pins its files and syncs the whole project.

```sh
fetchloom add iris    # at a terminal, pick one from a list
fetchloom add https://huggingface.co/datasets/scikit-learn/iris
fetchloom add https://zenodo.org/records/3608990 --as flowers
fetchloom add hf:datasets/scikit-learn/iris --select "*.csv" --dry-run
```

## usage

```sh
fetchloom add [options] <link>
```

| option | effect |
|---|---|
| `--as <name>` | choose the dataset name and its default folder under `data/` |
| `--select <glob>` | keep files matching any supplied pattern; repeat for more patterns |
| `--exclude <glob>` | drop matching files; repeat for more exclusions |
| `--no-extract` | keep archives packed and record `extract = false` |
| `-n`, `--dry-run` | plan the addition without applying project or cache changes |

[global options](README.md#global-options) also apply. [sources](../sources/README.md) lists accepted links and references. plain words use [catalog search and selection](search.md#choosing-a-dataset); quote several words, as in `fetchloom add "fashion mnist"`. the manifest stores the chosen canonical id, not the search words.

## what happens

1. find the nearest project manifest. without one, the current folder becomes a new project, and a `Started a project in <folder>` line says so at the end. in your home folder or at the root of a disk, where a project would take in every folder below it, `add` asks first; without a terminal it refuses unless `--yes` is given.
2. resolve the reference, filter its listed files and choose a dataset name.
3. download every file the whole project lacks into the cache and learn each hash, with the project still as it was.
4. write the entry in `data.toml`, pin selected files in `data.lock` and place the project's files from the cache.
5. in a git repository, add the data folder to `.gitignore` if needed.

run in a folder below a project's folder, `add` works on that project. a person at a terminal sees `using the project in <folder>` once, and the folder the dataset's files are in is given from where you stand, such as `../data/iris`.

there is no `init` command; `add` starts a project. `fetchloom init` is a usage error, exit 2, whose help says no init is needed and names `fetchloom add <link>`.

stderr for a single-file addition in a new project, matching a test snapshot. `[time]` replaces the measured duration and `[sandbox]` the test's folder:

```text
 Downloaded iris.csv · 32 B in [time]
    Started a project in [sandbox]/work
      Added iris, its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

commit `data.toml` and `data.lock`, not `data/`. names follow the [manifest rules](../reference/data-toml.md). a zenodo file-page link selects that file unless you supply `--select`.

adding an existing reference with identical filters and extraction settings syncs it without another entry, and in a git repository adds the data folder to `.gitignore` if needed. a resolved reference from an old lock identifies that entry only while its manifest reference is unchanged; editing `ref` does not make the old source count as already added. different settings fail unless you supply an unused `--as` name. edit `data.toml` to change an existing entry.

if a new addition fails to sync, fetchloom attempts to restore both `data.toml` and `data.lock`, including their absence before the command. it puts back only a file the add changed: an add that failed before changing either file reports no rollback. before changing an existing manifest or lock, fetchloom saves its exact bytes in a recovery file under `.fetchloom/`. if restoration fails, the error names that file without printing its contents, which may contain credentials. recovery files are removed after success or complete rollback and retained after incomplete rollback. on unix they are created readable only by the owner; on windows they inherit the metadata folder's permissions. cached downloads stay, so running the add again reuses them, while the files it placed for the new dataset are removed with it.

## stopping an add

ctrl-c stops an add and leaves `data.toml` and `data.lock` as they were. `add` writes them only once every file is in the cache, so an add stopped while it downloads, or ended at once by a second ctrl-c, has changed neither; one stopped while it places files puts both back, as a failed addition does. the add then ends with `Stopped, and nothing was added`, and a stopped download adds a line saying how much of it is kept. what arrived stays in the cache, so running the same add again continues where it stopped. exit code 130 means interrupted. once every file is placed the add is done, and a ctrl-c while it prints its last lines no longer stops it.

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

the plan covers the whole project, as the sync after a real addition would. a file whose hash only a download can tell counts as one to place, and an archive not yet unpacked here is listed with `Would open`, without its members. a link whose file name only the server gives is named from the link, since learning the name takes a download; pass `--as` to fix the name. adding an existing reference reports `<name> is already in data.toml`. JSON adds `dry_run` and `changes` to the usual result, as [JSON output](../reference/json-output.md#dry-runs) describes.

## generated names

the [dataset naming rules](../reference/data-toml.md#dataset-names) choose a name from a file, title or reference and handle collisions. use `--as` to choose your own.

## exit codes

0 means the command succeeded, including when sync kept local conflicts. inspect `result.placement.kept` in JSON. 1 means a question went unanswered or was answered no: approval of a large download, or of a project in the home folder or at a disk's root. 2 means invalid usage. source, network, integrity, access and local failures use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| a pattern matches nothing | it targets an archive member or a different source path | inspect listed paths and [pattern rules](../reference/data-toml.md#patterns) |
| a shell expands a pattern | the glob was not quoted | use `--select "**/*.csv"` |
| the same reference rejects new filters | the existing entry has different settings | edit its manifest entry, or use an unused `--as` name |
| adding fails for inline `datasets` | `add` cannot extend an inline table | use `[datasets.name]` tables |
| a large download stops in CI | it needs approval | check the size, then pass `--yes` if approved |
| `the project folder ... is read-only` | you may not write in the project folder | make the folder writable for you, then add again; on unix this is checked before anything downloads |
