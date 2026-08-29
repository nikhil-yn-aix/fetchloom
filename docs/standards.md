# Standards

How Fetchloom is written. These rules are enforced in review and, where possible, in CI.

## One thing

There is one format, one code path, one behavior.

The binary carries an ordinary release version. Before 1.0 it promises nothing, which is what a leading zero already means. A receipt records that version as provenance: it says which build produced a result and is never read to decide how to parse anything.

No format version fields in manifests, locks, receipts, plans, events, or config. A format version field is only useful if more than one set of rules is acceptable, and before 1.0 only one is. Detection of a mismatch uses the cache format fingerprint, which is an unordered hash of the format definition. Nothing can be branched on it, so it cannot become compatibility code.

Compatibility is designed now and implemented at 1.0. Writing compatibility code today is banned; making a decision today that would make compatibility expensive tomorrow is equally banned. The four enabling decisions are in contracts.md under Compatibility, and every change is reviewed against them.

Unknown keys are an error, and the `x-` prefix is reserved. Strict now leaves room to become extensible at 1.0; permissive now would not.

No compatibility shims, no deprecated flags, no aliases, no migration code, no feature flags for old behavior.

When a format changes, the old one stops existing. The cache format fingerprint causes a hard failure with instructions to clear the cache. Locks and receipts written by an older build are rejected with a clear message, not upgraded.

Two ways to do the same thing is a defect. Delete one.

This rule is lifted only when version 1.0 is published. Version 1.0 requires frozen contracts, the cross-platform conformance suite green in all six directions, the adversarial suite green, and published benchmark regimes. Staying below 1.0 until then is deliberate.

## Language

Rust. One workspace. A crate boundary exists only where the code is genuinely separable: platform, sources, archive, cache, engine, cli.

No unsafe code except where a platform call requires it. Every unsafe block carries a single `SAFETY:` line stating the invariant it relies on. This is the only comment permitted anywhere in the codebase.

No dependency without a stated reason in the pull request. Prefer no dependency over a small one. Prefer one well-maintained dependency over three.

## Tests

Write the test first. It must fail for the right reason before the implementation exists.

Test behavior stated in contracts.md, never internal structure. A test that breaks when an implementation detail changes is a bad test.

Every entry in the contracts tables has a test. Every error kind has a test that produces it.

Four kinds, all required.

Unit tests cover one function with real inputs. No mocking of types you own.

Contract tests drive the public surface and assert exact outputs: exit codes, JSON results, event sequences, and file trees.

Adversarial tests are first-class, not an afterthought: truncated responses, flipped bytes, changed validators mid-resume, rate-limit storms, stalled connections, DNS failure, full disks, killed processes, concurrent processes on the same digest, and the hostile archive corpus.

Cross-platform conformance tests build a tree on one platform, transport it, materialize it on the others, and assert identical tree digests or the exact declared failure. All six directions run in CI.

Fault injection is a library in the repository, not a mock in a test file. It is part of the product.

No test sleeps. Wait on a condition or a channel.

No test reaches the public network. Sources are served by a local test harness that can be told to misbehave.

Coverage is not a target. Every contract having a test is the target.

## Design

Small traits at real boundaries only: platform filesystem, source adapter, archive reader, cache store. Nothing else gets a trait.

A trait exists to allow a second real implementation or a fault-injecting one. A trait with one implementation and no test double is deleted.

Concrete types inside a crate. No generics that exist only to look flexible; they cost compile time and binary size.

One reason to change per module. Transfer does not know about archives. Extraction does not know about HTTP. The cache does not know what a dataset is.

Dependencies point inward. The engine depends on traits; adapters depend on the engine's traits, never the reverse.

Data flows through owned values and channels. Shared mutable state is a last resort and never appears in a hot path.

Errors are typed enums carrying the fields listed in contracts.md. No stringly-typed errors, no `anyhow` in library crates.

## Naming and shape

Functions do one thing and are named for the thing. If the name needs `and`, split it.

Names are ordinary words. No abbreviations beyond established ones, no invented vocabulary, no clever names.

A function that needs a comment to be understood is renamed or split instead.

No comments. No commented-out code. No banners, no section dividers, no decorative symbols, no emoji, anywhere in code or docs.

## Docstrings

Every public item has one. Private items have one only when the inputs and outputs are not obvious from the signature.

A docstring states what goes in, what comes out, and what causes failure. Nothing else. No history, no rationale, no examples unless the usage is genuinely non-obvious.

```rust
/// Reads the object with the given digest from the cache.
///
/// Takes a content digest. Returns an open reader positioned at the first byte.
/// Fails when the object is absent, when the cache format does not match, or
/// when the recorded fingerprint no longer matches the file on disk.
```

Plain sentences. No symbols standing in for words. The only markdown permitted is an `# Errors` or `# Panics` heading, because the lint policy requires them and they are structure rather than decoration.

## Failing

Fail fast, fail loud, fail once.

Validate at the boundary. Once a value is inside the system it is already correct and is not re-checked.

Make invalid states unrepresentable with types instead of checking for them at runtime.

Never swallow an error. Never log and continue. Never return a default in place of a failure.

Every fallback emits a `degrade` event naming what was requested, what was used, and why. A silent fallback is a defect of the same severity as data corruption.

`unwrap` and `expect` are permitted only where the invariant is enforced by the type system directly above the call.

Panics are for bugs. Expected failures are typed errors. A panic must never leave the cache or a destination in an invalid state.

