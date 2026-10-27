# plan

## how we work

every milestone is a vertical slice. it leaves `fetchloom` runnable and useful on its own, never a layer waiting for a later step.

1. plan. a short note at the top of the pull request: the slices, the tests that prove them, the decisions taken.
2. build one slice at a time. write a failing test at the level that shows the behavior, write the code that passes it, run the binary and look at what it did, then commit.
3. review. a code review and a design review read the whole diff. each finding is fixed with a test or answered in one line.
4. measure. the milestone's budgets from [performance](performance.md) are measured on a release build and written into the pull request.
5. ship. CI green on windows, macos and linux, merge, then use it the way a researcher would.

work that is fully specified, such as a source with its recorded responses, a test corpus or a review, runs in parallel, each piece in its own git worktree. design and decisions stay in one place, and parallel work is read in full and run before it is committed.

a choice that changes [vision](vision.md) or [scope](scope.md) goes to the maintainer, Nikhil Y N. every other choice is made on the spot and recorded in [decisions](decisions.md).

bugs, ideas and questions live in github issues.

## milestones

| # | milestone | what runs after it |
|---|---|---|
| 1 | skeleton | `fetchloom --version`, the welcome screen, colors, `--json`, errors with a fix. CI and a release dry run on three platforms |
| 2 | one file end to end | `fetchloom get <https url>`: resume, retries, SHA-256, the cache, placement by reflink, hardlink or copy, and copies inside synced folders |
| 3 | the project loop | `add`, `remove`, `sync`, `lock`, `status` and `path` for URLs. hand edits are kept and `--offline` works |
| 4 | archives | zip and the tar family unpacked safely into the cache. unsafe archives refused as a whole |
| 5 | the four big sources | google drive, kaggle, hugging face, zenodo |
| | first users | the maintainer and two or three researchers use it on real projects. what surprises them goes to the top of the issue list |
| 6 | the other sources | dropbox, osf, figshare, dataverse, DOI routing |
| 7 | cache and recovery | `cache`, `cache gc`, `cache verify`, stopping when upstream bytes change, asking the source again when a link dies |
| 8 | python and release | `fetchloom.path()`, wheels, installers, the readme, every budget measured |
| | v0.1 | released when every scenario in [scope](scope.md#done-means) passes on all three platforms |

the order follows evidence. google drive, kaggle and hugging face are the hosts readmes link to most, so they arrive before the others, and real users arrive right after them, not after everything.

## done, for a milestone

- each scenario it covers runs as a test on windows, macos and linux.
- its budgets are measured and the numbers are in the pull request.
- every doc it touched is updated in the same pull request.
- no dead code, no unused dependency, no warning.

## never

- build past the milestone's row. a new idea goes to [scope](scope.md#after-v01) with its evidence.
- loosen or skip a test to get green. a flaky test is a bug, fixed before merge.
- keep old code beside new code. what is replaced is deleted in the same change.
