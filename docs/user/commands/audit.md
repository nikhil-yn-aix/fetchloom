# audit

`audit` asks each source whether it still serves every file `data.lock` pins, whether it lists a newer version than the pinned one, and whether a dataset's title, license or access changed, downloading nothing.

```sh
fetchloom audit
fetchloom audit iris
fetchloom audit --check
fetchloom --json audit
```

## usage

```sh
fetchloom audit [options] [name]...
```

`name` is a dataset in `data.toml`; with none, every dataset is audited. an unknown name is a usage error, exit 2, that suggests the closest name.

| option | effect |
|---|---|
| `--check` | fail when a source lost or changed a pinned file, no longer lists a dataset, lists a newer version, or changed a title, license or access, naming each dataset and file, as [exit codes](#exit-codes) says. CI can run it on a fresh clone, which holds no data yet |

[global options](global-options.md) also apply.

## what it asks

for each file `data.lock` pins, one not downloaded yet too, audit asks its source for the first byte, as a download would, so nothing is downloaded. a file its source does not serve at its pinned size is then asked of each [mirror](../guides/mirrors.md) the run names, so a file only a mirror serves now is named under `Source lost`; a mirror holding an archive's list of files, as a push leaves it, serves that archive. a source that gives a file without its size serves it. once a host gives no answer, its other files are not asked and share its error.

for each dataset, audit asks its source what it lists at the pinned version, as `fetchloom lock` would, and, when the `ref` in `data.toml` names no version, what it lists now, as `fetchloom lock --upgrade --dry-run` would. a listing that gives a pinned file another SHA-256, or another MD5 than `data.lock` records, as a `SHA256SUMS` list or zenodo states them, shows its source no longer serves the pinned bytes, though their size held: the file is `changed` and asked of each mirror, as one served at another size is. where the listing states no checksum `data.lock` records, the size is all audit can compare. another version than the pinned one is `Newer`. a title, license or access the source states otherwise than `data.lock` is `Changed`; a fact only one side states is no change. a dataset imported from a folder has no source to ask, and only a mirror serves its files. a dataset that [waits for its file list](../reference/data-lock.md#big-datasets) is asked nothing, as its files are not known here: the run warns of it, its closing line ends `but photos waits for its file list and was not asked`, and `--check` fails for it.

each request goes where a sync or a lock would send it, as [privacy](../concepts/privacy.md) lists, and no file is downloaded. offline, audit asks nothing: `--offline` or `FETCHLOOM_OFFLINE` fails with `offline.missing`, exit 20, before any request.

## what it says

rows go to stdout: for each dataset with a file not served, not checked, or served by a mirror alone, one row with a row under it for each file, and one row for each newer version, change or dataset its source no longer lists. from a run whose zenodo record has a newer version and a new license:

```text
      Newer iris · zenodo:102, where data.lock pins zenodo:101
    Changed iris · license cc-by-4.0 → cc0-1.0
```

a dataset whose files are all served gets a line on stderr, as `Served iris · 1 file`, ending `· 1 answered without a size` when its server gave a file without saying its size, and the closing line then counts those apart from the files served at their pinned sizes. the closing line, hints and warnings go to stderr too; the hint names the command that takes a newer version, as ``hint: `fetchloom lock --upgrade iris` pins what their sources list now; check the change before you commit data.lock``. at a terminal each kind shows at most 20 files of a dataset, then how many more; `-v`, a pipe or a file shows them all.

## JSON

`result` has `project`; `files`, `served` and `unchecked`, the counts of the pinned files of the datasets audited; and `datasets`, a record for each dataset with its file counts, the files not served, not checked or served by a mirror alone, `newer`, `changed` and `unlisted`. [JSON output](../reference/json-output.md#command-results) gives every field. a failed `--check` keeps the result.

## exit codes

0 means audit completed, even when it found a change, so a script reads `result`, or asks for `--check`, which fails with the first that applies:

| finding | kind, exit |
|---|---|
| a file no mirror or source serves, as its source lost or changed it, or a file imported from a folder that no mirror holds; or a dataset its source no longer lists | `data.not_ready`, exit 1 |
| a newer version, or a title, license or access that changed | `lock.outdated`, exit 1 |
| a dataset that [waits for its file list](../reference/data-lock.md#big-datasets), so none of its files could be asked about | `project.invalid`, exit 50, in the words of the list's warning |
| a file or dataset its source refused or gave no answer about, as a dataset hugging face, kaggle, github or gitlab no longer shows to a request without a key, since they answer so for a private one | the error its download would meet, `auth.required` before `network.*` |

the error names each dataset and file in `details`, as `iris/iris.csv · gone: …` or `iris · newer version zenodo:102, where data.lock pins zenodo:101`, and its help the next step: `fetchloom push` from a machine whose cache holds the files, or `fetchloom lock --upgrade <name>`. a project with no data.lock fails `--check` with `data.not_ready`. other failures use the [shared exit codes](../reference/exit-codes.md).
