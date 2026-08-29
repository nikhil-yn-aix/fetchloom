# Fetchloom

A single binary that turns a dataset reference into exact, verified local files, and proves what it delivered.

## Read before working

`docs/standards.md` first, every session. It is how code is written here and it is not optional.

`docs/contracts.md` for the exact behavior you are implementing. Read the sections you touch, not the whole file.

`docs/roadmap.md` for the current phase and its exit criteria.

`docs/vision.md` and `docs/features.md` only when the reason behind a contract is unclear.

## Non-negotiables

Contracts come first. If contracts.md states a behavior, implement exactly that. If it is silent on something you need, stop and ask. Never choose for it and never invent a flag, field, event, or error kind that is not written down.

Tests are written before implementation and must fail for the right reason first.

No comments. No version fields. No compatibility code. No second way of doing anything that already exists.

Nothing degrades silently. Every fallback emits a `degrade` event naming what was requested, what was used, and why.

Every change works on Windows, macOS, and Linux, or it is not done.

## Verify, do not trust

Confirm every library API against current documentation before using it. Use Context7 for library docs and web search for anything else. Do not rely on recall for a signature, a default, or a platform behavior.

Do not trust this file, the docs, or a previous session's claim over what the code and the tests actually do. If a doc contradicts reality, say so instead of coding around it.

Run everything you claim. A test suite you did not execute has not passed. Paste the real output.

State uncertainty plainly. A guess labeled as a guess is useful; a guess presented as fact is a defect.

## Done means

Tests written first, passing, and asserting a stated contract rather than an implementation detail.

Adversarial cases covered: failure, corruption, interruption, concurrency, and hostile input where relevant.

Green on all three platforms.

Benchmark numbers included for anything performance-relevant, with no regime regressed.

Contracts updated in the same change if behavior changed.

No `todo!`, no placeholder, no skipped test, no dead code.

## Forbidden

Placeholder or trivially-true tests. A test that cannot fail is worse than no test.

Claiming success without running the command.

Committing or pushing unless explicitly asked.

Adding a dependency without stating why in the report.

Restating the plan, summarizing the docs, or explaining what you are about to do at length.

## Report format

Keep it short. Use `file:line` references instead of pasting code.

```
Done: <one line>
Decisions: <any choice not already written in the docs, with the reason>
Tests: <what was added, and the real pass or fail output>
Benchmarks: <numbers, or none needed>
Uncertain: <anything you could not verify>
Blocked: <anything requiring a human decision>
```
