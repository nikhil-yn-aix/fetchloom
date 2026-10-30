# scope

## built

the command set is `get`, `add`, `remove`, `sync`, `lock`, `status`, `path`, `cache`, `cache gc` and `cache verify`. `fl` calls the same implementation. [CLI](cli.md) defines their effects and output.

every command takes `-q`, `-v`, `--json`, `--offline`, `--yes`, `--color` and `-C <dir>`. `add` and `get` also take `--select <glob>`, `--exclude <glob>` and `--no-extract`. `add` takes `--as <name>` to name the dataset, and `get` takes `-o <dir>` to choose its folder. `fl` is a short name for the same program. powershell already uses `fl` for `Format-List`, so use `fetchloom` there.

- project manifests select published inputs. locks record source files by SHA-256 and size, plus stated MD5 when available.
- a local content-addressed cache serves multiple projects. placements use reflinks, hardlinks or copies without creating symlinks.
- zip and tar archives unpack into cached members with a pinned tree hash. single compressed files stay intact. 7z and rar stay packed with a warning, rather than failing the download.
- transfers have retry and resume support. locked 404 or 410 downloads can ask their source for a replacement URL while retaining the original hash.
- offline sync uses local placements, cache objects and unpack records. resolving a changed entry still needs an online run.
- stdout answers and versioned JSON support scripts. the python module's `fetchloom.path()` and `fetchloom.sync()` run the installed command with `--json`; there is no R wrapper.

these are not guarantees that a host keeps a file forever, that every resume can continue, or that cached objects are rehashed on every use. [architecture](architecture.md) defines the limits and trust assumptions.

## sources

| reference | example | current behavior |
|---|---|---|
| HTTP or HTTPS URL | `https://example.org/data.zip` | one file, including direct release-asset URLs |
| DOI | `doi:10.5281/zenodo.3608990`, a bare DOI or doi.org link | route the handle's URL to a known source |
| zenodo | `zenodo:3608990`, a record or file-page link | list the concrete record; check stated MD5 on download |
| figshare | `figshare:34744689@1`, including institutional article links | list and pin an article version; check stated MD5 |
| dataverse | `dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP`, `dataverse:dataverse.harvard.edu/file:6867336` or a dataset or file page | pin published dataset or file versions; accept numeric file ids and `doi:` or `hdl:` ids; check stated MD5 or SHA-256 |
| OSF | `osf:abc12`, project links or file GUID links | walk osfstorage folders and pin file-version URLs; refresh explicitly expired storage redirects |
| drive | a shared file or folder link, including `open?id=`, or `drive:<id>` or `drive:folders/<id>` | walk shared folders; follow download confirmations; report quota, access and missing-file errors; refuse google docs entries that remain selected, and empty folders |
| dropbox | `/s/`, `/sh/`, `/scl/fi/` or `/scl/fo/` shared links | download shared files or a folder zip through `dl=1`; unpack folder zips; refuse unnamed HTML pages |
| hugging face | `hf:datasets/org/name@revision` | list and pin a repository commit; check stated LFS SHA-256; gated data needs approved access |
| kaggle | `kaggle:owner/dataset@version` | list and pin dataset versions; public downloads need no token; competitions are refused |

zenodo rejects restricted, embargoed and closed records. figshare rejects embargoed or download-disabled articles. dataverse rejects a dataset if any listed file is restricted, and refuses a restricted file page. filtering cannot bypass these checks or the listing-size limit.

the adapters, credentials and release status are separate from accepting a reference spelling. do not infer a working download from a link appearing in help. [formats](formats.md#what-each-source-records) shows the pins written by working paths.

## platforms and release

fetchloom runs on windows and linux, and [CI](../.github/workflows/ci.yml) runs the checks on both. macos is not supported. releases carry static x86_64 binaries, installers and wheels for both; other machines build the executables [from source](../README.md#install). source completion remains in [plan](plan.md).

one user's processes can share a cache. a cache writable by several people trusts all of them; no group-permission or cross-user administration feature is provided. recognized synced folders skip hardlinks but can use reflinks. placement details belong in [architecture](architecture.md#placing-files).

## outside the project

| excluded | use or reason |
|---|---|
| preprocessing and workflow execution | call fetchloom from make, snakemake, nextflow or your script |
| publishing and upload workflows | use the repository's upload interface |
| data branches and merges | manage produced data separately from fetched inputs |
| cloud copying and mirroring | use a transfer tool such as rclone |
| dataset search | supply a reference you chose |
| plugins | source adapters live in this crate |
| accounts, hosting and telemetry | fetchloom is a local command |
| controlled-data access approval | access is granted by the institution or data owner |
| 7z and rar extraction | decoders are not included; the packed file remains usable elsewhere |

`cite`, archive-service fallbacks, object-storage protocols, lab administration, shell completions and language or workflow wrappers beyond the python module are not implemented. add them when a concrete project needs them. downloads run several files at once; one file splits into parallel range requests only with `FETCHLOOM_SPLIT=1`, as [architecture](architecture.md#split-downloads) describes.
