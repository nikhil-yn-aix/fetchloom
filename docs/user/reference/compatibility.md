# compatibility

fetchloom supports the systems, terminals and source APIs listed below. [versioning](../../dev/engineering/versioning.md) covers what a new release may change.

## systems

the system table describes configured release targets. source builds depend on their selected rust target and its runtime libraries.

tier 1 means CI builds the program and runs its tests there. tier 2 means the release workflow builds binaries there, but CI does not run tests there.

| system | cpu | tier | needs |
|---|---|---|---|
| windows 10, 11, server 2016 or later | x86_64, arm64 | 1 | nothing else |
| linux, any distribution | x86_64, arm64 | 1 | kernel 3.2 or later. the binary is static, so no glibc version matters, and a container without a certificate bundle still checks https against the mozilla roots fetchloom carries |
| macos 11 or later | apple silicon | 1 | nothing else |
| macos 11 or later | intel | 2 | nothing else |

other systems rust supports may build from source with `cargo install`, without promises.

some speedups depend on the system and fall back without failing:

| feature | where | otherwise |
|---|---|---|
| one flush for a batch of downloads and copies | linux 5.8 or later | each file is flushed on its own |
| one drive flush for a batch of unpacked files | macos | each file gets a full flush |
| reflinks, which share bytes until a file changes | btrfs, xfs, apfs, refs and dev drive | hardlinks |
| hardlinks | ntfs, ext4, hfs+ and other filesystems that support them | copies when links are unavailable or disabled, including fat, exfat and recognized synced folders |

## archives

fetchloom unpacks zip, 7z and tar archives itself on every system. it leaves a `.rar` packed, and the warning's help names a program for the system it runs on and a command that unpacks the archive into a folder beside it, inside the data folder, which fetchloom's `.gitignore` keeps out of git. for `data/sample/sample.rar`, run from the project's folder:

| system | program | the help |
|---|---|---|
| linux | unrar, rarlab's free reader, from the distribution's packages: `unrar` in ubuntu's multiverse, debian's non-free, rpm fusion and arch | `unrar x data/sample/sample.rar data/sample/sample-unpacked/` |
| macos | unar, from homebrew | `brew install unar`, then `unar -D -o data/sample/sample-unpacked data/sample/sample.rar` |
| windows | 7-Zip, from winget | `winget install 7zip.7zip`, then, in powershell, `& "C:\Program Files\7-Zip\7z.exe" x data\sample\sample.rar -odata\sample\sample-unpacked` |

rar volumes, `clips.part1.rar` and on, take the same help for their first volume, from which unrar and 7-Zip read the others. a zip spanned over `.z01` files beside its `.zip` stays packed as well: on linux and macos its help joins the parts with Info-ZIP's `zip -s 0 data/scans/scans.zip --out data/scans/scans-joined.zip` and unpacks that with `unzip data/scans/scans-joined.zip -d data/scans/scans-unpacked`, and on windows it runs 7-Zip, which reads spanned zips since 16.02, as `x data\scans\scans.zip -odata\scans\scans-unpacked`.

the unpacked files are yours to fetchloom, as notes you add would be: sync keeps them, `status` lists them as extra, and `git add .` leaves them out with the rest of the data folder. a `fetchloom get` folder is the exception: its help unpacks next to that folder, `sample-unpacked` beside `sample`, since a later `get` refuses a folder holding files it did not place.

macos's own `tar` and windows' own `tar.exe` are libarchive builds, and libarchive 3.7.2 refused solid rar4 and rar2 archives, rar5 files with filters and rar5 volumes that unrar read, so the help names programs that read them. unrar makes `data/sample/sample-unpacked/` when the trailing `/` names a folder; unar's `-D` puts the files straight into it, as 7-Zip's `-o` does. [decisions](../../dev/decisions.md#storage-and-requests) links the sources.

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

python 3.10 or later, on the systems above.

## building from source

rust 1.99 or later, the version in `rust-version`, and a C compiler: visual studio build tools on windows, `xcode-select --install` on macos, `build-essential` or `gcc` on linux.

## sources

fetchloom talks to each source's public web API. when a source changes its API, a patch release follows; [live tests](../../dev/engineering/ci.md#live-source-checks) exercise the configured public services each monday. not every adapter has a live check.

| source | API | tested against |
|---|---|---|
| zenodo | REST API under `/api` | zenodo.org |
| figshare | API v2 | api.figshare.com |
| dataverse | native API: `/api/datasets/:persistentId`, `/api/access/datafile` | dataverse.harvard.edu |
| osf | API v2 and its file storage links | api.osf.io |
| hugging face | hub API under `/api` and `resolve` links | huggingface.co |
| kaggle | public API v1 | api.kaggle.com |
| s3 buckets | ListObjectsV2 and GetObject, anonymous or with AWS signature version 4, path-style at other services | aws s3 for anonymous reads; signatures against AWS's published examples and a local stand-in; minio, r2, b2, wasabi and ceph from their documentation only |
| google cloud storage buckets | JSON API v1, anonymous | storage.googleapis.com |
| DOIs | doi.org handle API, then the repository it names | doi.org |
| plain links | HTTP/1.1 and HTTP/2 over https; range requests resume and split downloads where the server allows | any server |

## keys shared with other tools

fetchloom reads and saves keys where each source's own tools keep them, so one sign-in serves both. [environment](environment.md#credentials) lists where it looks and in which order, and [auth](../commands/auth.md) where a sign-in saves.
