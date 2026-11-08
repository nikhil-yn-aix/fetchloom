# compatibility

fetchloom supports the systems, terminals and source APIs listed below. [versioning](../../dev/engineering/versioning.md) covers what a new release may change.

## systems

the system table describes configured release targets. source builds depend on their selected rust target and its runtime libraries.

tier 1 means CI builds the program and runs its tests there. tier 2 means the release workflow builds binaries there, but CI does not run tests there.

| system | cpu | tier | needs |
|---|---|---|---|
| windows 10, 11, server 2016 or later | x86_64, arm64 | 1 | nothing else |
| linux, any distribution | x86_64, arm64 | 1 | kernel 3.2 or later. the binary is static, so no glibc version matters |
| macos 11 or later | apple silicon | 1 | nothing else |
| macos 11 or later | intel | 2 | nothing else |

other systems rust supports may build from source with `cargo install`, without promises.

some speedups depend on the system and fall back without failing:

| feature | where | otherwise |
|---|---|---|
| one flush for a batch of downloads and copies | linux 5.8 or later | each file is flushed on its own |
| one drive flush for a batch of unpacked files | macos | each file gets a full flush |
| reflinks, which share bytes until a file changes | btrfs, xfs, apfs, refs and dev drive | hardlinks |
| hardlinks | ntfs, ext4, hfs+ and most local disks | copies, as on fat, exfat, network shares and synced folders |

## terminals

any terminal works. colors, symbols and clickable links adapt to what the terminal says about itself:

| setting | effect |
|---|---|
| windows terminal, vs code, conemu, and any terminal on macos or linux with a utf-8 locale | unicode symbols and color |
| windows terminal, vs code, iterm2, wezterm, ghostty, vte 0.50 or later | links you can click, outside tmux and screen |
| the legacy windows console | colors with plain ascii symbols |
| `NO_COLOR`, `CLICOLOR=0`, `TERM=dumb` or `--color never` | no color |
| `CI`, or a coding agent's variable such as `AI_AGENT` | no questions, color or animation |
| output into a pipe or file | plain text, no animation |

[environment](environment.md) lists every variable.

shell completions: bash, zsh, fish, elvish, windows powershell 5.1 and powershell 7.

## the python helper

python 3.9 or later, on the systems above.

## building from source

rust 1.99 or later, the version in `rust-version`, and a C compiler: visual studio build tools on windows, `xcode-select --install` on macos, `build-essential` or `gcc` on linux.

## sources

fetchloom talks to each source's public web API. when a source changes its API, a patch release follows; [live tests](../../dev/engineering/ci.md#live-source-checks) run against every source each monday to catch it early.

| source | API | tested against |
|---|---|---|
| zenodo | REST API under `/api` | zenodo.org |
| figshare | API v2 | api.figshare.com |
| dataverse | native API: `/api/datasets/:persistentId`, `/api/access/datafile` | dataverse.harvard.edu |
| osf | API v2 and its file storage links | api.osf.io |
| hugging face | hub API under `/api` and `resolve` links | huggingface.co |
| kaggle | public API v1 | api.kaggle.com |
| s3 buckets | ListObjectsV2, anonymous | aws s3 |
| google cloud storage buckets | JSON API v1, anonymous | storage.googleapis.com |
| DOIs | doi.org handle API, then the repository it names | doi.org |
| plain links | HTTP/1.1 and HTTP/2 over https; range requests resume and split downloads where the server allows | any server |

## keys shared with other tools

fetchloom reads and saves keys where each source's own tools keep them, so one sign-in serves both. [environment](environment.md#credentials) lists where it looks and in which order, and [auth](../commands/auth.md) where a sign-in saves.
