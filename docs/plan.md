# plan

## milestones

| # | milestone | built | left |
|---|---|---|---|
| 1 | command shell | both executables, welcome and project status, help, colors, JSON, errors, progress and interruption | help that follows color settings, progress lines on narrow terminals |
| 2 | one file | URL `get`, cache objects, conditional requests, retries, resume that starts over when a server answers with another version, copies published only once complete | |
| 3 | project loop | add, remove, sync, lock, upgrade, check, status and path; offline use, hand-edit reporting, interrupted replacements adopted when they hold the locked bytes | one project used from windows and WSL at the same time; paths that differ only by case or unicode form on case-insensitive or normalizing filesystems |
| 4 | archives | zip and the tar family, member checks, tree hashes, staged publication, placement and packed-object cleanup, run on ntfs, refs, exfat, fat32, ext4, btrfs and xfs in CI | staged members after a hard kill wait 24 hours for gc |
| 5 | main source adapters | zenodo, hugging face and kaggle listings at pinned revisions, drive files and folders with confirmation and quota handling | |
| 6 | other source adapters | DOI handles, figshare versions, dataverse datasets and file pages, OSF folders and file GUIDs, dropbox files and folder zips | |
| 7 | cache and recovery | inspection, gc, verification that tells unreadable from changed, change marks that are hashed before use, re-resolution after a locked 404 or 410 | flushing copies before the state that vouches for them |
| 8 | distribution | source install and first-use docs; a release workflow that builds, checks and attests static windows and linux binaries and wheels into a draft release; installers; the python `path()` and `sync()` helpers | the first published release, PyPI trusted publishing, signed windows binaries |

every source was checked against the live service, and CI runs every test on linux and windows.

## next

1. lock one project across windows and WSL. both take their own OS lock on `.fetchloom/lock` and do not see each other's. a `.fetchloom/owner` record both sides honor closes it: stale when its os is this one and this process holds the OS lock, or when it is empty and older than ten seconds; any other record means wait and name the path to delete.
2. track placed files by the filesystem's identity where names alias: a case-only rename on ntfs, a name a source sends in another unicode form, a `data_dir` whose case changed. today such a file reads as missing plus extra, and `sync --force` stops at the existing name.
3. flush copied files before the state is written, if a measurement shows the cost is small; clean staged archive members from a killed run without waiting a day; replace a damaged cache object by renaming the good copy over it, so two processes repairing it at once never leave its name empty, which today can fail the other's placement until the next run.
4. make help follow `--color` and `NO_COLOR`, and keep the file name when a terminal is too narrow for the whole progress line.
5. close the budget misses [performance](performance.md#budgets) records: placing 10,000 files on windows takes 2.7 s where bare links take 1.3 s; adding 100,000 files peaks at 162 MiB on linux; small archive members take 1.4 to 1.6 times bsdtar's time on windows, where defender scans each new file as it closes, and bsdtar's files scan faster for a reason defender's performance recorder could show with an administrator; http/2 is on but unmeasured, and since four requests run per host it can save only a few TLS handshakes, so measure it on a real many-file dataset before keeping or removing it; a 1 GiB download runs at half of curl's speed on this machine, bound by hashing and the flush.
6. merge the duplicate test helpers and the two `real_folder` functions, drop the cache's unread `layout` marker, and remove doc comments that restate their item.
7. publish 0.1.0 as [releases](engineering.md#releases) lists, with the wheels uploaded to PyPI by hand. then let release.yml publish them through a PyPI trusted publisher, and sign the windows binaries: on a managed windows machine, defender's ransomware rule blocked the unsigned `fl.exe` from a release build while `fetchloom.exe` ran. the source install in the README clones the default branch, which holds the old project until the rebuild replaces it.
8. have the maintainer and two or three researchers use real projects. record failed links, confusing output and recovery work before expanding the command set.

## a release is ready when

- a clean clone can sync the locked bytes on every supported platform, or stop with the right error.
- repeat, offline, edited-file, missing-link and interrupted-transfer cases have regression coverage.
- supported sources pass their recorded cases and have been checked against the live service.
- binaries, installers and wheels are installed and exercised from their built artifacts.
- format changes, command output and known limits agree with the docs.

new commands and source families outside that work stay in [scope](scope.md#outside-the-project) until a user's project needs them.
