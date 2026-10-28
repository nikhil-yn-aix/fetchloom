# scope

v0.1 is the smallest fetchloom that serves the two groups [vision](vision.md) puts first.

## commands

| command | does |
|---|---|
| `fetchloom add <link>` | adds a dataset to `data.toml`, locks it and syncs it |
| `fetchloom remove <name>` | drops a dataset from `data.toml` and `data.lock` and deletes the files fetchloom placed for it. files you changed or added stay, and are listed |
| `fetchloom sync` | makes `data/` match `data.lock`, locking first when `data.toml` changed |
| `fetchloom lock` | brings `data.lock` in line with `data.toml`. `--upgrade` asks each source for its newest version and shows what changed. `--check` fails when `data.lock` is out of date, for CI |
| `fetchloom status` | lists files that are missing, changed by hand or extra |
| `fetchloom path <name>` | prints where a dataset or a file inside it is, syncing first if needed |
| `fetchloom get <link>` | fetches into a new folder named after the dataset, or into `-o <dir>`, through the same cache, and prints the folder's path. it refuses a folder that already holds other files |
| `fetchloom cache` | shows where the cache is, how big it is and which projects use it. `cache gc` frees what no project needs. `cache verify` hashes every object again and removes any that changed |
| `fetchloom` | inside a project, a short status. outside, a welcome with one command to try |

every command takes `-q`, `-v`, `--json`, `--offline`, `--yes`, `--color` and `-C <dir>`. `add` and `get` also take `--select <glob>`, `--exclude <glob>` and `--no-extract`. `add` takes `--as <name>` to name the dataset, and `get` takes `-o <dir>` to choose its folder. `fl` is a short name for the same program. powershell already uses `fl` for `Format-List`, so use `fetchloom` there.

## sources

| reference | example | checksum the source states |
|---|---|---|
| a URL | `https://host/file.tar.gz`, including github release assets | none |
| a DOI | `doi:10.5281/zenodo.3608990`, or a pasted doi.org link | whatever the source it lands on states |
| zenodo | `zenodo:3608990` | MD5 |
| figshare | `figshare:34744689` | MD5 |
| dataverse | `dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP` | MD5 by default |
| osf | `osf:abc12` | SHA-256 and MD5 |
| hugging face | `hf:datasets/org/name@revision` | SHA-256 for large files |
| kaggle | `kaggle:owner/dataset` | none |
| google drive | a file or folder link, pasted as is | none |
| dropbox | a file or folder link, pasted as is | none |

when a source states no checksum, the first download defines the SHA-256, and every later download must match it.

## what v0.1 guarantees

- before any download, fetchloom shows what a reference resolved to: title, version, file count, size and license. on a terminal it asks before downloading more than 1 GiB. elsewhere it stops and names `--yes`, which approves the download.
- every file is pinned by SHA-256 and size. a checksum the source states is checked as well.
- a dataset can list up to 100,000 files, and an archive can hold up to 1,000,000.
- downloads run in parallel with a limit per host, resume after an interruption, retry what can still succeed and respect rate limits.
- a timeout limits silence on a connection, never the length of a download.
- credentials come from where each source already keeps them (`HF_TOKEN`, `~/.kaggle/kaggle.json`, `ZENODO_TOKEN`) and go only to that source's host, only over https.
- zip and tar archives (tar, tar.gz, tar.xz, tar.zst, tar.bz2) are unpacked unless `data.toml` says `extract = false`, or `--no-extract` keeps them packed. an unsafe archive is refused as a whole. single compressed files like `.fastq.gz` stay as they are.
- one cache per machine, with files named by SHA-256 and stored read-only. projects get them by reflink, else hardlink, else copy. no symlinks, so windows needs no admin rights.
- inside a folder that onedrive, dropbox or icloud drive syncs, files are copied instead of linked, because those tools break hardlinks. a warning says what that costs and that moving the project out of the synced folder avoids it.
- several fetchloom processes can use one cache at the same time.
- a file someone edited by hand is never overwritten. `status` shows it, and `sync --force` replaces it.
- when upstream serves bytes that differ from `data.lock`, sync stops, names the file and gives the fix: `fetchloom lock --upgrade <name>`.
- when a recorded link fails, the source is asked again, since a DOI or dataset id usually still resolves. whatever comes back is checked against the pinned hash.
- `--offline` makes no network request of any kind and uses only the cache.
- the cache can live in any folder (`FETCHLOOM_CACHE`). in v0.1 one person's processes share it safely. one cache for a whole lab comes with the lab features.
- python: `fetchloom.path("name")` returns a dataset's path, syncing first, by calling the binary.
- output is friendly by default, `-q` prints only the answer, `-v` adds detail and timings, `--json` is for programs. every error says what happened and what to do next.

## platforms and install

- windows x64, macos arm64 and x64, linux x64 and arm64.
- `uvx fetchloom`, `pipx install fetchloom`, `pip install fetchloom`, and standalone binaries with shell and powershell installers.

## done means

each scenario runs as an end-to-end test on windows, macos and linux against recorded responses, and once by hand against the real sources.

1. `uvx fetchloom get <zenodo link>` downloads, verifies, unpacks and prints the folder. running it again moves zero bytes.
2. an author adds a drive link, a zenodo dataset, a hugging face dataset and a kaggle dataset, then commits `data.toml` and `data.lock`.
3. a fresh clone runs `fetchloom sync` and gets byte-identical files.
4. a second project with the same datasets syncs with zero bytes downloaded, within the budgets in [performance](performance.md).
5. an upstream file changes. sync stops, names it and gives the fix.
6. the recorded link fails but the source still has the file. sync succeeds and says where the file came from.
7. a hand-edited file shows in `status` and survives `sync`.
8. `fetchloom sync --offline` works with a warm cache and the network unplugged. with a cold cache it lists what is missing.
9. a download killed halfway resumes and asks only for the missing bytes.
10. `fetchloom.path("eeg")` in a notebook returns a path.
11. on windows without admin rights, inside onedrive, everything works and the warning shows once.

## not in v0.1

| left out | why |
|---|---|
| preprocessing steps, `run`, `why` | that is a build system. snakemake, nextflow and make do it |
| `publish`, `search`, `copy`, `push`, `pack` | other tools already do these jobs |
| plugins | sources live in this repository until a real need says otherwise |
| `cite` | small and useful, so it is the first candidate after v0.1 |
| s3, gcs, azure, ftp and sftp sources | no evidence of need yet |
| lab features: group permissions, gc across users | the next audience, not the first |
| fallbacks to the web archive and software heritage | the basics have to hold first |
| 7z and rar | 7z is rare, and the only rar decoder is not open source. both are refused with a message naming a tool that opens them |
| facets, samples, size filters | `--select` and `--exclude` globs cover the need |
| an R package, fsspec, snakemake, nextflow, a github action | they come after the CLI is stable |
| login flows, `doctor`, shell completions | environment variables and each source's own token file are enough |
| parallel range requests for one file | measure first |

## after v0.1

candidates in the order the evidence suggests. one enters only when real users ask for it.

1. `cite`, through DOI content negotiation.
2. labs on clusters: group permissions, a shared cache with safe gc, a guide for compute nodes without internet.
3. fallbacks to the web archive and software heritage.
4. openml, openneuro and other sources users name.
5. public object storage.
6. an R wrapper.

## changing scope

- v0.1 is fixed once the maintainer approves it.
- a new idea goes to "after v0.1" with the evidence that would justify it.
- nothing joins v0.1 unless something of equal cost leaves, and the maintainer agrees.
