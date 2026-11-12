# data.lock

`data.lock` pins every selected source file by SHA-256 and size.

a lock entry for the iris dataset, with no filters:

```toml
version = 1

[[dataset]]
name = "iris"
ref = "zenodo:3608990"
spec = "sha256:7e24dc80010999fe1ec2eaa0e602e1a3af375a2a5bd0e9d70173e62871f56744"
resolved = "zenodo:3608990"
locked = "2026-10-28T00:00:00Z"
files = [
  { path = "iris.csv", size = 4177, sha256 = "b1f0b46c3e27350cbf0f75fb3b7d36adcd997c443e764bf09255126feeb11f31", md5 = "395d6abd001abf3760a1a794ef4c453c", url = "https://zenodo.org/api/records/3608990/files/iris.csv/content" },
]
```

| field | value |
|---|---|
| `version` | integer `1`, the one format; a dataset may keep its files in [`data.lock.d`](#big-datasets) |
| `dataset` | array of dataset tables; omitted for an empty lock. a dataset a recipe made has the keys under [recipe results](#recipe-results) instead of those below |
| `dataset[].name` | name in `data.toml` |
| `dataset[].ref` | the manifest reference |
| `dataset[].spec` | `sha256:` followed by 64 hex digits, computed below |
| `dataset[].extract` | `false` when `data.toml` keeps the dataset's archives packed; omitted when they unpack |
| `dataset[].resolved` | the source reference used to list the files, or `local` for an import |
| `dataset[].title`, `license`, `doi` | optional strings supplied by the source; omitted when absent |
| `dataset[].access` | who may read the data, as the source said: `public`, `login` for an account, or `terms` for accepted terms; omitted when the source said nothing, which means unknown |
| `dataset[].locked` | UTC timestamp, `YYYY-MM-DDTHH:MM:SSZ` |
| `dataset[].files` | selected source files, including archives as single files |
| `files[].path` | relative path inside the dataset, with `/` separators |
| `files[].size` | byte count, unsigned 64-bit integer |
| `files[].sha256` | 64 hex digits, without a prefix |
| `files[].md5` | optional source checksum, 32 hex digits |
| `files[].url` | download URL recorded before redirects; empty for an imported file, which no source holds |
| `files[].unpacks_to` | optional `sha256:` plus the 64-digit archive tree hash |

hashes are written in lowercase. datasets sort by name and files by path. the writer uses the field order above, inline file tables, LF newlines and a final newline. identical lock contents render identically; a fresh resolution can have a different timestamp.

`locked` stays unchanged when `resolved` and `files` stay unchanged, even after re-resolution.

`lock` can record a file without downloading it when its SHA-256 and size are known. an archive needs a tree hash before the lock can mark it for extraction.

fetchloom reads and writes format 1 only. a lock with a higher `version` comes from a newer fetchloom and asks you to update. any other version, or a lock that does not parse, is broken: restore it from git, or delete it before `fetchloom lock` rebuilds it. rebuilding can pin newer data. a `version` that is no whole number names no format, so the error says what it holds instead; for `version = "1"` that is `data.lock cannot be read: version is text in quotes, not a whole number such as 1`.

## a git merge conflict

a git merge that could not combine two changes leaves both sides in the file between conflict lines, `<<<<<<<`, `=======` and `>>>>>>>`. every command then stops with `project.invalid`, exit 50, saying `data.lock has an unfinished git merge conflict` and naming the first such line; `data.toml` gets the same. its help, and the JSON `fix`, name `fetchloom lock`, which [merges both files](../commands/lock.md#after-a-git-merge) one dataset at a time without downloading: each side of `data.lock` must read on its own, a pin is kept only when it answers the merged `data.toml`, and two pins of the same files made at different times are alike. a dataset each side pinned its own way waits for `--ours <name>` or `--theirs <name>`.

the lists in `data.lock.d` are named by their contents, so a merge brings each branch's new parts in beside the others without a conflict. the merge reads only the parts of the pins it keeps, which may be all git left of a side, and removes the parts no pin names once `data.lock` is written.

## big datasets

a dataset of more than 50,000 files keeps them beside `data.lock`, in compressed parts in the folder `data.lock.d`, so `data.lock` stays small enough to read and review and git takes every part under its size limits. the rule is part of the format, not a setting, so every machine writes the same lock. commit `data.lock.d` with `data.lock`; a clone without it cannot read the lock, and says so: `data.lock names data.lock.d/photos.2e682d3ff7a4.tsv.zst, which is not there`, with help to get the commit that adds data.lock.d. a list is named by its content, so a new one is a new file, which `git commit -a` leaves out: in a git repository, `status` warns of each list data.lock names that git does not have, and a command that writes one ends with `git add data.lock.d`.

the entry of a real run that imported a folder of 50,001 small files:

```toml
version = 1

[[dataset]]
name = "photos"
ref = "local"
spec = "sha256:b52165b04be5b07b1a5050f8bd2a189fd4dc1acbecb65af3e85c3a2c4157b538"
resolved = "local"
locked = "2026-11-12T10:25:38Z"
files_in = ["data.lock.d/photos.2e682d3ff7a4.tsv.zst"]
file_count = 50001
file_bytes = 288896
files_sha256 = "sha256:2e682d3ff7a415d2fa4340c6b13454e3d6ac7cce7e11119505ad0b55821e05d7"
```

| field | value |
|---|---|
| `files_in` | the parts, in order, in place of `files`; more than one part takes a number after the hash, as `photos.2e682d3ff7a4.1.tsv.zst` |
| `file_count` | the files the parts list |
| `file_bytes` | the sum of their sizes |
| `files_sha256` | `sha256:` and the SHA-256 of the parts' text, one part after another |

a part is a zstandard frame of UTF-8 text: a header line naming its columns, then one line per file, sorted by path, at most 250,000 files a part. a source's list has the header `path\tsize\tsha256\tmd5\turl\tunpacks_to` and the fields of an inline file table; a [recipe result](#recipe-results)'s list has the header `path\tsize\tsha256`. reading expects the header of the dataset's kind, so one kind of list never stands for the other. `md5` and `unpacks_to` are empty when absent, and `unpacks_to` is the tree hash without its `sha256:`. a backslash, tab or line break inside a path or URL is written `\\`, `\t`, `\n` or `\r`. a part's name holds the dataset's name and the first 12 digits of `files_sha256`, so a new list never overwrites one the lock on disk still names; parts no lock names are removed once data.lock no longer needs them. `zstdcat data.lock.d/photos.*.tsv.zst | head` shows one.

reading checks every part's name, line, order and count against `data.lock`, and the SHA-256 of the whole. a missing, changed or foreign part fails with `project.invalid`, and `cache gc` then keeps the whole cache rather than guess what the project needs. a list of more files than [`FETCHLOOM_MOST_FILES`](environment.md) allows is refused before it is read. a million files take about 46 bytes each, 47 MB in four parts; listed inline they would take about 300 MB.

## recipe results

a table with `from` and `run` in place of `ref` pins a dataset a [recipe](../recipes.md) made: the files a command wrote from other datasets. only `fetchloom build` writes these tables, once every step of the build is done, after the result's files are in the cache; every other command reads and checks them, keeps one as it is while `data.toml` has its recipe and drops it with the recipe, and `sync` places a result's files like a fetched dataset's. `locked` moves only when the key or the tree changes, so a build that makes the same files again leaves the table as it is. a result always keeps its files in `data.lock.d`, since a rebuild replaces them all at once and `data.lock` then shows only what the recipe and its pins changed.

from a real build:

```toml
[[dataset]]
name = "clean"
from = [
  { name = "iris", content = "sha256:44b3e8323e385e61743eade568bb1ca54b660cc33aadd89a324b80a467b8a2a9" },
]
run = ["python3", "clean.py", "{iris}", "{out}"]
code = [
  { path = "clean.py", sha256 = "d71b468a0dc9d58310667feaf6b814d38d05f1e5e4d7a63d3be4e1f609f53a3e" },
  { path = "lib", tree = "sha256:0a0b56cbee4ab6f62bc733bfe4378f682d6a5c8d598bbf70c4d1dcf7a694120c" },
]
env = { SOURCE_DATE_EPOCH = "315532800" }
key = "sha256:d764536988d07c1808714c1d822acd09625f068427e816f05f01867649c3015d"
locked = "2026-11-12T10:25:28Z"
tree = "sha256:0b2cd7241af67f42241767d525012f7e7fb6eab90c364cc97aec8edf6f21d21b"
files_in = ["data.lock.d/clean.48557b15b131.tsv.zst"]
file_count = 1
file_bytes = 366
files_sha256 = "sha256:48557b15b131a59c6aff11e5019b52425a87400b3ddadc974a5c046735ea9048"
```

| field | value |
|---|---|
| `from` | the datasets it is made from, in `data.toml` order, each with its `name` and the `content` hash the key took, `sha256:` and 64 hex digits |
| `run` | the command as `data.toml` writes it, with `{name}` and `{out}` not filled in |
| `code` | the project files and folders whose bytes entered the key, sorted by path: a file with its `sha256`, 64 hex digits without a prefix, a folder with the `tree` of its files, `sha256:` and 64 hex digits; `[]` when there are none. [recipes](../recipes.md#what-fetchloom-keys) says how each is hashed |
| `env` | the variables set for the command, by name; omitted when there are none |
| `key` | `sha256:` and the hash of everything the result was made from, as [recipes](../recipes.md#what-fetchloom-keys) lists it |
| `locked` | when the key or the tree last changed, in UTC |
| `tree` | `sha256:` and the [tree hash](#the-tree-hash) of the files the result holds |
| `files_in`, `file_count`, `file_bytes`, `files_sha256` | the result's list in `data.lock.d`, as for a [big dataset](#big-datasets) |

a file's own hash has no prefix; a hash of something else, such as an input's content, a key, a tree or a list, starts with `sha256:`. a table that mixes these keys with those of a fetched dataset (`ref`, `spec`, `extract`, `resolved`, `title`, `license`, `doi`, `access` or `files`) is refused with `project.invalid`, as is one with no input, an input named twice or after the dataset itself, an empty program, a code path that leaves the project or comes twice, code with both or neither of `sha256` and `tree`, a variable whose name holds `=`, a NUL character in the command or a variable, or a list whose files do not hash to `tree`.

each input's `content` is what the recipe read of it. for a fetched dataset it is the SHA-256 of one line a pinned file, sorted by path: `path`, `size` and `sha256` separated by tabs, then a tab and `sha256:` with the tree hash for an archive that unpacks, and a newline. a [split archive](#split-archives) is one line, at its first part, so the same files cut into other parts keep the content. for a recipe's result it is the result's `tree`.

## the spec hash

for the unfiltered iris entry above, SHA-256 hashes these UTF-8 bytes, including the final newline:

```text
ref=zenodo:3608990
```

the `ref=` line contains the manifest reference. when patterns are present, add all `select=` lines next, then all `exclude=` lines. keep each list in written order; empty lists add no lines.

`extract`, `members`, `copy`, `path` and `project.data_dir` are excluded, since they decide what happens to the downloaded files, not which files are downloaded.

an unchanged spec and `extract` keep the existing lock entry unless an upgrade is requested. when only `extract` changed, the entry keeps every pinned file and learns again only what its archives unpack to: from this machine's record of an earlier unpacking, or from the cached archive, so the source is not listed again and a `--offline` run works while the archives are cached. an archive the cache lacks is downloaded by its pinned link. filter edits resolve the existing `resolved` reference. changing `ref`, deleting the lock or requesting `--upgrade` resolves the manifest reference instead. an explicitly versioned reference still names that version.

sync drops an archive's own bytes once its files are in the cache, unless another project here keeps it packed, so turning `extract` off after unpacking downloads the archives again by their pinned links, and `--offline` names them as missing. for webdataset shards, choose before the first sync: the hint that [webdataset shards](../concepts.md#webdataset-shards) shows comes before anything downloads.

`resolved` is not always immutable. plain URLs, drive links, dropbox links, OSF nodes and bucket prefixes can change. in particular, changing filters on an OSF node can select newer file versions. the file hashes pin the bytes after resolution.

## what each source records

| source | `resolved` | file `url` |
|---|---|---|
| plain URL | the parsed URL | the same URL |
| zenodo | `zenodo:<concrete record id>` | `https://zenodo.org/api/records/<id>/files/<name>/content` |
| figshare | `figshare:<id>@<version>` | the API's download URL |
| dataverse dataset | `dataverse:<host>/<persistent id>@<major.minor>`; the id includes `doi:` or `hdl:` | `/api/access/datafile/<id>?format=original` on that host |
| dataverse file | `dataverse:<host>/file:<numeric id>@<major.minor>`; a file PID resolves to its numeric id | `/api/access/datafile/<id>?format=original` on that host |
| dryad | `dryad:<doi>@<version number>` | `https://datadryad.org/api/v2/files/<id>/download` |
| OSF | `osf:<node or file id>` | the download link with a file version |
| drive file or folder | `drive:<id>` or `drive:folders/<id>`, with `?resourcekey=<key>` when supplied; `open?id=` resolves to one of these | `https://drive.google.com/uc?id=<id>`, with `resourcekey` when supplied |
| dropbox | the shared URL with `dl=1` | the same URL |
| hugging face | `hf:<kind>/<repo>@<commit>` | the repository's `/resolve/<commit>/<path>` URL on `huggingface.co` |
| kaggle | `kaggle:<owner>/<dataset>@<version>` | `https://api.kaggle.com/v1/datasets/download/<owner>/<dataset>/<file>?datasetVersionNumber=<version>&raw=true` |
| kaggle competition | `kaggle:competitions/<name>`, without a version | `https://api.kaggle.com/v1/competitions/data/download/<name>/<file>` |
| S3 bucket | `s3://<bucket>/<prefix>`, or `s3:https://<host>/<bucket>/<prefix>` at another service | the object's HTTPS URL on the endpoint that listed it, with `?versionId=<id>` when the bucket keeps versions |
| cloud storage bucket | `gs://<bucket>/<prefix>` | `https://storage.googleapis.com/<bucket>/<key>?generation=<generation>` |
| [imported files](data-toml.md#imported-files) | `local` | empty |

an import records no path of the machine it came from, and no link: its bytes are in the caches of the machines that imported or were given them. a machine whose cache lacks them stops before any download with `reference.unresolved`, exit 10. files copied to the dataset's folder are adopted when they hold the pinned bytes, and `fetchloom add <folder> --as <name>` checks a folder against the pins before it copies anything in. an import leaves out `access`, which is then unknown.

`access` is a hint for sharing the files, never a license check: [`push`](../commands/push.md#data-you-may-not-share) asks before copying a dataset whose `access` is not `public` into a mirror. hugging face records `terms` for a gated repository, `login` for a private one and `public` when the hub says it is neither. kaggle records `terms` for a competition, whose data comes under its rules, `login` for a private dataset and `public` for a public one. dryad records `public` for a version it shows to anyone and `login` for one it restricts, though its downloads always need a key. other sources leave it out.

DOIs route to one of these sources. hugging face resolves a branch or tag to its full commit id before listing files. kaggle encodes each file path as one URL segment. a competition file kaggle sent as a zip of itself is recorded at its path plus `.zip`, with `unpacks_to`.

drive locks keep the file id and resource key, without a confirmation form's fields. transfers start at `https://drive.usercontent.google.com/download?id=<id>&export=download&confirm=t`, with the resource key when needed. a confirmation page can supply the next request without changing the lock URL.

drive folders list no sizes. downloads supply the sizes written to the lock; [approval](../concepts.md#approval) checks a server's length, or the bytes received when no length is stated. google docs exports can change between downloads. select and exclude match those entries too; only a doc that remains selected stops the command. the error names each selected doc and explains how to exclude it, or you can ask the owner to upload a fixed file.

dropbox folders arrive as one zip, named by `Content-Disposition`. the lock pins the zip; extraction adds its tree hash. an unnamed HTML answer fails instead of entering the cache as dataset bytes.

source metadata keeps its casing. zenodo's iris fixture, for example, supplies `license = "cc-by-4.0"`. supported stated checksums are MD5 and SHA-256; dataverse ignores other checksum algorithms and learns SHA-256 from the download.

when adding or resolving a link, project commands reject login details and recognized secret query or fragment keys. that holds for a link a source reads by its own reference too, such as a zenodo record's: the reference needs no token, but data.toml keeps the link as written and data.lock copies it, so the help names the reference to use instead. reading `data.lock` refuses a link that holds them, as [encoding and validation](#encoding-and-validation) says. `get` permits secret query values for one-off downloads. its JSON file URLs and errors mask logins and recognized query secrets, including errors for malformed links. [local URL and partial records](cache-folder.md#url-and-partial-records) store a fingerprint of the original URL, not its raw text. cache payloads and other metadata can still contain private data.

masking recognizes key names such as `token`, `signature` and `sig`; it cannot identify an arbitrary secret.

## split archives

an archive cut into parts is pinned as its parts, each an ordinary file, with `unpacks_to` on the first part alone: the tree of the archive the parts make read in order. no field says which files are parts; their names do, so a reader or `export` finds them again:

- parts share one dataset, one folder and the path before their counters, which names an archive fetchloom unpacks: `.zip`, `.7z`, `.tar` or a compressed tar. a file whose own name is such an archive is never a part.
- between that path and the counter comes `.`, `.part`, `.part-`, `.part_` or `.part.`. a counter is digits or lowercase letters, at least two of them, though one digit may follow a `part`.
- counters sort by length, then as text, and follow one numbering from the first part: zero-padded numbers from 0 or 1, growing past their width as 7-Zip's `.1000` follows `.999`; letters from all `a`s, of one width; or GNU split's own, two characters until the first would be the alphabet's last, which then stays as two more follow, so `9000` follows `89` and `zaaa` follows `yz`.

with `extract = false`, or a part missing, no part carries `unpacks_to` and each is a plain file. rar volumes and zips spanned over `.z01` files are no cut of the bytes and stay packed. the cache keeps what a split archive unpacked to under the key its parts give, the SHA-256 of `fetchloom parts`, a line break, then for each part in order its SHA-256 in hex, a space, its size and a line break, so parts that changed, or one more part, never find the record of other parts. for `headers.zip.00` to `.02`, these bytes hash to `d8efb6b636f926170ecc1d70df2e6fd62103f9360a5fcf9ae118e38de147fd1f`, the name of its record under `unpacked/`:

```text
fetchloom parts
8bb11984d17650386b7085dc6c010d62e8bb8a17f6209b9177a19aa80373fde4 262144
3f47d2ea3ee9bacb5e04f7d8af1b4040bea0e47fe825ed10e72270b516eb3906 262144
46dbe106e1dfdeab8bfc04d7d8175a352731fa9076ad855b52d95dd017749dc9 242241
```

## the tree hash

`unpacked/<archive sha256>`, or for a [split archive](#split-archives) the key its parts give, contains one UTF-8 line per file, sorted by path:

```text
<path>\t<size in bytes>\t<file sha256>\n
```

`\t` and `\n` above mean a literal tab and LF. paths are NFC, with `/` separators and leading `./` removed. SHA-256 of these exact bytes is `unpacks_to`. there is no header or byte order mark. an empty tree hashes zero bytes.

a [recipe result](#recipe-results)'s `tree` hashes its files the same way, so the cache can keep both kinds of tree alike.

paths in this record retain their archive folders; placement may strip a shared top folder. directory entries, times and permissions are absent. empty directories are not placed. files a system writes by itself, such as finder's `.DS_Store` and the `._` files under `__MACOSX`, are left out of the record, so they are in neither the tree nor what a [members table](data-toml.md#files-inside-archives) matches.

## encoding and validation

locks are UTF-8 TOML. a byte order mark is accepted on read and omitted on write. unknown keys fail, and the parser lists the allowed fields. duplicate dataset names are refused with `project.invalid` and lock-recovery help. restore the intended lock rather than relying on one duplicate to win. a dataset name data.toml would refuse, such as `../outside`, is refused the same way, since a name also names its list's parts in `data.lock.d`.

each file's `url`, and `resolved` when it is an http or https link, are the links a request uses. neither may hold a login or a recognized secret such as `token=`: fetchloom never pins one, so a lock that holds one was edited, and every command that reads it stops with `project.invalid`, exit 50, before any request. the error names the key, never its value, and its JSON `fix` is null. on stderr:

```text
error: the link data.lock gives x/extra-a.txt carries token, a secret fetchloom never pins
  help: remove token and its value from that link in data.lock, or restore data.lock from git
```

a list in `data.lock.d` that holds such a link comes back from git with `data.lock` instead. an import's empty `url`, and a query that grants nothing, such as `?download=1` or `?versionId=`, read as written.

a file's `size` must be the size of the bytes its `sha256` names. fetchloom compares the two where this machine already holds checked bytes, a cached copy with that SHA-256 or a placed file that hashes to it, and reads only a file whose size differs from its pin, so sound pins cost no extra reading. when they disagree, data.lock was edited or damaged, and no source or mirror can send what it pins: `status`, `lock --check`, `sync` and `push` stop with `project.invalid`, exit 50, and a null JSON `fix`. on stderr:

```text
error: data.lock pins small/small.csv at 13 bytes, but its SHA-256 is that of 12 bytes on this machine
  help: data.lock was edited by hand or damaged: restore it from git, or set the size of small/small.csv to 12 in data.lock
```

missing or damaged bytes on this machine say nothing against the pin. `fetchloom lock --upgrade <name>` pins what the source serves now, which may be other bytes, and fetchloom never rewrites a pin's size by itself.
