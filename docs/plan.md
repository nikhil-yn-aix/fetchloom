# plan

## milestones

| # | milestone | built | left |
|---|---|---|---|
| 1 | command shell | both executables, welcome and project status, help that follows color settings, colors, JSON, errors, progress that keeps file names on narrow terminals, and interruption, including at a download question | |
| 2 | one file | URL `get`, cache objects, conditional requests, retries, resume that starts over when a server answers with another version, copies published only once complete | |
| 3 | project loop | add, remove, sync, lock, upgrade, check, status and path; offline use, hand-edit reporting, interrupted replacements adopted when they hold the locked bytes; one project used from windows and WSL through an owner record; placed files tracked through case and unicode aliases | |
| 4 | archives | zip and the tar family, member checks, tree hashes, staged publication, cleanup after killed unpacking runs, placement and packed-object cleanup, run on ntfs, refs, exfat, fat32, ext4, btrfs and xfs in CI | |
| 5 | main source adapters | zenodo, hugging face and kaggle listings at pinned revisions, drive files and folders with confirmation and quota handling | |
| 6 | other source adapters | DOI handles, figshare versions, dataverse datasets and file pages, OSF folders and file GUIDs, dropbox files and folder zips | |
| 7 | cache and recovery | inspection, gc, verification that tells unreadable from changed, change marks that are hashed before use, atomic replacement of damaged objects, re-resolution after a locked 404 or 410 | flushing copies before the state that vouches for them |
| 8 | distribution | source install and first-use docs; a release workflow that builds, checks and attests static windows and linux binaries and wheels into a draft release; installers; the python `path()` and `sync()` helpers | the first published release, PyPI trusted publishing, signed windows binaries |

every source was checked against the live service, and CI runs every test on linux and windows.

## next

1. flush copied files before the state is written, if a measurement shows the cost is small.
2. close the budget misses [performance](performance.md#budgets) records: placing 10,000 files on windows takes 2.7 s where bare links take 1.3 s; adding 100,000 files peaks at 162 MiB on linux; small archive members take 1.4 to 1.6 times bsdtar's time on windows, where defender scans each new file as it closes, and bsdtar's files scan faster for a reason defender's performance recorder could show with an administrator; http/2 is on but unmeasured, and since four requests run per host it can save only a few TLS handshakes, so measure it on a real many-file dataset before keeping or removing it; a 1 GiB download runs at half of curl's speed on this machine, bound by hashing and the flush.
3. publish 0.1.0 as [releases](engineering.md#releases) lists, with the wheels uploaded to PyPI by hand. then let release.yml publish them through a PyPI trusted publisher, and sign the windows binaries: on a managed windows machine, defender's ransomware rule blocked the unsigned `fl.exe` from a release build while `fetchloom.exe` ran. the source install in the README clones the default branch, which holds the old project until the rebuild replaces it.
4. have the maintainer and two or three researchers use real projects. record failed links, confusing output and recovery work before expanding the command set.

## a release is ready when

- a clean clone can sync the locked bytes on every supported platform, or stop with the right error.
- repeat, offline, edited-file, missing-link and interrupted-transfer cases have regression coverage.
- supported sources pass their recorded cases and have been checked against the live service.
- binaries, installers and wheels are installed and exercised from their built artifacts.
- format changes, command output and known limits agree with the docs.

new commands and source families outside that work stay in [scope](scope.md#outside-the-project) until a user's project needs them.
