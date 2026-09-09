# security

## reporting

open a private security advisory on the repository, which is enabled, or email
the address on the commit history. say what you did, what happened, and what you
expected. a proof of concept helps and is not required.

expect an acknowledgement within a week. there is no bounty.

nothing is released yet, so there are no supported versions and no backports.
the fix lands on `main`.

## the boundary

fetchloom fetches data from servers you do not control, unpacks it on your
machine, and keeps what it fetched in a cache that may be shared between users
on that machine.

three parties are outside the boundary and everything they control is hostile
input: the server, whatever document it serves, and any other local user who can
reach a shared cache directory. the person running the command is inside it.

## in scope

archive handling. a member escaping the destination, by an absolute path, by
traversal, by a symlink, by a hard link, or by a name the volume treats
differently than the bytes suggest. an archive that exhausts memory or disk
through entry count, expanded size, or expansion ratio, whether the size it
declares is true or not.

resolution. a manifest, listing, or metadata document that causes unbounded
memory use, unbounded time, a request to somewhere the reference did not name,
or a read of a file on this machine the person did not name.

credentials. a token reaching any output stream, any file, or any host other
than the one it was resolved for, including across a redirect, and a token
reaching a connection that is not secured.

addresses. a document or a redirect causing a connection to an address inside
this machine or the network it runs in, the cloud metadata endpoint above all.

integrity. any path that reports a trust class higher than the evidence
supports, or that publishes bytes into the cache or a destination without them
being verified whole.

the local cache. a user who can write in a shared cache directory causing
another user's run to write outside that directory, to read bytes under a digest
that are not that digest's bytes, or to lose an object they own.

offline. any network activity under `--offline`.

## not in scope

compromise of a source you pointed at. if a publisher serves different bytes,
fetchloom reports the mismatch. it cannot decide which bytes were correct.

a cache directory whose permissions the person set themselves. fetchloom creates
a cache private when its parent is private and shared when its parent is already
shared, and never widens one that exists. someone who was given write access to
a directory you made world writable was given it by you.

the configuration files, which are the person's own.

speed. being slower than another tool is not a vulnerability.

## known and stated

the address check reads what a name resolves to at the moment it is made, and
the transport resolves the name again when it connects. a name that answers
publicly to the check and privately to the transport is not closed by this.
closing it needs the resolver the transport itself uses.

a credential may be sent over `http` to a host whose every address is on this
machine, because there is no wire between the two ends. every other unsecured
host is refused.

a location the person types on the command line is not address checked. they can
already reach any address on their own machine without this tool. what a
document or a redirect names is checked.

## fixed

local privilege escalation through the cache. every cache directory was created
world writable whatever the person's own directory permitted, and five writes
inside them opened a fixed or guessable name with a call that follows a symlink
and truncates. another local user could plant a link and have the next run write
through it as the victim. the mode now follows the directory the cache was put
in, and every write creates its file exclusively.

a cache hit taken on a stat fingerprint. every field in a fingerprint is a field
whoever wrote the file chose, so on a shared cache an object this process does
not own is hashed rather than stat'd.

a bearer token over `http`. credentials resolved by host with no condition on
the scheme, so a token went out in the clear to a host reached over `http`. FTP
had this guard and HTTP did not.

no address policy. a manifest, listing or redirect naming `169.254.169.254` was
fetched and cached, which is a credential exfiltration primitive on any cloud
host.

unbounded zip decompression. `open_body` bounded the compressed size and the
expansion guard observed the member after it was written whole, so a 100 MB zip
wrote about 103 GB before `archive.bomb` fired. the stream is now held to the
size the guard was shown.

a silent `https` to `http` redirect. TLS was dropped without a word, where
nothing else in this build degrades silently.

a remote manifest naming an absolute local path and having it read.

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
value is constructed, not where it is printed, and the fields that carry a
location are typed so that a site which forgets does not compile.

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

the requests a run issues carry `fetchloom/<version>` and nothing about the
machine.
