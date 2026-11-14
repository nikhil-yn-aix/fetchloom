# compatibility

fetchloom supports the systems, terminals and source APIs listed below. [versioning](../../dev/engineering/versioning.md) covers what a new release may change.

## tested systems

these hosted systems execute release-artifact journeys on every CI run. the runner labels select an image family, not a frozen OS patch. linux, Windows and apple-silicon macos also run the native unit and CLI suites; intel macos runs the packaged journey, not a second source suite.

| runner | cpu | tests beyond the default disk |
|---|---|---|
| Ubuntu 24.04 | x86_64, arm64 | x86_64: btrfs, xfs and a chmod-refusing FAT mount |
| Windows Server 2025 | x86_64 | ReFS, exFAT and FAT32 |
| Windows 11 | arm64 | native executable and wheel |
| macos 15 | apple silicon | HFS+ |
| macos 15 | intel | native packaged executable, wheel and shell installer |

[CI](../../dev/engineering/ci.md#installed-artifacts) lists the exact runner labels and installer/interpreter checks. a simulated CloudStorage path tests fetchloom's placement decision, not a real cloud provider or network disk.

## compatibility targets

the configured release targets below retain their intended lower bounds. the newer hosted systems above do not check those minimum versions. static linkage and a macos deployment target check cannot establish kernel or old-OS behavior; minimum-version machines still need the same artifact journey before those floors can be called tested. source builds depend on their selected rust target and its runtime libraries.

| system | cpu | target requirements |
|---|---|---|
| windows 10, 11, server 2016 or later | x86_64, arm64 | no separate C runtime installation |
| linux | x86_64, arm64 | kernel 3.2 or later; the binary is static, with no glibc dependency, and carries mozilla roots for HTTPS |
| macos 11 or later | apple silicon, intel | only system libraries |

other systems rust supports may build from source with `cargo install`, without promises.

some speedups depend on the system and fall back without failing:

| feature | where | otherwise |
|---|---|---|
| one flush for a batch of downloads and copies | linux 5.8 or later | each file is flushed on its own |
| one drive flush for a batch of unpacked files | macos | each file gets a full flush |
| reflinks, which share bytes until a file changes | btrfs, xfs, apfs, refs and dev drive | hardlinks |
| hardlinks | ntfs, ext4, hfs+ and other filesystems that support them | copies when links are unavailable or disabled, including fat, exfat and recognized synced folders |

## terminals

any terminal works. colors, symbols and clickable links adapt to what the terminal says about itself:

| setting | effect |
|---|---|
| windows terminal, vs code, conemu, and any terminal on macos or linux with a utf-8 locale | unicode symbols and color |
| windows terminal, vs code, iterm2, wezterm, ghostty, vte 0.50 or later, konsole, kitty | links you can click, outside tmux and screen |
| the legacy windows console | colors with plain ascii symbols |
| `NO_COLOR`, `CLICOLOR=0`, `TERM=dumb` or `--color never` | no color |
| `CI`, or a coding agent's variable such as `AI_AGENT` | no questions, color or animation |
| output into a pipe or file | plain text, no animation |

[environment](environment.md) lists every variable.

shell completions: bash, zsh, fish, elvish, windows powershell 5.1 and powershell 7.

## the python helper

python 3.10 or later. CI installs the x86_64 macos wheel on Python 3.14, the other five wheels on Python 3.15.2, and also checks Python 3.10 on linux x86_64 and Windows x86_64. native Windows-arm Python 3.10 is not tested.

## building from source

rust 1.99 or later, the version in `rust-version`, and a C compiler: visual studio build tools on windows, `xcode-select --install` on macos, `build-essential` or `gcc` on linux.

## sources

fetchloom talks to each source's public web API. when a source changes its API, a patch release follows; [live tests](../../dev/engineering/ci.md#live-source-checks) exercise the configured public services each monday. not every adapter has a live check.

| source | API | tested against |
|---|---|---|
| zenodo | REST API under `/api` | zenodo.org |
| figshare | API v2 | api.figshare.com |
| dataverse | native API: `/api/datasets/:persistentId`, `/api/access/datafile` | dataverse.harvard.edu |
| dryad | API v2: datasets, versions and files | datadryad.org for listings; downloads, which need an API account, against a local stand-in |
| mendeley data | the public API under `/public-api` that its site uses: a version, its folders and their files | data.mendeley.com |
| ENA | portal API: `filereportcount` and `filereport` for read runs | www.ebi.ac.uk, its files on ftp.sra.ebi.ac.uk over https |
| GEO | the folder indexes of NCBI's file server under `/geo` | ftp.ncbi.nlm.nih.gov over https |
| DANDI | API: a dandiset, a version's `info/` and its `assets/` with `metadata=true`, in pages | api.dandiarchive.org, its blobs on dandiarchive.s3.amazonaws.com over https |
| OpenNeuro | GraphQL by GET with `apollo-require-preflight`: a dataset's newest snapshot, a snapshot's description and its folders, one at a time | openneuro.org, its files on s3.amazonaws.com/openneuro.org over https |
| arXiv | API query (`id_list`, and `cat:` with `submittedDate` 1,000 papers a page) and OAI-PMH `GetRecord` in `arXivRaw`, one request every 3 s; pdfs from cornell's mirror through the cloud storage JSON API, else export.arxiv.org's `/pdf/` | export.arxiv.org, oaipmh.arxiv.org and the `arxiv-dataset` bucket on storage.googleapis.com |
| semantic scholar | graph API v1: a paper by any id it takes, with `paperId`, `corpusId`, `externalIds`, `url`, `title`, `year`, `authors` and `openAccessPdf`, one request at a time, 1 s apart with a key and 3 s without; a pdf from the link it gives, an arXiv paper through the arXiv source; datasets API v1: a release, then a dataset's signed links, the S3 links of the older form, `AWSAccessKeyId`, `Signature`, `x-amz-security-token` and `Expires`, asked again for each download | api.semanticscholar.org without a key, and publishers' hosts for pdfs; the datasets API and its links with a key, once, on 2026-11-13 |
| osf | API v2 and its file storage links | api.osf.io |
| hugging face | hub API under `/api` and `resolve` links | huggingface.co |
| kaggle | public API v1 | api.kaggle.com |
| github releases | REST API: a release by tag, or the latest | api.github.com |
| gitlab releases | REST API v4: a release by tag, or the newest through its permalink | gitlab.com |
| s3 buckets | ListObjectsV2 and GetObject, anonymous or with AWS signature version 4, path-style at other services | aws s3 for anonymous reads; signatures against AWS's published examples and a local stand-in; minio, r2, b2, wasabi and ceph from their documentation only |
| google cloud storage buckets | JSON API v1 and object links, anonymous or with an OAuth 2.0 access token from application default credentials: a service account's signed assertion or a gcloud login's refresh token | storage.googleapis.com for anonymous reads; the token request and the bearer token against google's published OAuth 2.0 flows and a local stand-in |
| azure blob containers | `List Blobs` and blob links in version 2020-10-02 of the REST API: anonymous, with a SAS from the settings or `AZURE_STORAGE_SAS_TOKEN`, signed with an account key, or with a token from the az cli or a managed identity | `<account>.blob.core.windows.net` for anonymous reads; the account key's signature against azure's published example and the azure-storage-blob library; tokens against a local stand-in |
| DOIs | doi.org handle API, then the repository it names | doi.org |
| plain links | HTTP/1.1 and HTTP/2 over https; range requests resume and split downloads where the server allows | any server |

google drive and dropbox are read through their shared links rather than an API, and drive's API v3 serves the private items `drive_folders` lists. no live check covers dropbox, and drive's needs google credentials the weekly run does not have.

[search](../commands/search.md) asks each site's own search: hugging face's, kaggle's and dandi's dataset lists, zenodo's records, EBI search's `sra-study` domain for ENA, NCBI's E-utilities `esearch` and `esummary` at eutils.ncbi.nlm.nih.gov for GEO, openneuro's GraphQL `advancedSearch`, arXiv's API query by title, then in every field, and semantic scholar's graph API `paper/search`. the weekly macos journey runs one search naming all nine, which passes while any of them finds a match.

## keys shared with other tools

fetchloom reads and saves keys where each source's own tools keep them, so one sign-in serves both. [environment](credentials.md) lists where it looks and in which order, and [auth](../commands/auth.md) where a sign-in saves.
