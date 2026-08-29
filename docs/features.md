# Features

Each feature states what it does, what the user sees, and why it exists. Exact formats and values live in contracts.md.

## References

Accept every way a user might name data: bare names, namespaced names with releases, local manifest files, remote manifest URLs, direct file URLs, provider references, and pure content addresses.

The user never has to know which kind they have. One verb takes all of them.

Moving aliases are resolved before transfer and the resolved identity is pinned into the lock.

A reference naming a folder is expanded by listing that folder: an object store prefix, a repository or record, a WebDAV or FTP directory, or a generated HTML index. Fetchloom lists what it was pointed at and never wanders outside it, follows links found inside files, or executes anything. An index it does not recognize is an error, never a guess.

## Manifest inference

Most data has no manifest. Fetchloom writes one instead of demanding one.

`fetchloom get <url>` needs no manifest at all. It probes the source, transfers, verifies, extracts, and writes a lock.

`fetchloom init <url|dir>` walks a listing, an API, or a local folder and emits a manifest with digests filled in. A lab publishes a dataset by running init and putting the file on their web server.

Existing truth is absorbed rather than retyped: checksum sidecar files, Croissant metadata, Frictionless data packages, pooch registries, DVC files, BagIt manifests, and torrent piece hashes.

Manifests are declarative. No shell, no hooks, no generators.

## Planning

`fetchloom plan` resolves the request and reports what will happen without moving bytes: the resolved release, hosts to be contacted, files selected, known sizes before and after expansion, cache hits, destination conflicts, trust class, credentials and terms required, and disk needed at each of the four storage locations.

Plans are files, not just screen output. A plan produced on a connected machine can be carried to a disconnected one and executed there. This is what makes air-gapped work practical.

A plan never promises an exact size or duration the source cannot support. Unknown stays unknown.

## Trust classes

Every result is labeled with one of four classes, each with a mechanical definition: verified, corroborated, trust-on-first-use, or unverified.

Publisher identity, manifest authenticity, and content integrity are tracked separately. One never implies another.

Weak classes require explicit policy to accept. Nothing silently falls through to a weaker class.

Trust can be raised without a central authority. When independent parties, such as a teammate lock, a CI run, or a public log, record the same digest, the class rises from trust-on-first-use to corroborated.

## Transfer

Bytes stream into dedicated partial files, are hashed as they arrive, and are published atomically only after the digest matches.

Independent artifacts transfer concurrently inside global and per-host limits. Range requests split a single object only when the source is immutable, supports ranges, the object is large, and measurement showed a gain.

When several sources offer the same object, Fetchloom probes them cheaply in parallel and picks one. It does not race them, because racing wastes the user's bandwidth and gets tools blocked by the hosts that matter. If the chosen source stalls or fails, the transfer moves to the next one and keeps every byte already verified.

Transient failures retry with bounded exponential backoff, jitter, and any server-supplied retry guidance. Terminal failures stop and say why.

Concurrent Fetchloom processes wanting the same object share the work. One downloads, the others wait and reuse. Never a duplicate, never a corrupted object.

Cancellation stops promptly and leaves resumable state.

## Adaptive performance

Fetchloom measures instead of asking the user to guess.

Connection count per host rises while throughput improves and falls when it does not, and backs off on rate-limit responses. Protocol choice is measured per host rather than assumed, because the faster protocol depends on the network. Decisions are cached per host.

Adaptation may change timing. It may never change bytes, digests, or the resulting tree.

## Resume

Resume happens only when the remaining bytes provably belong to the same object. There is a defined ladder from strongest to weakest evidence, and the chosen rung is reported.

The strongest rung needs no help from the server. Because the internal hash is a tree, bytes already on disk can be proven correct against the recorded root before transfer continues.

When identity cannot be proven, Fetchloom restarts and says why rather than silently appending to a different object.

## Verification and repair

Content is verified during transfer, before any object becomes valid, and again according to policy on cache hits.

Size, ETag, Last-Modified, transport security, and provider identity are supporting evidence only. They are never content identity.

Because verification is tree-based, damage is localized. A single bad region inside a very large object is identified and refetched by range instead of restarting the whole transfer. This works against any source that supports ranges, with no cooperation from the publisher, once the object has been fetched successfully once.

Mismatches are quarantined with diagnostics.

## Cache

Completed objects are stored by content digest in the operating system per-user cache location. Objects, partial transfers, metadata, locks, and staging are visibly separate.

The cache is reusable internal storage, never the only copy the user has, and never required. A cache that is missing, full, or read-only makes Fetchloom say so and continue without it rather than fail.

It can live in the platform default location, in the project, or in a directory shared by several users on one machine. A shared cache is refused rather than used unsafely when the filesystem cannot lock across users.

Operations: status, list, verify, pin, unpin, prune, import, export, repair, clear.

Pruning is deterministic and safe while other processes are running. Active and pinned objects always survive. Removing a destination and pruning the cache are separate operations and neither implies the other.