## Observability

Structured logging only. Fields, never formatted sentences.

Every operation emits start and end events with byte counts and durations. Every retry, failover, wait, rejection, and degradation emits an event.

Log level controls verbosity, never correctness or which events exist.

Argument formatting never happens for a suppressed log level.

Redaction happens at the point of construction, not at the point of output. A secret must never exist inside a log record.

## Optimization

Optimization is a design constraint, not a later pass. These rules apply from the first commit.

### Measure

No optimization merges without a benchmark in the harness showing the gain on a named regime. Regimes are: no-op run, cold cache, warm cache, interrupted transfer, many small files, one large file, slow disk, and constrained network.

The no-op regime is a locked run against an unchanged destination. It measures startup, configuration discovery, and reconciliation, and it bounds how much a dependency may cost simply by existing.

Benchmarks include verification and extraction time. A benchmark that excludes them is invalid.

Metrics are of two kinds and are gated differently.

Deterministic metrics, such as binary size and bytes read or written, are identical on identical inputs. They gate everywhere, including a developer machine, and a regression above five percent fails.

Timing metrics are a property of the machine as much as the code. They gate only on continuous integration, against a baseline recorded on that same runner for that same target. A timing baseline is never recorded on a machine running other work, and a timing gate never fails a local run.

A timing measurement that moves while the deterministic metrics are unchanged is evidence about the machine, not about the change. It is investigated, never silenced by re-recording the baseline.

Profile before changing anything. Guessing at bottlenecks is not permitted as a justification.

### Memory

Nothing is read whole into memory. Every path is streaming, with a bounded buffer.

Buffers are allocated once and reused. No allocation inside a per-chunk or per-entry loop.

Bounded channels everywhere, so a slow consumer applies backpressure instead of growing a queue. Unbounded channels are banned.

Paths and member names are handled as bytes. No repeated conversion to and from string types, no lossy conversions.

Reference incoming data rather than copying it. Copy only when ownership must outlive the buffer.

### CPU

Hashing runs on the bytes as they arrive, in one pass, in parallel across chunks. There is never a second read to compute a digest.

The interop digest is computed on a separate thread from the content digest so neither serializes the other.

Asynchronous tasks never perform blocking work. Compression, hashing, and filesystem work run on a dedicated pool sized to the hardware.

No shared lock in a per-chunk path. Prefer per-task ownership, then atomics, then sharding. A global mutex in a transfer or extraction loop is a defect.

Feature detection for vector instructions happens at runtime. Builds target a portable baseline so one binary runs everywhere.

### Disk

Preallocate the full length before writing. Write at explicit offsets. Never seek in a loop.

Buffer sizes are aligned and sized for the device, not for convenience.

Flush according to the durability tier, at object granularity, never per entry.

Large sequential transfers avoid evicting the page cache the rest of the system is using.

Materialize from the cache with a copy-on-write clone when the filesystem supports it. Never write the same bytes twice when a clone is available.

Batch filesystem metadata calls. Thousands of individual stat calls on a directory tree is a defect.

### Network

One connection pool per host with keep-alive and session resumption. Connections are never opened per request.

Concurrency per host is discovered by measurement and bounded by a politeness ceiling. Fixed default connection counts are guessing.

Protocol choice is measured per host and cached, because the faster protocol depends on the network path.

Ranged transfers of a single object are used only when the object is large, immutable, the server supports ranges, and measurement showed a gain.

Retry with exponential backoff, jitter, and any server-supplied retry guidance. Never retry a non-idempotent or terminal failure.

Rate-limit responses reduce concurrency immediately and are recorded per host.

Read the transfer directly into the hashing and writing pipeline. No intermediate accumulation.

### Startup

The binary does no work at startup that a command does not need. Configuration discovery, cache opening, and credential lookup are lazy.

A command that fails on policy fails before opening a socket or allocating a buffer.

## User-facing text

Assume no prior knowledge of the provider, the protocol, the terminal, or Fetchloom itself. A message that only makes sense to someone who already knows the answer is a defect.

Never interrupt without a reason the user can act on. A prompt states what is being decided, what each choice costs, and what happens if it is ignored.

Never present setup before it is needed. There is no onboarding, no first-run wizard, and no configuration step between installing and fetching.

Instructions are numbered, exact, and name the page to open and the control to use. A link is never a substitute for a step.

Say the number. Faster, slower, and large are not measurements.

Write like a person, not like a product. Warmth comes from plain words and short sentences, never from decoration, exclamation, or personality inserted where information belongs.

Never tell the user something about Fetchloom that does not help them with what they are doing.

## Debloat

Applies to code, documents, error messages, and output.

Say a thing once, in one place. Repetition is deleted, not cross-referenced twice.

No meta writing. Nothing describes what the document is about to say or has just said.

No filler qualifiers, no hedging, no restating the obvious.

The shortest form that is still exact wins. If two lines do the work of five, the five are wrong.

Every sentence carries information the reader does not already have.

## Review gates

A change merges only when all of these hold.

The contract it changes is updated in contracts.md in the same change.

Tests were written first and cover the contract, including its failure modes.

It runs green on Windows, macOS, and Linux, including the adversarial and conformance suites.

It introduces no version field, no compatibility path, and no second way of doing something that already exists.

It adds no comment, no dead code, and no dependency without a stated reason.

Any performance-relevant change carries benchmark numbers for the affected regime.

Any new fallback emits a `degrade` event.
