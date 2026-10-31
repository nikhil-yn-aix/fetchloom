# data.lock

`data.lock` pins every selected source file by SHA-256 and size.

a lock entry for the iris dataset, with no filters:

```toml
version = 1

[[dataset]]
name = "iris"
ref = "zenodo:3608990"
spec = "sha256:add8bf89a369d1494b5c180d377362cbdcd2d1c064f8867eb934aab59c1adaf2"
resolved = "zenodo:3608990"
locked = "2026-10-28T00:00:00Z"
files = [
  { path = "iris.csv", size = 4177, sha256 = "b1f0b46c3e27350cbf0f75fb3b7d36adcd997c443e764bf09255126feeb11f31", md5 = "395d6abd001abf3760a1a794ef4c453c", url = "https://zenodo.org/api/records/3608990/files/iris.csv/content" },
]
```

| field | value |
|---|---|
| `version` | integer `1` |
| `dataset` | array of dataset tables; omitted for an empty lock |
| `dataset[].name` | name in `data.toml` |
| `dataset[].ref` | the manifest reference |
| `dataset[].spec` | `sha256:` followed by 64 hex digits, computed below |
| `dataset[].resolved` | the source reference used to list the files |
| `dataset[].title`, `license`, `doi` | optional strings supplied by the source; omitted when absent |
| `dataset[].locked` | UTC timestamp, `YYYY-MM-DDTHH:MM:SSZ` |
| `dataset[].files` | selected source files, including archives as single files |
| `files[].path` | relative path inside the dataset, with `/` separators |
| `files[].size` | byte count, unsigned 64-bit integer |
| `files[].sha256` | 64 hex digits, without a prefix |
| `files[].md5` | optional source checksum, 32 hex digits |
| `files[].url` | download URL recorded before redirects |
| `files[].unpacks_to` | optional `sha256:` plus the 64-digit archive tree hash |

hashes are written in lowercase. datasets sort by name and files by path. the writer uses the field order above, inline file tables, LF newlines and a final newline. identical lock contents render identically; a fresh resolution can have a different timestamp.

`locked` stays unchanged when `resolved` and `files` stay unchanged, even after re-resolution.

`lock` can record a file without downloading it when its SHA-256 and size are known. an archive needs a tree hash before the lock can mark it for extraction.

a newer lock version asks you to update fetchloom. a broken or older lock must be restored from git, or deleted before `fetchloom lock` rebuilds it. rebuilding can pin newer data.

## the spec hash

for the unfiltered iris entry above, SHA-256 hashes these UTF-8 bytes, including the final newline:

```text
ref=zenodo:3608990
extract=true
```

the `ref=` line contains the manifest reference. when patterns are present, add all `select=` lines next, then all `exclude=` lines. keep each list in written order; empty lists add no lines.

the final `extract=` value is `true` or `false`. `copy`, `path` and `project.data_dir` are excluded.

an unchanged spec keeps the existing lock entry unless an upgrade is requested. filter or extraction edits resolve the existing `resolved` reference. changing `ref`, deleting the lock or requesting `--upgrade` resolves the manifest reference instead. an explicitly versioned reference still names that version.

`resolved` is not always immutable. plain URLs, drive links, dropbox links and OSF nodes can change. in particular, changing filters on an OSF node can select newer file versions. the file hashes pin the bytes after resolution.

## what each source records

| source | `resolved` | file `url` |
|---|---|---|
| plain URL | the parsed URL | the same URL |
| zenodo | `zenodo:<concrete record id>` | `https://zenodo.org/api/records/<id>/files/<name>/content` |
| figshare | `figshare:<id>@<version>` | the API's download URL |
| dataverse dataset | `dataverse:<host>/<persistent id>@<major.minor>`; the id includes `doi:` or `hdl:` | `/api/access/datafile/<id>?format=original` on that host |
| dataverse file | `dataverse:<host>/file:<numeric id>@<major.minor>`; a file PID resolves to its numeric id | `/api/access/datafile/<id>?format=original` on that host |
| OSF | `osf:<node or file id>` | the download link with a file version |
| drive file or folder | `drive:<id>` or `drive:folders/<id>`, with `?resourcekey=<key>` when supplied; `open?id=` resolves to one of these | `https://drive.google.com/uc?id=<id>`, with `resourcekey` when supplied |
| dropbox | the shared URL with `dl=1` | the same URL |
| hugging face | `hf:<kind>/<repo>@<commit>` | the repository's `/resolve/<commit>/<path>` URL on `huggingface.co` |
| kaggle | `kaggle:<owner>/<dataset>@<version>` | `https://api.kaggle.com/v1/datasets/download/<owner>/<dataset>/<file>?datasetVersionNumber=<version>&raw=true` |

DOIs route to one of these sources. hugging face resolves a branch or tag to its full commit id before listing files. kaggle encodes each file path as one URL segment.

drive locks keep the file id and resource key, without a confirmation form's fields. transfers start at `https://drive.usercontent.google.com/download?id=<id>&export=download&confirm=t`, with the resource key when needed. a confirmation page can supply the next request without changing the lock URL.

drive folders list no sizes. downloads supply the sizes written to the lock; [approval](../concepts.md#approval) checks a server's length, or the bytes received when no length is stated. google docs exports can change between downloads. select and exclude match those entries too; only a doc that remains selected stops the command. the error names each selected doc and explains how to exclude it, or you can ask the owner to upload a fixed file.

dropbox folders arrive as one zip, named by `Content-Disposition`. the lock pins the zip; extraction adds its tree hash. an unnamed HTML answer fails instead of entering the cache as dataset bytes.

source metadata keeps its casing. zenodo's iris fixture, for example, supplies `license = "cc-by-4.0"`. supported stated checksums are MD5 and SHA-256; dataverse ignores other checksum algorithms and learns SHA-256 from the download.

when adding or resolving a plain URL, project commands reject login details and recognized secret query keys. `get` permits secret query values for one-off downloads. its JSON file URLs and errors mask recognized query secrets, including errors for malformed links. local URL records keep the raw links, so do not share those records.

masking recognizes key names such as `token`, `signature` and `sig`; it cannot identify an arbitrary secret.

## the tree hash

`unpacked/<archive sha256>` contains one UTF-8 line per file, sorted by path:

```text
<path>\t<size in bytes>\t<file sha256>\n
```

`\t` and `\n` above mean a literal tab and LF. paths are NFC, with `/` separators and leading `./` removed. SHA-256 of these exact bytes is `unpacks_to`. there is no header or byte order mark. an empty tree hashes zero bytes.

paths in this record retain their archive folders; placement may strip a shared top folder. directory entries, times and permissions are absent. empty directories are not placed.

## encoding and validation

locks are UTF-8 TOML. a byte order mark is accepted on read and omitted on write. unknown keys fail, and the parser lists the allowed fields. duplicate dataset names are refused with `project.invalid` and lock-recovery help. restore the intended lock rather than relying on one duplicate to win.
