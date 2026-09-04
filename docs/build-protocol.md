# Build protocol

How Fetchloom is built by separate Claude sessions. The rules exist to remove drift, assumptions, and unverified claims.

## Unit of work

A phase is too large for one session. A phase is split into three to five slices. One session does one slice.

A slice is independently testable, independently mergeable, and small enough that its result can be judged in minutes. If a slice cannot be described in one paragraph, it is two slices.

## Session lifetime

Sessions reset at phase boundaries, not between slices. Within a phase, a session that already holds the context keeps working. Across a phase boundary every session is closed, because a new phase reads different contracts and inherits nothing but the documents.

Decision records are how context crosses a boundary. Anything a session learns that must survive is written to `docs/decisions.md` before it closes.

## Roles

Three roles, never combined in one session.

Researcher settles open questions and writes a decision record. Produces no code.

Builder implements one slice against settled decisions. Never decides anything the docs are silent about; it stops and asks instead.

Judge reviews a finished slice against the contracts. Never wrote the code it reviews, because a session cannot see its own assumptions.

## Loop

Research the phase's open questions. Write decisions. Human approves.

For each slice: build, then judge, then fix or accept.

At the end of the phase, judge against the roadmap exit criteria. A phase is not partially carried forward.

## Parallelism

Builders run in parallel only when they own disjoint crates. Two sessions inside one crate produce merge conflicts and, worse, two different answers to the same unwritten question.

Parallel builders require a foundation session first. Every seam trait, domain type, and signature they compile against must already exist, or they will each invent their own. That foundation is one session and it is never parallelized.

A judge runs alone, after the work it judges.

## Decision records

Every choice not already written in the docs is recorded in `docs/decisions.md` before code depends on it.

```
## <short title>
Question: <what had to be decided>
Options: <what was considered>
Chosen: <what was chosen>
Because: <evidence, measured or cited>
Costs: <what this makes harder>
```

If a decision changes a contract, contracts.md is updated in the same change. The decision record is the reason; the contract is the rule.

## Token discipline

The standing rules live in `CLAUDE.md` and are not repeated in prompts. A prompt carries only what is specific to its slice.

Prompts name the sections to read, not whole documents.

Reports use the format in `CLAUDE.md`: short, `file:line` references, no pasted code, no restating the plan.

A session that has drifted is restarted with a fresh prompt rather than argued with.

## Researcher prompt

```
Role: researcher. Write no code.

Read: CLAUDE.md, docs/standards.md, and <named sections>.

Settle these questions:
1. <question, with what evidence would settle it>
2. ...

For each: verify current library APIs with Context7, search for benchmarks or
specifications rather than relying on recall, and state where evidence is thin.

Output: append one decision record per question to docs/decisions.md, and list
any contract change they require. Do not edit contracts.md yourself.

Stop and ask if a question cannot be settled with evidence.
```

## Builder prompt

```
Role: builder. Slice <id>: <one line>.

Read: CLAUDE.md, docs/standards.md, docs/contracts.md sections <names>,
docs/decisions.md entries <names>.

Build: <what exists when this is done>

Prove: <the specific tests and adversarial cases required>

Done when: <objective, checkable criteria>

Do not: touch files outside <scope>, add a dependency without stating why,
decide anything the docs are silent about, or leave a placeholder.

Verify every library API against current documentation before using it.
Run every test and paste the real output.
```

## Judge prompt

```
Role: judge. Review slice <id>. You did not write it.

Read: CLAUDE.md, docs/standards.md, docs/contracts.md sections <names>, and the diff.

Check, and answer each with evidence from the code:
1. Does the behavior match the contract exactly, including error kinds, event
   names, exit codes, and limits?
2. Was each test written to fail first, and does it assert a contract rather
   than an implementation detail?
3. Can any test pass while the feature is broken? Name it if so.
4. Are the adversarial cases real: failure, corruption, interruption,
   concurrency, hostile input?
5. Did it run green on both platforms? Show the evidence, not the claim.
6. Any comment, version field, compatibility path, second way of doing an
   existing thing, dead code, or placeholder?
7. Any silent fallback without a degrade event?
8. Any new dependency, and is the reason stated?
9. Any benchmark regime regressed or missing?

Output: a numbered list of defects, each with file:line and the contract it
violates. Say pass or fail. Do not fix anything.
```

## Sizing

A session takes the largest coherent batch it can finish well. Small batches waste sessions and force the same context to be rebuilt repeatedly. A batch is too large only when its work spans seams that do not depend on each other, or when its result cannot be judged against a single set of contracts.

Research and build never share a session. That split is what removes assumptions, and merging it costs more than it saves.

Research runs immediately before the build that depends on it, so decisions are made once, in context, and never made twice.

## Phase 0 sessions

R1 Research. Toolchain and crate selection for parsing, styling, progress, hashing, platform calls, structured events, testing, and benchmarking. Repository and crate layout matching the six seams. A local verification matrix covering both platforms. Thread pool sizing policy, including how core counts, affinity, container limits, and user limits are read and honored. Lint and dependency policy that mechanically enforces the standards.

R2 Research. Content hash chunk size and outboard threshold. The exact canonical entry stream for the tree digest, including symlinks, empty directories, zero-byte files, and mode reduction. Atomic publication primitive and durability tier calls per platform. Filesystem capability detection method per platform. Advisory locking primitive, including cross-user and network filesystem limits.

B0 Build, alone. Workspace and crate layout, lint and dependency policy, the verification matrix green on every target it builds, the benchmark harness, every seam trait and domain type, the error and event types with redaction at construction, and the fault injection crate. No behavior beyond what a signature requires. Everything after this compiles against it.

B1, B2, B3 Build, in parallel, one crate each.

B1 owns `platform`: capability detection, atomic publication, durability tiers, cloning with copy fallback, preallocation, and advisory locking with liveness.

B2 owns the digest modules in `engine`: content digest, interop digest, outboard tree, tree digest, and the cross-platform conformance corpus and matrix.

B3 owns `cli`: command shell, configuration precedence, Policy and Observer implementations, the plain renderer, `explain`, `completions`, and the wiring for `get file:///path` and `verify`.

J Judge. Reviews all four builds against the contracts and the phase 0 exit criteria.
