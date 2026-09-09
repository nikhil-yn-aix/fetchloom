# changelog

format is [keep a changelog](https://keepachangelog.com/en/1.1.0/).

nothing has been released. the version in `Cargo.toml` promises nothing before
1.0, so every entry lands in unreleased.

## [unreleased]

### added

`get` with no reference fetches every dataset a `fetchloom.toml` names in its
`datasets` table. an entry is a reference, or a table stating `ref` and any of
`output`, `select`, `exclude` and `layout`. a string is never a table and a
table always states `ref`. every relative path resolves against the directory
holding that file and the lock lands beside it, so a run from four directories
down writes what a run from the top writes. two entries that would write to one
destination fail before anything is fetched, naming both. `get --locked` with no
reference is the reproducible install.

`probe <ref>` says what a source states about an object without moving a byte of
it: the resolved location, the size, every digest it states with its algorithm,
the trust class a fetch would land in, whether it serves ranges, and whether the
cache already holds it. anything unstated is unknown, and unknown is an answer
that exits 0. `--offline` answers from the cache or fails `policy.offline`.

`list <ref>` says what a container holds, moving as few bytes as the format
allows. a zip over a source that serves ranges costs its index and one small
read per member rather than the archive. a directory or a prefix is the listing
seam that already exists. an object the cache holds is read from the cache. a
tar states no index, so listing one reads all of it: the run says so with a
`degrade` before the bytes move, keeps what it read, and proceeds rather than
refusing.

`--library` materializes into one directory holding datasets, separate from the
cache, and `where <ref>` prints the path a dataset lands at without fetching.
the path is `<library>/<name>/<identity>`, decided by what the reference
resolves to and nothing else, so two versions sit beside each other and a script
can read the path before a run has happened. a name a Windows volume refuses, a
device name or one with a separator, a colon or a trailing space, is sanitized
deterministically. `--library` and `--output` together are an error.

`library ls` states what the library holds and what it takes. `library rm
<path>` removes one entry and the record of the run that wrote it, and refuses a
path outside the library. nothing is removed from the library on its own.

`--library-dir`, `FETCHLOOM_LIBRARY_DIR` and `library = { dir = "..." }` say
where the library is. the default is the platform data location rather than the
cache location.

the field names of every `--json` result are a contract, stated in
`docs/contracts.md` per command and asserted by a test, so a rename fails the
gate rather than a pipeline. they are additive only, and this build has no
version field to negotiate one.

`status <path>` and `diff <path>` say how a directory differs from the record
the run that wrote it left. four states and no others: unchanged, modified,
deleted, added. an unchanged entry is not printed, so a directory that is
exactly what the run left prints nothing at all. `diff` adds the digest and the
length on each side and never looks inside a file. both are read only and
neither touches the network.

`revert <path>` puts back what the record states, and `revert <path>
<entry>...` puts back only what it names, leaving every other edit alone. the
bytes come out of the cache, so a revert works offline. an entry the cache no
longer holds fails naming the object and the command that would fetch it, rather
than quietly going to the network.

`get` against a directory you edited, where upstream has since moved, is a three
way compare against the record rather than a refusal. per entry and never per
line: what only upstream changed is taken, what only you changed is kept, what
you added stays, what upstream added arrives, and what you deleted stays
deleted. where both sides changed one entry, upstream's version is written
beside yours as `<name>.upstream`, yours is left exactly as it is, every
conflict is named, and the run exits 60 with `destination.conflict`. the merged
tree is built whole in staging and published by rename, so a run killed halfway
leaves the old directory or the new one.

`promote <path>` makes the directory as it stands a dataset of its own: every
file read, the bytes kept in the cache, a manifest naming one artifact per file
with both digests, and a lock pinning them. the manifest states `derived_from`,
which is the dataset, the manifest digest and the tree digest it came from.
ingestion happens at promote and at no other time.

a record now states two entry streams: what was materialized, which is what
`verify <path>` folds and what a fingerprint answers about, and what the
reference resolved to, which is the merge base. they differ only where a run
kept your version of an entry over upstream's.

an artifact whose stated digest the cache already holds resolves out of the
cache without its source path existing, which is what makes a promoted manifest
fetchable on a machine that took the objects in a bundle.

`merge.resolution` is a new event, one per entry a three way run decided.
`destination.conflict` is a new error kind, exit 60.

FTP and FTPS are spoken, written here rather than taken as a dependency.
`ftp://host/path` and `ftps://host/path` fetch a file or expand a directory,
anonymous by default, passive, binary, resumed with `REST` at rung four. a
directory is listed with `MLSD` and falls back to `LIST` with a `degrade` when
the server refuses it, which two of the four reference hosts checked live do not
support. the address a `PASV` reply names is discarded and only its port used,
so a data connection is never opened to a host the control connection is not
already talking to.

`ftps://` secures the control connection with `AUTH TLS`, then `PBSZ 0` and
`PROT P` for the data connection, using the rustls already in the graph. no flag
turns that off. `ftp://` attempts it and says so when it falls back to the
clear, and a credential resolved for the host is refused rather than sent over a
connection that could not be secured.

a name with no `sources` configured is searched for across the eight registries
that offer search, in parallel: Hugging Face, Kaggle, OpenML, Zenodo, Figshare,
CKAN, Dataverse and DataCite. one record carrying the name proceeds and prints
what it resolved to. several are printed with size, provenance and what each
states about its bytes, the run refuses, and the command for each is given. none
fails naming the nearest names it did find. nothing is chosen for the user. the
name is never written to a lock and what it resolved to is.

six more providers are reached by their own identifier: `kaggle:owner/slug`,
`openml:61`, `github:owner/repo@tag`, `figshare:1234567`, `ckan:host/dataset`
and `dataverse:host/doi:10.x/y`. they share one internal listing shape described
by an endpoint and a field mapping, so a provider is a value rather than an
adapter, and Hugging Face and Zenodo now share the same `Source`
implementation. CKAN and Dataverse carry the host in the reference, over HTTPS
only, so one description reaches every installation.

a digest a provider's listing states is carried as a claim and verified against
the bytes. of the six, GitHub releases states a SHA-256 on every asset, and
Dataverse states one on an install configured for it. the rest state only MD5,
which is not carried, so their trust class stays `tofu` rather than implying
evidence a run cannot recheck.

`KAGGLE_API_TOKEN` and `GITHUB_TOKEN` are read, so a machine that already has
the provider's own variable set needs nothing restated. `FETCHLOOM_TOKEN_<HOST>`
is still read first and still holds the whole `Authorization` header.

`doi:10.7910/DVN/OMV93V` resolves. the registration is read once from DataCite
and the landing page decides which provider holds the record, reported as a
resolution alias. a DOI resolving to a provider no adapter serves fails naming
the registrant, the landing page, and what serving it would need.

`Artifact.sources` is a list of mirrors across adapters. each candidate is
served by whichever adapter states it serves it, so a manifest may name an HTTPS
mirror and a provider mirror for the same bytes and the run falls through from
one to the other. this is safe by arithmetic rather than by promise: the digest
is over the bytes and not the location.

cached objects are stored compressed. `--compress <auto|none|zstd:1..19>`, the
`compress` configuration key and `FETCHLOOM_COMPRESS` decide how, `auto` being
the default. `auto` compresses an object only when compressing its first 1 MiB
measured a ratio of at least 1.10, so an archive or a photograph is stored raw
rather than spending processor time to grow. `explain` reports the effective
value and the level that supplied it.

a compressed object is written as zstd frames of one outboard chunk group each
with a table of their lengths, so a ranged verify or a localized repair
decompresses only the frames covering the range it asked for.

float arrays are byte shuffled before compression when that measured better on
the object itself. the stride is measured rather than assumed: the probe
compresses the head at strides 1, 2, 4 and 8 and stores the winner in the frame
footer, where a stride of 1 is spelled 0 because shuffling one byte words is a
copy. an eight byte array measured 4.388 at stride 8 against 2.147 at the stride
4 that used to be hardcoded. which objects those are is measured from the bytes,
never taken from an extension or a media type.

bundles are compressed. every trust property is unchanged: a member is still
named by the digest of its own bytes, import still derives every digest from
what it reads, a name is still never a path, and a bundle that fails anywhere
still publishes nothing.

a volume that compresses what is written to it is a detected capability on both
platforms, queried on Windows through the file attribute and measured on Linux
from the blocks a compressible write allocates. a run on one stores objects raw
and emits `degrade`, rather than compressing what the filesystem will compress
again.

`cargo deny check` runs as a step of `cargo xtask verify`, so a crate entering
the dependency graph without a reviewed entry fails a gate instead of appearing
in a lockfile diff.

`cargo xtask verify` builds the workspace at the `rust-version` its manifest
states, so the stated minimum is a checked claim rather than a number.

`cargo xtask verify --install-hook` installs a `pre-push` hook. installing over
a hook it did not write refuses and changes nothing.

`cache compact` rewrites packs, training one zstd dictionary over the objects
each pack holds and recompressing them against it at level 19. a pack holds
objects at or below one frame, which is where a frame gives an object no context
and a dictionary supplies it: measured at 9.260 without and 13.167 with, over
the same objects at the same level. the dictionary is stored in the pack it
belongs to and shared with no other pack.

`cargo xtask surface` reports which public items cross a crate boundary.

the `checks` lane fails on any plain comment outside a `SAFETY` block, in any
`.rs` file in the workspace, and the pre-push hook runs the same check.

`README.md`, `CHANGELOG.md`, `LICENSE`, and `CONTRIBUTING.md` and
`SECURITY.md` under `.github/`.

### changed

the project is MIT, where it was Apache-2.0. that is the more permissive of the
two conventional Rust licenses and imposes nothing on anyone who takes the code.


`mimalloc` is what the two musl targets get, rather than a feature no build
enabled and that did not compile when enabled. measured on ext4, interleaved,
ten runs each: 2000 files of 1 KiB take a median 320 ms against 372 without it,
and one 256 MiB file is indistinguishable. it costs 194,592 bytes of binary on
those two targets and nothing anywhere else.

what a volume accepts as its longest path is measured once per volume per boot
and recorded in the cache at `meta/volume-<volume id>`, rather than measured in
every process. the probe is still the authority and a record from another boot
is refused and replaced. on Windows it built a chain of 139 nested
255-character directories, failed, and removed it: 325 to 450 ms in every run,
78 percent of a run that fetched nothing. a warm `get` is 105 to 187 ms where it
was 499 to 991, and `cache status` is 77 to 109 where it was 442 to 622.

`--durability normal` pushes a pack once for the run rather than once per entry
appended to it, and contracts.md now states what each of the three tiers
promises. `normal` is durable per run, `strict` per object, `fast` not at all.
2000 files of 1 KiB cost 2,023 file operations where they cost 6,021, and
`strict` still costs 6,024. nothing is served from bytes that are not there
under any tier.

a first cold fetch of one source under a digest a manifest already states costs
one request rather than a `HEAD` and then a `GET`. every fetch a probe can still
tell something still probes. the lease that stops two runs fetching one digest
twice is claimed before any request rather than after the probe.

a source that asked to be left alone for longer than the retry ceiling is left
for the next candidate rather than asked again after a shorter wait.

an FTP control connection is refused under `--offline` at the latch, before the
host is looked up.

a run refuses before it fetches anything when a volume cannot hold what it
needs. the partial and the object it becomes count once where they share a
volume, because publication there is a rename rather than a copy.

the `Store` seam states the nine methods a transfer is polymorphic over. the
other twelve were asked of the one cache by name and are now inherent to it.

peak resident set gates per target, upward only, at ten percent. it differs 44
percent between the two runners because the allocator and the page size differ
by target, so one number could never gate both.

the benchmark harness measures against a corpus no compressor shrinks. the one
it used repeated every 251 bytes, so 256 MiB of it stored as 109,034 and the
regime named for one enormous file was measuring the compressed publication
path. twelve deterministic metrics moved.

the `benchmark` lane runs in CI on `ubuntu-24.04` and `windows-2025`, gating the
deterministic counters and never a duration, with `--deterministic-io` so a
recorded host measurement stops making `file-operations` a function of the run.
each runner records its own baseline.

the binary is 9,614,848 bytes where it was 9,053,696, which is 6.20 percent and
past the five percent a gated metric may move. it is the four commands that
change adds, at about 140 kilobytes each, measured against the release binary
the base commit builds. the recorded value moves to what was measured.

a degradation the platform recorded is now said. the cache owns a platform, and
the `degrade` it records when a volume refuses a copy-on-write clone was written
into a queue nothing drained, so a run that copied every byte instead of cloning
them reported nothing. `take_degradations` is now part of the `Platform` seam,
and a platform's queue is drained once at the end of a run rather than beside
the cache's own, so where the event lands in the stream does not depend on when
the volume was first asked about itself.

a run records what a location resolved to whether or not the source stated a
validator. it used to record it only when there was one, so a source with no
`ETag` and no `Last-Modified` left nothing tying a location to the bytes it
gave, and neither `list`, `probe` nor `repair` could say the cache already held
it. nothing revalidates against a record that cannot be asked with, so what a
run fetches is unchanged.

a fingerprint is recorded only for a file whose modification and change times
are already behind the instant the run began recording them, so a file written
while the record was being taken is always read rather than trusted. the window
that leaves is stated in contracts.md rather than claimed closed.

deciding whether a destination entry is unchanged no longer walks the resolved
tree once per file, which was quadratic in the number of entries.

the zstd codec is libzstd through the `zstd` crate, replacing the pure-Rust
`ruzstd`. `ruzstd` implements one of nineteen compression levels and ran 4x to
27x slower on the same corpus entries, so it could not carry `--compress
zstd:1..19` without the flag becoming a placeholder. the workspace now compiles
and statically links C.

the gate is eight lanes rather than thirteen steps, and a lane runs where it is
native or it does not run. Docker and the qemu-emulated `--arm` lane are gone,
along with `xtask/verify/Dockerfile` and `linux.sh`. `cargo xtask verify` runs
every lane this machine can prove and names the machine each declined lane
needs. `--lane <name>` runs one, and `--lane <name> --provision` installs the
targets, toolchain, system packages and tools that lane needs. GitHub Actions
runs the rest natively on Linux x86_64, Linux aarch64, Windows x86_64 and
Windows aarch64, so `aarch64-pc-windows-msvc` and the aarch64 Linux pair are run
rather than compiled. a workflow step is `cargo xtask verify --lane <name>` and
states nothing else.

the cache format fingerprint changed, so an existing cache is discarded with
`cache clear` rather than migrated. no lock, receipt, plan or bundle manifest
changed, and no digest moved.

documents are bounded by what wrote them rather than by which parser reads them.
a receipt is this machine's record of work it already did, so it carries its own
limits. a manifest is written by a stranger and keeps the strict ones.

a document past a size or node bound fails with `resource.limit`, which is what
the contract always said. it previously reported `manifest.invalid`.

a volume's capability answer is decided once per run, so two callers asking at
the same time cannot be given different ones.

the public surface of every library crate is now what crosses a crate boundary.
roughly 800 items became private.

`missing_errors_doc` and `missing_panics_doc` are denied and satisfied.

documentation is five files under `docs/` plus the README, the changelog, and
the two files GitHub reads from `.github/`. sixteen files were deleted.

### fixed

a cache shared between users on one machine could not be locked by a second
user. the cache directory is published `0o1777` so anyone may create in it, but
a lock file was created under the process umask, so a lock one user took landed
`0o644` and a second user could not open it to ask whether it was held, and got
`cache.corrupt` naming a permission error instead. lock files are now created
`0o666`, the mode of the directory they live in. nothing caught it because every
previous Linux run of the suite was root inside a container, and root ignores a
mode.

the cache format fingerprint was written straight onto its name, so two runs
starting against one fresh cache could have the second read it half written and
refuse with `cache.format_mismatch`, telling the user to clear a cache that was
correct. it is written beside its name and renamed onto it, which is what every
other record in the cache already does. two aarch64 Linux runs starting together
found it. nothing else ever had.

a reference this build cannot serve was told that this build resolves only a
local path or a `file:` location, in a build that serves http, https, ftp, ftps,
an object store and eight providers. any scheme no adapter claims reached it,
`sftp://` and `gopher://` among them. it now says that nothing here serves that
reference, which is what the two other places a run decides the same thing
already say.

a zip holding more than 65,535 members had its member names read from the
classic end of central directory record, whose count field is a sentinel above
that number, so the name pre-scan saw only the first 65,535 while the
enumeration behind it read them all. path validation and collision detection ran
over every member regardless, so nothing escaped, but the separator decision was
made from a truncated view and then applied to all of them: an archive whose
only forward slash sat past the truncation had 65,535 members whose names
legally contain a backslash silently rewritten into directories it never
described. the zip64 end of central directory record is now read whenever a
classic field states its sentinel and a locator is there, which is also what the
crate behind the enumeration does, so the two agree by construction. a locator
pointing at a record that is absent or malformed is `archive.unsupported` naming
zip64.

a SHA-256 a manifest stated was never compared to the bytes. contracts.md has
always said either algorithm satisfies `verified`. only BLAKE3 did, because it
becomes the address the store commits under. a manifest stating a SHA-256 that
disagreed materialized the bytes and exited zero.

a run that verified every artifact reported `tofu`. the receipt's per-artifact
class was right. the run's own class was seeded from a provisional `tofu` that a
fold taking the weakest class could never improve on. a matching BLAKE3 claim
was affected too.

reference.md carried a corrupted row in its not built table, left over when
`--compress` graduated out of it. the reference test only checked the built
half. it now also checks that nothing below the not built marker is something
the binary offers.

`verify` failed on any tree past roughly 17,000 files, because a receipt was
bounded by the manifest node limit. a run could materialize a tree it could not
verify, which is the one thing a receipt exists to prevent.

`repair` accepted seven flags it could never act on, all of them about
materialization, which it does not do.

a `degrade` event stated one hardcoded reason for three different causes, so a
failing fetch of an archive claimed the reference named a directory.

a zip symlink member was read before the expansion guard observed its size, so a
100 MB archive could force an allocation near 100 GB.

selection globs backtracked, so eight star components took 143 ms and each
further one multiplied that by about 5.8. patterns arrive in remote manifests.

reconcile was quadratic in entry count. at the contracted limit it extrapolated
to about 269 seconds.

manifests and plans were read whole before their size was checked.

`corroborated` is documented as unreachable in this build, which it always was,
and a test pins it in both directions.

### removed

the packed-index gate ceiling. it compared `cache status` over 2000 packed
objects against 4000 and failed above a ratio of 2.0, measuring 1.160 and 1.077
while about ninety percent of both terms was the volume probe. with the probe
recorded once per boot the same measurement is 1.407, and it is still a
duration, which this project does not gate. the ratio is published.

the benchmark baseline recorded on one laptop. a counter can depend on what the
volume underneath can do, so a baseline is recorded per target on the runner
that gates it.

`--track` is gone from the not built list rather than built. detection costs
nothing, because every completed run already writes the record `status`, `diff`,
`revert` and `promote` read, so the flag would have been a second way of doing
what the tool does.

`cargo xtask check-comments`, replaced by the scanner the `checks` lane runs.

`audit.md`, `audit2.md`, `plan.md`, and eleven documentation files.

the `restarted-from-zero` hint. no run set what it was guarded by, so it was a
line the tool could never print.

the `selection` hint, and `Limits::resident_memory`. the hint needed a field
written on exactly the code path that also writes the one the lock hint reads,
and the lock hint is tested first and returns, so no run could construct the
state it needed. the limit carried a default of one gibibyte and no reader, so
nothing ever compared anything to it.

`spike/`, the pre-phase-6 measurement workspace, and the `exclude = ["spike"]`
line in the root `Cargo.toml` that was the only change it ever made outside
itself. what still constrains a decision is one record in `docs/decisions.md`.
the rest answered questions shipping code has since settled.

`verify` and `durability` as configuration keys, and `per_host` as the spelling
of one, from reference.md. the first two are flags and were never keys, and the
third is `per-host`, which the file's own parser has always said.
