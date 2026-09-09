# security

## reporting

open a private security advisory on the repository, or email the address on the
commit history. say what you did, what happened, and what you expected. a proof
of concept helps and is not required.

expect an acknowledgement within a week. there is no bounty.

nothing is released yet, so there are no supported versions and no backports.
the fix lands on `main`.

## in scope

fetchloom fetches data from servers you do not control and unpacks it on your
machine. that is the whole threat model.

archive handling. a member escaping the destination, by an absolute path, by
traversal, by a symlink, by a hard link, or by a name the volume treats
differently than the bytes suggest. an archive that exhausts memory or disk
through entry count, expanded size, or expansion ratio.

resolution. a manifest, listing, or metadata document that causes unbounded
memory use, unbounded time, or a request to somewhere the reference did not
name.

credentials. a token reaching any output stream, any file, or any host other
than the one it was resolved for, including across a redirect.

integrity. any path that reports a trust class higher than the evidence
supports, or that publishes bytes into the cache or a destination without them
being verified whole.

offline. any network activity under `--offline`.

## not in scope

compromise of a source you pointed at. if a publisher serves different bytes,
fetchloom reports the mismatch. it cannot decide which bytes were correct.

anything requiring write access to the cache directory or the configuration
files. someone who can write there is already inside the boundary.

speed. being slower than another tool is not a vulnerability.

## fixed

zip symlink bypass. a symlink member's bytes were read before the expansion
guard observed them, so a 100 MB zip could force an allocation of roughly 100
GB. deflate reaches 1032 to 1. the guard now observes the size before the read.

exponential glob. selection patterns backtracked, so eight `*` components took
143 ms and each additional one multiplied that by about 5.8. patterns arrive in
remote manifests, so this was remotely reachable. matching is now non
backtracking.

unbounded document reads. a manifest or plan was read whole before its size was
checked.

## notes for a reviewer

manifests are declarative. no key can express a command, a script, or a path to
execute. there are no hooks and no generators.

every rejection stops the entire run. no member is ever skipped, and excluding a
rejected member with `--select` does not let the archive through.

query parameter values are redacted as a class rather than by a list of
sensitive names, because a list can be incomplete. redaction happens where the
value is constructed, not where it is printed.

fetchloom lists what you point at and never follows a link outside that prefix,
never opens a file to discover more work, and never executes anything.

TLS verification is on and there is no flag or configuration key that turns it
off.

an FTP data connection is opened to the address the control connection is
already talking to. the address a `PASV` reply names is read for its port and
discarded, so a server cannot point a transfer at a third party. a reference
written `ftps://` fails rather than continuing in the clear, no flag turns that
off, and a credential resolved for a host is never sent over a control
connection that could not be secured. SFTP is not spoken.
