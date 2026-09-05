# Fetchloom

A single binary that turns a dataset reference into exact, verified local files, and proves what it delivered.

## Read before working

`CONTRIBUTING.md` first, every session. It is how code is written here and it is not optional.

`docs/contracts.md` for the exact behavior you are implementing. Read the sections you touch, not the whole file.

`docs/internals.md` when the reason behind a contract is unclear.

`docs/reference.md` for the surface as it stands, including what is not built.

Everything the old `standards.md`, `vision.md`, `features.md` and `roadmap.md` carried is in those four files. Do not look for them.

## Report format

```
Done: <one line>
Decisions: <any choice not already in the docs, with the reason>
Tests: <what was added, and the real pass or fail output>
Benchmarks: <numbers, or none needed>
Uncertain: <anything you could not verify>
Blocked: <anything needing a human decision>
```

Do not restate the plan, summarize the docs, or explain at length what you are about to do.

## Forbidden

Placeholder or trivially true tests. A test that cannot fail is worse than no test.

Claiming success without running the command.

Committing or pushing unless explicitly asked.

Adding a dependency without stating why in the report.
