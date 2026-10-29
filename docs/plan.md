# plan

## milestones

| # | milestone | built | left |
|---|---|---|---|
| 1 | command shell | both executables, welcome and project status, help, colors, JSON, errors, progress and interruption | a first passing CI run on all three platforms, and release dry runs |
| 2 | one file | URL `get`, cache objects, conditional requests, retries, checkpointed resume, placement | tighten destination ownership and interrupted replacement |
| 3 | project loop | add, remove, sync, lock, upgrade, check, status and path; offline use and hand-edit reporting | fix writable-placement transitions and stale reads around concurrent locking |
| 4 | archives | zip and the tar family, member checks, tree hashes, staged publication, placement and packed-object cleanup | run the corpus on the platform matrix |
| 5 | main source adapters | zenodo listing; hugging face and kaggle parsers; drive file URL routing | hugging face and kaggle listing and credentials; drive folders, confirmation and quota handling |
| 6 | other source adapters | DOI handles, figshare versions, dataverse datasets, OSF folders and file GUIDs, dropbox download-link rewriting | dataverse file pages, dropbox page handling and common source conformance cases |
| 7 | cache and recovery | inspection, gc, verification, integrity failures, re-resolution after a locked 404 or 410 | close publication races and shared-hardlink integrity gaps |
| 8 | distribution | source install and first-use docs | python `path()` wrapper, wheels, binaries and installers, publishing and release checks |

source work and cache work have landed before the automation and packaging milestones. a working command does not imply its release checks are in place.

## next

1. finish the source adapters and test their access failures, paging and filename discovery. parsing a source reference is not enough to mark that source done.
2. close the data-safety gaps: atomic file replacement, ownership of adopted and one-off files, writable-copy transitions, changed MD5 carry-forward, and project reads under the project lock. hardlink reuse and the cache's rename fallback need their own regression cases.
3. get [CI](../.github/workflows/ci.yml) passing on windows, macos and linux. it runs the source fixtures and archive corpora, so a platform counts as supported only once its job passes.
4. add a python wrapper around `path --json` and test the installed wheel. build standalone release artifacts and installers. publishing and attestations need configured workflows, not only a version tag convention.
5. measure release builds against [performance](performance.md), with cache state, placement method and hardware recorded. fix excess requests and copies before setting speed claims.
6. have the maintainer and two or three researchers use real projects. record failed links, confusing output and recovery work before expanding the command set.

## a release is ready when

- a clean clone can sync the locked bytes on every supported platform, or stop with the right error.
- repeat, offline, edited-file, missing-link and interrupted-transfer cases have regression coverage.
- supported sources pass their recorded cases and have been checked against the live service.
- binaries, installers and wheels are installed and exercised from their built artifacts.
- format changes, command output and known limits agree with the docs.

new commands and source families outside that work stay in [scope](scope.md#outside-the-project) until a user's project needs them.