Cache location is configurable by command, environment, project config, and user config, with documented precedence. A single operation can bypass the cache entirely.

Storage cost, reuse, and retained bytes are reported before and after operations.

Internal cache layout is never a public interface.

## Safe materialization

Extraction happens in staging. The destination appears only after every check passes.

Rejected outright: absolute paths, traversal, symlink and hardlink escapes, device files, case collisions, Unicode normalization collisions, reserved and unrepresentable filenames, alternate data streams, sparse bombs, and expansion bombs.

Bounded: file count, expanded bytes, path length, nesting depth, memory, and temporary disk.

Deterministic: permissions, executable bits, timestamps, and symlink handling follow one documented rule set on every platform.

Selection happens without extracting irrelevant archive members when the format allows it.

Copy-on-write clones are used when the filesystem supports them, so materializing from a warm cache costs metadata rather than a second copy of the data. Otherwise it falls back to a copy and reports which happened. Hard links and symlinks are opt-in.

Unrelated or user-modified destination files are never overwritten without explicit approval.

## Locks, receipts, and idempotence

A lock records the resolved identity, artifact digests, selection, and materialization recipe using portable paths only.

A receipt records the manifest digest, artifact digests, the source actually used, the trust class, the canonical tree digest, and the local result.

The canonical tree digest covers normalized paths, file types, content, and the metadata that matters, under one cross-platform rule set.

Running the same locked request again reconciles the destination, reuses valid work, and rewrites nothing needlessly. If the same tree cannot be represented safely on the current platform, Fetchloom fails and names the conflicting entries.

Locks, receipts, manifests, plans, and bundles are the durable surface. From 1.0 they are readable by every later release. The cache is not durable and is discarded rather than migrated, which costs nothing because it can always be rebuilt.

## Interfaces

One primary command, `get`, plus focused commands for inspection and maintenance.

Human output goes to stderr. Machine output goes to stdout. Exit codes are stable and meaningful enough to branch on.

A structured event stream exposes timing and byte counts for resolution, cache lookup, transfer, verification, extraction, and publication.

`explain` shows every effective setting and where it came from, including values chosen by measurement.

`doctor` checks configuration, permissions, disk, cache health, certificates, and provider setup without changing anything.

Progress is plain by default. An opt-in live view shows what the run is actually doing: which source was chosen and why, throughput per host, retries, verification, cache hits, and rejected archive entries. It reads the event stream and nothing else, so it can never show something the machine-readable output does not already carry, and it can be turned off or removed without affecting a run. A run can be rendered from another terminal, including one placed in the background by the shell. Fetchloom never backgrounds itself and never runs a daemon.

After a run, Fetchloom may print one hint about something the user could have done differently, such as a token that would have made the transfer far shorter. It never states facts about itself, never interrupts a transfer, never repeats itself, and is disabled by one flag.

Shell completion is supported. Telemetry does not exist.

## Configuration

Precedence is command line, environment, project config, user config, then defaults.

Configuration covers paths, network access, source priority, proxy and TLS policy, retries, resource limits, integrity policy, extraction limits, logging, and output format.

TLS verification is on by default. Custom certificate authorities and proxies are explicit and inspectable.

Every optional optimization can be disabled without disabling any correctness check.

## Credentials and access

Credentials are scoped to the host or provider they were issued for and are never sent elsewhere, including across redirects.

Sources are environment variables, platform credential stores, and provider-native helpers. Secrets are never copied into project files.

Fetchloom never asks for a token at startup and never presents a setup screen. It asks at the moment a token would change the outcome, and it says what the change is. When a token is required, the run stops and prints exact steps. When a token would only make things faster, Fetchloom names the two options with the measured difference, prints the steps, and continues without it if the user declines. Small differences produce no prompt at all.

Instructions assume no prior knowledge of the provider, of tokens, or of the terminal. Every provider ships the same fixed set of facts: what the token unlocks, whether it is required, numbered steps to obtain it, where to put it, how to confirm it works, and the narrowest permissions that suffice.

An expired or revoked credential is reported as such, with how to renew it. A bare authorization failure is never shown on its own.

Secrets, signed query strings, and private paths are removed from logs, receipts, events, and error messages.

Terms and license acceptance are never automatic. Fetchloom records that the user asserted acceptance and does not interpret its legal meaning.

## Mobility

Tool mobility: one static binary per platform, installable without root, working in containers, on clusters, and in minimal images.

Data mobility: a verifiable bundle can be exported from one cache and imported into another, offline, on removable media.

Identity mobility: locks travel and contain no machine facts. Tree digests either agree across platforms or fail loudly.

## Observability

Nothing degrades silently. Every fallback, skip, retry, weakened trust class, unsupported filesystem capability, and unavailable optimization emits an event and appears in the result.

Errors are typed and carry the failing layer, the dataset, the artifact, the source, the retry state, and the recommended next action.

Performance claims come only from repeatable tests covering cold cache, warm cache, interrupted transfer, many files, large files, slow disks, and constrained networks. No benchmark disables verification or omits extraction time.
