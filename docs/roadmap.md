# Roadmap

Ten phases. Each one ships a working vertical slice on Windows and Linux, with real tests against real behavior. No phase is a layer that waits for a later phase to become useful.

## How phases work

Every phase has the same four parts.

Decide. Open questions are researched and settled before code. The answer and the reason are written into contracts.md or standards.md in the same change. Unsettled questions do not enter implementation.

Build. The slice, end to end, behind the seams below.

Prove. Contract tests, adversarial tests, and cross-platform conformance. Benchmarks for anything performance-relevant. Tests assert stated contracts, never internals.

Done when. Objective exit criteria. A phase is not finished early and is not partially carried forward.

## Seams

Six interfaces are defined in phase 0 and never change shape afterward. Later phases add implementations behind them and never widen them casually.

Platform. Filesystem, atomic publication, cloning, locking, capability detection.

Store. Content-addressed objects, partial transfers, staging, leases.

Source. Metadata probe, validators, ranges, authentication, retry guidance, immutable identity.

Archive. Enumeration, selection, bounded extraction.

Policy. Trust, offline, limits, credentials, terms.

Observer. Events, errors, progress, redaction.

Widening a seam requires the same justification as changing a contract.

## Optimization

Optimization is not a phase. Every phase carries the performance rules in standards.md and merges nothing that regresses a benchmark regime. Phase 6 is the exception only because adaptive measurement is itself a feature, not because tuning was deferred.

---

## Phase 0. Spine

Purpose. Everything later depends on identity, platform behavior, and observability. All three are unfixable if retrofitted.

Decide. Chunk size and outboard threshold for the content hash. Exact canonical entry stream for the tree digest, including symlinks, empty directories, zero-byte files, and mode reduction. Filesystem capability detection method per platform. Atomic publication primitive per platform and how a cross-volume attempt fails. Async runtime and thread pool sizing policy, including how core count and user limits are read and honored. Error and event taxonomy, and the point at which redaction is applied.

Build. Workspace and the six seams. Platform implementations for both targets. Content digest, interop digest, outboard tree, tree digest. Error and event types with redaction at construction. Fault injection library. Local test source and hostile archive corpus scaffolding. Configuration precedence and `explain`. `get file:///path` and `verify`.

Prove. Tree digest conformance in both cross-platform directions. Capability detection against real filesystems: ext4, btrfs, XFS, tmpfs, NTFS, ReFS, and a network share. Every error kind reachable by a test. Kill and restart during publication leaves no invalid state.

Done when. A directory materialized on one platform reproduces an identical tree digest on the other, or fails naming the exact entries, with no network code in the binary.

## Phase 1. Store

Purpose. The cache is the shared mutable resource. Concurrency and crash safety here are the hardest correctness problems in the project.

Decide. Lock record contents and liveness rules. Grace period and lease model for prune. Startup recovery for orphaned partial and staging entries. Durability tiers and where each flush occurs. Fingerprint tuple and its exact platform meanings. Cache format fingerprint derivation. Cross-user locking and ownership for a shared cache, and which filesystems cannot support it. Behavior when the cache is missing, read-only, or full.

Build. Object store, partial store, staging, metadata, pins, locks. Single-writer-per-digest with wait-and-reuse. Prune with mark, grace, sweep. `cache status`, `ls`, `verify`, `pin`, `unpin`, `prune`, `clear`.

Prove. Many processes racing on one digest produce one transfer and no corruption. Kill loops during publication and prune leave invariants intact. Prune never removes pinned, leased, or referenced objects under concurrent load. Warm reuse is measurably faster than cold on every platform.

Done when. The store survives a thousand random kills under concurrent load with zero invalid objects and zero orphans after recovery.

## Phase 2. Transfer

Purpose. The network is where honesty is easiest to fake and hardest to earn.

Decide. HTTP client and TLS stack, and how platform trust stores and proxies are read. Connection pool and keep-alive policy. Retry classification: which failures are transient, which are terminal, and how server retry guidance is honored. The resume ladder and how source identity is recorded in a partial file. Politeness ceilings and default concurrency before measurement exists. Redirect handling and credential dropping. The source scoring inputs and their priority, the probe budget, and the conditions for switching mid-transfer. Which directory index formats are recognized and how an unrecognized one fails.

Build. HTTPS source behind the Source seam. Streaming into the hash and write pipeline in one pass. Range support, resume, retry with backoff and jitter, failover across ordered sources. `get https://...`.

