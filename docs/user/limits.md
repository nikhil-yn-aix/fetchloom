# limits

fetchloom fetches and pins published inputs; it leaves analysis, publication and data hosting to other systems.

| outside fetchloom | what to use or expect |
|---|---|
| preprocessing and workflow execution | call fetchloom from make, snakemake, nextflow or your script |
| publishing and uploads | use the data repository's upload interface |
| data branches and merges | manage produced data separately from fetched inputs |
| cloud copying and mirroring | use a transfer application such as rclone |
| search beyond hugging face, kaggle and zenodo | choose a reference from the source's own catalog |
| plugins | source adapters live in the fetchloom crate |
| accounts, hosting and telemetry | fetchloom is a local command |
| controlled-data access approval | request access from the institution or owner |
| 7z and rar extraction | keep the packed file and use another decoder |
| recursive archive extraction | nested archives remain files |
| macos | use a supported windows or linux machine |
| cross-user cache administration | cache writers are trusted; no group-permission management is provided |

## commands and integrations not implemented

`cite`, archive-service fallbacks, object-storage protocols, lab administration and shell completions are not implemented. the python module provides `path()` and `sync()`; there is no R wrapper or other language or workflow wrapper.

see the [developer roadmap](../dev/roadmap.md) for future work. current commands are listed in the [command guide](commands/README.md).

## reproducibility boundaries

file hashes and sizes pin bytes. they do not guarantee that a source will host those bytes forever. locked 404 or 410 downloads can try a replacement URL from the source, but the bytes must still match.

cached objects are not rehashed on every use. status uses saved metadata and selective hashing, and cache verification does not check independent copies in `data/`.

sync preserves local conflicts by default and can exit 0 while keeping them. `path` returns a location even when an edited file remains there. scripts that require untouched inputs must inspect [status and placement results](scripting.md#checking-and-syncing).

resumption depends on usable hashes or server validators. unsupported ranges, changed validators or damaged saved bytes can require restarting a transfer.

offline mode uses local placements, cache objects and unpack records. it cannot resolve changed manifest entries or repository listings. [offline preparation](concepts.md#offline) avoids discovering those gaps after disconnecting.

## access and platforms

windows and linux are supported on x86_64 and arm64; [installation](getting-started.md#install) is currently from source. one executable is installed as `fetchloom`. an accepted source spelling does not guarantee that access or a download will succeed.

source restrictions and listing-size limits apply before most filtering. credentials cannot grant approvals that a source has not given. [source guides](sources/README.md) lists each family's rules.
