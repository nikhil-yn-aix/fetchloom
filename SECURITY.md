# Security

## Reporting

Open a private security advisory on the repository, or email the address on the commit history. Include what you did, what happened, and what you expected. A proof of concept helps and is not required.

Expect an acknowledgement within a week. There is no bounty.

## What is in scope

Fetchloom fetches data from servers you do not control and unpacks it on your machine. That is the whole threat model.

**Archive handling.** A member escaping the destination, whether by an absolute path, traversal, a symlink, a hard link, or a name the volume treats differently than the bytes suggest. An archive that exhausts memory or disk through entry count, expanded size, or expansion ratio.

**Resolution.** A manifest, listing, or metadata document that causes unbounded memory use, unbounded time, or a request to somewhere the reference did not name.

**Credentials.** A token reaching any output stream, any file, or any host other than the one it was resolved for, including across a redirect.

**Integrity.** Any path that reports a trust class higher than the evidence supports, or that publishes bytes into the cache or a destination without them being verified whole.

**Offline.** Any network activity under `--offline`.

## What is not

Compromise of a source you pointed at. If a publisher serves different bytes, Fetchloom reports the mismatch. It cannot decide which bytes were correct.

Anything requiring write access to the cache directory or the configuration files. A local attacker who can write there is already inside the boundary.

Speed. Being slower than another tool is not a vulnerability.

## Fixed

**Zip symlink bypass.** A symlink member's bytes were read before the expansion guard observed them, so a 100 MB zip could force an allocation of roughly 100 GB. Deflate reaches 1032 to 1. The guard now observes the size before the read.

**Exponential glob.** Selection patterns backtracked, so eight `*` components took 143 ms and each additional one multiplied that by about 5.8. Patterns arrive in remote manifests, so this was remotely reachable. Matching is now non backtracking.

**Unbounded document reads.** A manifest or plan was read whole before its size was checked.

## Design notes relevant to review

Manifests are declarative. No key can express a command, a script, or a path to execute. There are no hooks and no generators.

Every rejection stops the entire run. No member is ever skipped, and excluding a rejected member with `--select` does not let the archive through.

Query parameter values are redacted as a class rather than by a list of sensitive names, because a list can be incomplete. Redaction happens where the value is constructed, not where it is printed.

Fetchloom lists what you point at and never follows a link outside that prefix, never opens a file to discover more work, and never executes anything.

TLS verification is on and there is no flag or configuration key that turns it off.

An FTP data connection is opened to the address the control connection is already talking to. The address a `PASV` reply names is read for its port and discarded, so a server cannot point a transfer at a third party. A reference written `ftps://` fails rather than continuing in the clear, no flag turns that off, and a credential resolved for a host is never sent over a control connection that could not be secured. SFTP is not spoken.