Prove. Truncated responses, flipped bytes, changed validators mid-resume, rate-limit storms, stalled connections, and DNS failure each produce the correct exit code and a resumable state. Every resume rung is exercised and reported. Credentials are dropped on cross-host redirect and never appear in any output.

Done when. A large transfer interrupted twenty times completes with the correct digest and never restarts from zero above rung four.

## Phase 3. Materialize

Purpose. Extraction is where hostile input meets the filesystem, and where most tools are unsafe.

Decide. Which archive and compression formats ship, based on real dataset demand. Selection semantics and layout rewriting. The complete rejection list and which rejections are per-entry versus fatal. Clone versus copy decision per platform and filesystem. Reconcile outcomes and what requires explicit approval.

Build. Archive seam with the chosen formats. Staging extraction using handle-relative operations. Bounded limits. Collision detection under target filesystem case and normalization rules. Clone with copy fallback. Reconcile with unchanged, restored, modified, and foreign outcomes.

Prove. The hostile corpus is rejected entry by entry with the named error, on every platform, with nothing published. Collisions fail before publication and print both names. Warm materialization uses cloning where available and reports it where not. Reconcile refuses to touch modified and foreign entries. Extraction of many small files is measured with an on-access scanner enabled, not only with one disabled.

Done when. No corpus entry escapes staging on any platform, and repeated runs against an unchanged destination write nothing.

## Phase 4. Record

Purpose. This is where reproducibility becomes portable and air-gapped work becomes possible.

Decide. Lock and receipt contents, and the exact separation between portable and local fields. Plan contents, and how a plan proves it is still valid at apply time. Bundle format and how it stays digest-addressed. Behavior of a locked run when resolution differs.

Build. Lock writing and locked-run enforcement. Receipts. `plan` to a file and `apply` from one. `cache export` and `cache import`. Partial success semantics.

Prove. A lock produced on one platform reproduces the tree on the other. A plan and bundle produced online execute correctly on a machine with no network. A source serving changed content fails apply with an integrity error and no fallback. Locks contain no machine-specific values.

Done when. Plan on a connected machine, carry the bundle on removable media, apply offline, and the tree digest matches.

## Phase 5. Prove

Purpose. Localized verification and repair is the capability nothing else has. It depends on the store, transfer, and outboard trees already existing.

Decide. When an outboard tree is stored and when it is skipped. How a bad range is identified and the refetch is bounded. Quarantine layout and diagnostics. Witness record format and what counts as independent. Exact conditions raising trust from first-use to corroborated.

Build. Range verification against the outboard tree. `repair`. Quarantine with diagnostics. Witness recording and trust class resolution. Cache-hit verification policies.

Prove. Corrupting a single region of a large cached object is detected, localized, and repaired by refetching only that region. Verification never passes on damaged content under any policy. Trust classes are produced exactly as their definitions state, and no path silently weakens one.

Done when. Repair of a one-megabyte region inside a very large object transfers approximately one megabyte and restores the correct digest.

## Phase 6. Tune

Purpose. Turning measurement into defaults, so users never tune anything, without letting measurement touch correctness.

Decide. What is measured, how a decision is scored, and how it is stored per host. The increase and backoff policy for concurrency. How protocol choice is evaluated. When ranged splitting of a single object is worthwhile. How disk write rate applies backpressure to network concurrency. Which I/O mode is chosen per platform and volume. How user limits and core counts bound every decision.

Build. Adaptive concurrency controller with politeness ceilings. Per-host measurement cache. Protocol selection. I/O mode selection. Backpressure from disk to network. `--deterministic-io`. The published benchmark suite covering every regime the harness runs.

Prove. Adaptation changes throughput and never changes bytes, digests, or tree digests. Rate-limit responses reduce concurrency immediately. Deterministic mode reproduces identical timing-independent results. Published numbers include the regimes where Fetchloom is slower.

Done when. Default settings beat hand-tuned fixed settings across the regime matrix, with no correctness difference in any run.

## Phase 7. Reach

Purpose. Sources are permanent maintenance, so the contract is proven by one real implementation before more are added.

Decide. Which sources meet measured demand and in what order. How each maps to the Source seam, especially immutable identity and range support. Credential lookup order per platform and the complete redaction target list. The offer threshold above which an optional credential is worth interrupting for. The help record for every provider, written and reviewed as product text. Requester-pays and egress cost reporting. Terms acceptance flow.

Build. Object storage, then the providers that demand justifies. Credential resolution across environment, platform stores, and provider helpers. Required and optional credential flows with their help records. Cost reporting in plans. Terms gating.

