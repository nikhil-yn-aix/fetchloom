# sources

fetchloom accepts direct links and published dataset references from these source families.

```sh
fetchloom get zenodo:3608990
fetchloom add zenodo:3608990 --select "*.csv"
```

use [search](../commands/search.md) when you have words rather than a reference. use `get` to inspect data once, or `add` to pin it in a project. each selected file gets a SHA-256 and size in `data.lock`. supported source checksums are checked while pinning downloads.

fetchloom pins data it can download again, so a reference is always a link, a DOI or an id, never a path on this machine. `add` and `get` refuse a path such as `./flowers.csv`, `/data/flowers.csv`, `~/flowers.csv`, `C:\data\flowers.csv` or `file:///data/flowers.csv` with `reference.invalid`, exit 10, before any request, and so does sync for a path written as `ref` in data.toml. a plain word that names a file or folder in the current folder, such as `iris` beside an `iris` folder, is not searched for either: it exits 2 with `usage`, and `fetchloom search iris` searches for it.

| family | reference spellings | what gets pinned | credentials | limits |
|---|---|---|---|---|
| [URLs and DOIs](urls-and-dois.md) | HTTP or HTTPS URL; `doi:10.5281/zenodo.3608990`, bare DOI or doi.org link | direct file bytes, or files from the routed source | HTTPS host logins from `.netrc`; one-off signed URLs work with `get` | DOI must route to a known source; project URLs reject recognized secrets |
| [zenodo](zenodo.md) | `zenodo:3608990`, record or file-page link | concrete record and file bytes; stated MD5 | `fetchloom auth login zenodo` or `ZENODO_TOKEN` | restricted, embargoed and closed records need an account zenodo lets read them |
| [figshare](figshare.md) | `figshare:34744689@1`, article links including institutional sites | article version and file bytes; stated MD5 | `FIGSHARE_TOKEN` | embargoed or download-disabled articles refused |
| [dataverse](dataverse.md) | host plus dataset PID or file id; dataset or file page | published dataset or file version; stated MD5 or SHA-256 | `fetchloom auth login dataverse`, or `DATAVERSE_API_TOKEN` for the host `DATAVERSE_URL` names | without a key for the host, any listed restricted file rejects the dataset |
| [OSF](osf.md) | `osf:3eunr`, project or file GUID link | versioned file URLs and bytes | `OSF_TOKEN` | walks osfstorage; a node can change when resolved again |
| [hugging face](hugging-face.md) | `hf:datasets/openai/openai_humaneval@7dce6050a7d6d172f3cc5c32aa97f52fa1a2e544` | repository commit and bytes; stated LFS SHA-256 | `HF_TOKEN` or existing token files | gated data requires approved access |
| [kaggle](kaggle.md) | `kaggle:uciml/breast-cancer-wisconsin-data@2`, `kaggle:competitions/titanic`, dataset or competition link | dataset version and file bytes; competition file bytes, which have no version | token or existing kaggle credentials; public datasets need none | competitions need `fetchloom auth login kaggle` and accepted rules; a file kaggle sends zipped is unpacked |
| [google drive](google-drive.md) | shared file or folder links, `open?id=`, `drive:` or `drive:folders/` | file ids, resource keys and byte hashes | shared-link access | selected google docs entries and empty folders refused |
| [dropbox](dropbox.md) | shared `/s/`, `/sh/`, `/scl/fi/` or `/scl/fo/` links | file bytes or folder zip, with an extracted tree when enabled | shared-link access | unnamed HTML pages refused |
| [public cloud buckets](buckets.md) | `s3://<bucket>/<prefix>/`, `gs://<bucket>/<prefix>/` or an HTTPS prefix link ending in `/` | object URLs and bytes; S3 single-part ETag or cloud storage MD5, and the cloud storage generation | none; anonymous only | private and requester-pays buckets refused |

## selection and access

`--select` and `--exclude` match listed source paths before download. quote them so your shell passes them unchanged. each select pattern must match at least one path before exclusions; some files must remain. patterns do not select members inside an archive.

listing-size limits apply before filtering. filters cannot bypass zenodo, figshare or dataverse access checks. a selected google docs entry is different: you can exclude it before fetching the remaining fixed files.

plain URLs have no resolved-metadata line. other sources show available metadata; a license containing `-NC` or `-ND`, ignoring case, gets a warning. check the license before using or sharing those files.

accepting a reference spelling does not guarantee access or a successful download. hosts can move or remove files. [locked URL recovery](../concepts.md#recovery) can find a replacement URL while retaining the original hashes.

[environment](../reference/environment.md#credentials) defines credential precedence. [lock records](../reference/data-lock.md#what-each-source-records) defines resolved references and download URLs.
