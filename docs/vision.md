# vision

a research result can depend on data that changed after the code was published. a list of download links does not record which bytes the analysis used.

fetchloom records the selected files, their sizes and SHA-256 hashes in a lock that travels with the code. someone running the project later can request those bytes and detect a different download. the manifest keeps the source references and selection readable enough to review.

## who it serves

the first reader shares research code with published data inputs. the next reader clones that code and needs the same inputs without repeating a page of browser downloads and folder renames.

students and researchers also reuse datasets across projects. a content-addressed cache lets those projects share cached objects. reflinks and hardlinks avoid another full file copy where available; writable files and copy fallbacks still cost space.

labs on clusters need the same reuse, but sharing a writable cache between users also needs permission and retention rules. those rules are not implemented as lab administration features yet.

## why a project lock

a source reference is useful for finding a dataset. it may still point to different files tomorrow. the lock preserves the selected paths and hashes so routine sync does not ask a mutable source for its newest data.

keeping the lock separate from the cache lets a project name its inputs without putting them in git. a cached object can serve several projects, and another machine can download the locked bytes when it lacks them. the cache is disposable; the lock is part of the research project.

a lock cannot restore bytes that every host and cache has lost. it also does not authenticate the publisher or make a writable shared cache trustworthy. long-term preservation still needs someone to retain the data.

## boundaries

fetchloom retrieves inputs. it does not run preprocessing, publish datasets or reproduce an analysis by itself. a workflow can ask `fetchloom path` for a location and then run its own work. [scope](scope.md#outside-the-project) keeps those boundaries separate from unfinished release work.

one-off `get` serves a different need: fetch what a link serves now without creating a project. reproducible inputs use `data.toml` and `data.lock`. [plan](plan.md) records what is built and what still stands between that loop and a release.