Prove. Every adapter satisfies the same contract test suite, including its degraded trust behavior. No adapter emits a secret in any output stream under fault injection. Sources without range support degrade honestly and say so. A run needing a required credential fails with steps a first-time user can follow, and a run offered an optional one completes correctly when it is declined.

Done when. A new adapter can be added by implementing the seam and passing the shared suite, with no change to the engine.

## Phase 7.5. Concurrency

Purpose. Phase 6 built adaptation against a harness that issued one request at a time, so the sentences features.md wrote about concurrency, probing, splitting and the credential offer were contracted and unproven. This phase makes the run actually do several things at once and holds each of those sentences to a test.

Decide. What the two in-flight ceilings bound and what they may never change. How several candidates are probed without racing them. Which four conditions together make splitting one object worthwhile, and how wide. What raises a per-host count and what lowers it. Which credential is resolved for which host when a transfer moves between hosts.

Build. The scheduler behind the global and per-host ceilings. The probe phase and the fixed scoring order. Ranged splitting of a single object. The adaptive count that rises only when a wider count delivered more. Per-candidate credential resolution. The two seam counts and the dispatch that names adapters.

Prove. Ceilings bound what is in flight, asserted against what the fault server was answering at once rather than against a clock. No setting changes bytes, digests, or the tree digest. A losing candidate is never asked for bytes. A split object has the digest the whole object has, and a refusal names the condition that failed. No output stream carries a secret when a transfer moves to a second host.

Done when. Every sentence features.md writes about concurrency is either true and tested or corrected. Closed. Four defects that concurrency made observable are recorded in decisions.md under this phase, and phase 6's performance half is explicitly not closed by it.

## Phase 8. Ingest

Purpose. Existing metadata is absorbed rather than retyped, and every feature the product describes exists. This is what removes the cold start and what ends the era of a document promising more than the binary does.

Decide. Which metadata formats map cleanly onto the manifest model and which cannot be represented honestly. How inference handles listings, APIs, and local directories. What inference records when a source supplies no digest. Where `init` writes. Where the second credential shape's values come from. What a configured source priority is. What a log level is.

Build. Readers for the chosen metadata formats and checksum sidecars. `init` for URLs, listings, and local directories. Manifest emission in the human format. The four reference forms that remain: metadata document, provider, bare name, and namespaced release. Provider adapters behind the Source seam. SigV4 request signing as a second credential shape, and the provider-native credential helper tier. Repository, record, and FTP listing. `doctor`, `why`, `watch`, the live display mode, hints, and the flags contracts names that this build does not carry.

Prove. Round-trip tests from each supported metadata format to a manifest to a materialized tree. Inference over a directory produces a manifest that reproduces that directory exactly. Formats that cannot be represented fail with a named reason rather than an approximation. Adding two real provider adapters changes no engine code. `doctor` leaves the state it inspects byte-identical. Identical runs under every display mode produce identical results, exit codes, and event streams, and the live view is shown structurally, not by inspection, to read nothing but events.

Done when. A dataset with existing published metadata is fetched with no hand-written manifest, a local directory becomes a publishable manifest in one command, and features.md carries no unbuilt marker except for distribution.

## Phase 9. Ship

Purpose. Distribution is part of the product. A binary users cannot trust or install does not exist.

Decide. Packaging targets and install paths, including no-root installation. Signing, notarization, and offline verification per platform. What earns 1.0 and what the compatibility freeze covers.

Build. Static binaries for all targets. Signing and notarization pipelines. Checksums and signed release metadata. Package manager entries. No-root install.

Prove. Every artifact installs and runs on a clean machine per platform, without root and without a network beyond the download. Signatures verify offline. The full suite runs green on release artifacts, not only on development builds, including phase 8's display-mode equivalence and the structural proof that the live view reads only events.

Done when. Contracts are frozen, all six conformance directions and the adversarial suite are green, benchmark regimes are published, and the portable artifact set is committed to permanent readability. That is 1.0.

---

## What bites later if rushed

Tree digest and platform capability rules decided after phase 0 invalidate every lock and receipt already written.

Cache locking and recovery added after phase 1 means rewriting the storage layer with real users depending on it.

Redaction added after secrets already flow through logs and events means auditing every call site instead of one constructor.

Seams widened casually turn adapters into engine changes and make phase 7 permanent maintenance instead of a contract.

Compatibility decisions deferred past 1.0 cannot be recovered, because the artifacts are already in the wild.

Benchmarks written after optimization make every performance claim unverifiable and every regression invisible.
