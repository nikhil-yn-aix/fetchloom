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
| `version` | integer `1`, the one format; a dataset may keep its files in [`data.lock.d`](#big-datasets) |
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

fetchloom reads and writes format 1 only. a lock with a higher `version` comes from a newer fetchloom and asks you to update. any other version, or a lock that does not parse, is broken: restore it from git, or delete it before `fetchloom lock` rebuilds it. rebuilding can pin newer data.

## a git merge conflict

a git merge that could not combine two changes leaves both sides in the file between conflict lines, `<<<<<<<`, `=======` and `>>>>>>>`. every command then stops with `project.invalid`, exit 50, saying `data.lock has an unfinished git merge conflict` and naming the first such line; `data.toml` gets the same. keep the datasets from both sides, delete those lines, then run `fetchloom lock`, which pins any dataset data.lock still lacks. with git's `diff3` conflict style, delete the older lines between `|||||||` and `=======` as well. where both sides changed one dataset, keep one side's entry for it. the JSON error has no `fix`, since only a person can choose what each side keeps.

## big datasets

a dataset of more than 50,000 files keeps them beside `data.lock`, in compressed parts in the folder `data.lock.d`, so `data.lock` stays small enough to read and review and git takes every part under its size limits. the rule is part of the format, not a setting, so every machine writes the same lock. commit `data.lock.d` with `data.lock`; a clone without it cannot read the lock.

```toml
version = 1

[[dataset]]
name = "photos"
ref = "hf:datasets/org/photos"
spec = "sha256:..."
resolved = "hf:datasets/org/photos@3c1f0e2..."
locked = "2026-11-10T00:00:00Z"
files_in = ["data.lock.d/photos.0123456789ab.1.tsv.zst", "data.lock.d/photos.0123456789ab.2.tsv.zst"]
file_count = 412000
file_bytes = 98765432100
files_sha256 = "sha256:0123456789ab..."
```

| field | value |
|---|---|
| `files_in` | the parts, in order, in place of `files` |
| `file_count` | the files the parts list |
| `file_bytes` | the sum of their sizes |
| `files_sha256` | `sha256:` and the SHA-256 of the parts' text, one part after another |

a part is a zstandard frame of UTF-8 text: a header line `path\tsize\tsha256\tmd5\turl\tunpacks_to`, then one line per file with the fields of an inline file table, sorted by path, at most 250,000 files a part. `md5` and `unpacks_to` are empty when absent, and `unpacks_to` is the tree hash without its `sha256:`. a backslash, tab or line break inside a path or URL is written `\\`, `\t`, `\n` or `\r`. a part's name holds the dataset's name and the first 12 digits of `files_sha256`, so a new list never overwrites one the lock on disk still names; parts no lock names are removed once data.lock no longer needs them. `zstdcat data.lock.d/photos.*.1.tsv.zst | head` shows one.

reading checks every part's name, line, order and count against `data.lock`, and the SHA-256 of the whole. a missing, changed or foreign part fails with `project.invalid`, and `cache gc` then keeps the whole cache rather than guess what the project needs. a list of more files than [`FETCHLOOM_MOST_FILES`](environment.md) allows is refused before it is read. a million files take about 46 bytes each, 47 MB in four parts; listed inline they would take about 300 MB.

## the spec hash

for the unfiltered iris entry above, SHA-256 hashes these UTF-8 bytes, including the final newline:

```text
ref=zenodo:3608990
extract=true
```

the `ref=` line contains the manifest reference. when patterns are present, add all `select=` lines next, then all `exclude=` lines. keep each list in written order; empty lists add no lines.

the final `extract=` value is `true` or `false`. `copy`, `path` and `project.data_dir` are excluded.

an unchanged spec keeps the existing lock entry unless an upgrade is requested. filter or extraction edits resolve the existing `resolved` reference. changing `ref`, deleting the lock or requesting `--upgrade` resolves the manifest reference instead. an explicitly versioned reference still names that version.

`resolved` is not always immutable. plain URLs, drive links, dropbox links, OSF nodes and bucket prefixes can change. in particular, changing filters on an OSF node can select newer file versions. the file hashes pin the bytes after resolution.

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
| kaggle competition | `kaggle:competitions/<name>`, without a version | `https://api.kaggle.com/v1/competitions/data/download/<name>/<file>` |
| S3 bucket | `s3://<bucket>/<prefix>` | the object's HTTPS URL on the endpoint that listed it |
| cloud storage bucket | `gs://<bucket>/<prefix>` | `https://storage.googleapis.com/<bucket>/<key>?generation=<generation>` |

DOIs route to one of these sources. hugging face resolves a branch or tag to its full commit id before listing files. kaggle encodes each file path as one URL segment. a competition file kaggle sent as a zip of itself is recorded at its path plus `.zip`, with `unpacks_to`.

drive locks keep the file id and resource key, without a confirmation form's fields. transfers start at `https://drive.usercontent.google.com/download?id=<id>&export=download&confirm=t`, with the resource key when needed. a confirmation page can supply the next request without changing the lock URL.

drive folders list no sizes. downloads supply the sizes written to the lock; [approval](../concepts.md#approval) checks a server's length, or the bytes received when no length is stated. google docs exports can change between downloads. select and exclude match those entries too; only a doc that remains selected stops the command. the error names each selected doc and explains how to exclude it, or you can ask the owner to upload a fixed file.

dropbox folders arrive as one zip, named by `Content-Disposition`. the lock pins the zip; extraction adds its tree hash. an unnamed HTML answer fails instead of entering the cache as dataset bytes.

source metadata keeps its casing. zenodo's iris fixture, for example, supplies `license = "cc-by-4.0"`. supported stated checksums are MD5 and SHA-256; dataverse ignores other checksum algorithms and learns SHA-256 from the download.

when adding or resolving a plain URL, project commands reject login details and recognized secret query keys. `get` permits secret query values for one-off downloads. its JSON file URLs and errors mask logins and recognized query secrets, including errors for malformed links. [local URL and partial records](cache-folder.md#url-and-partial-records) store a fingerprint of the original URL, not its raw text. cache payloads and other metadata can still contain private data.

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
