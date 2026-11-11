# network

`net` turns source requests into bounded HTTP work and verified cache objects.

## ownership

`Net` owns clients, request permits, retry policy, address checks and when a request to an S3 bucket is signed. metadata uses `Api`; file downloads use `Fetch` and the cache's partial writer, with disk writes and prefix hashing off the async workers.

## files

| file | what it holds | types and functions |
|---|---|---|
| [net.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net.rs) | lazy clients, redirect policy and concurrency | `Net`, `Wire`, `Options`, `Credential`, `Offline`, `Splitting`, `Split`, `send`, `send_on` |
| [api.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/api.rs) | bounded metadata responses | `Api`, `Response`, `json`, `text` |
| [download.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/download.rs) | cache hits, resumes, body streaming and expected hashes | `Fetch`, `Expect`, `Fetched`, `Origin`, `Event`, `download` |
| [download/split.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/download/split.rs) | adaptive range scheduling | `Job`, `Board`, `Share`, `Streamed`, `split` |
| [headers.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/headers.rs) | filenames, lengths and version validators | `file_name`, `if_range`, `changed`, `validators`, `range_start`, `range_end` |
| [failure.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/failure.rs) | classifications and retry waits | `Failure`, `Retry`, `classify`, `retry_after` |
| [pace.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/pace.rs) | per-host request quotas | `Limit`, `Paces`, `Hold`, `Ticket`, `admit`, `heard` |
| [rate.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/rate.rs) | shared byte-rate budget | `Rate`, `read`, `throttle` |
| [address.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/address.rs) | typed-host trust, DNS and proxies | `Typed`, `Proxies`, `Resolver`, `is_private` |
| [shown.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/shown.rs) | URL secret recognition and masking | `Shown`, `secret`, `mask_query` |
| [sign.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/sign.rs) | AWS signature version 4: the canonical request and the authorization header, on `hmac` and `sha2` | `Keys`, `Scope`, `Request`, `authorization`, `canonical`, `amz_date` |
| [roots.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/roots.rs) | TLS roots besides the system's: none where it has a store, mozilla's carried roots on a linux system without one, an error when `SSL_CERT_FILE` or `SSL_CERT_DIR` names unreadable ones | `extra` |
| [partial.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cache/partial.rs), [hashing.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cache/partial/hashing.rs) | download storage and ordered hash queue | `PartialWriter`, `HashPipe` |

## how it works

### download pipeline

```mermaid
flowchart TD
    request["GET response"] --> body["async response chunks"]
    body --> queue["bounded writer queue: 16 batches of 256 KiB"]
    queue --> writer["writer thread: offsets and pending bytes"]
    writer --> disk["partial file"]
    writer -->|"large or unknown length"| hash["HashPipe: separate hash thread, 16 batches of 1 MiB"]
    writer -->|"known length at most 1 MiB"| inline["hash on the writer thread"]
    disk -->|"read back joined ranges"| hash
    body --> slow{"eligible stream below 1 MB/s?"}
    slow -->|yes| handover["drain writer; retain response at written offset"]
    handover --> split["adaptive range streams"]
    split --> queue
    hash --> barrier["checkpoint barrier or final join"]
    inline --> barrier
    barrier --> finish["seal; flush and publish, or wait for the batch flush"]
    finish --> check["check expected size and hashes"]
```

`attempt` checks response status and resume offsets through `starting_offset`, then chooses a whole or split stream. `stream` owns the writer queue and joins it before reporting an error or a handover; `read_stream` owns the chunk loop and idle/rate deadlines. `split` opens and joins the range writer around `run_split`, which schedules streams and records why they stopped.

known downloads of at most 1 MiB use `HashPipe::inline` on the writer thread. larger files and unknown lengths use the separate worker described below.

`PartialWriter` shares `Bytes` chunks with `HashPipe`, so hashing can run beside disk writes without copying each response chunk. both handoffs carry batches, not single chunks: `read_stream` gathers chunks into a `Batch` of 256 KiB before waking the writer, and `HashPipe` gathers 1 MiB before waking the hash thread. waking a thread for every 16 KiB TLS record cost more than the download on a fast link; see [measurements](../performance/measurements.md#observations-and-rejected-experiments). a range, checkpoint or finish sends what is gathered first, so order holds, and every exit from the read loop flushes its batch, so a resume or split keeps every byte received. the prefix is the contiguous run of bytes from offset zero. out-of-order ranges have separate hashes; when the prefix reaches them, the writer flushes pending bytes before asking the hash worker to read those ranges back.

checkpoint messages are FIFO barriers: every earlier chunk is hashed, later chunks are excluded, and the worker checks the requested prefix length. finishing joins the worker before truncating, flushing and publishing. dropping it also joins before the partial's ownership lock can be released. the final size and checksum comparisons follow storage and can leave an unreferenced object for gc. a response declaring the wrong size is refused before its body is read, and a body without a length is stopped when it exceeds the pin.

after all response streams and the writer queue join, the download releases its network slots before final hashing, sealing and durable publication. its partial claim stays held. a checksum restart reacquires slots before sending again.

### batch durability

a download must be on disk before it takes its name in the cache, because the cache trusts an object by its name later. flushing each file cost more than downloading many small ones, so on linux 5.8 and later `fetch::all` opens a batch with `Cache::defer_downloads` for a run of more than 64 downloads. a run of up to 64 opens none and flushes each download on its own, since `syncfs` also waits for everything other programs wrote to the filesystem: on a busy disk it took 0.4 to 15 s, where 64 files flushed one by one took 0.2 s. each finished download is sealed but keeps its partial name and its claim, which holds a file open. every 256 finished downloads, the download that completes the group flushes the filesystem once with `syncfs` on the batch's handle and links that group in under its hash, releasing its claims, so a batch of thousands of files never holds more than a few hundred files open; `Cache::publish_downloads` does the same for the rest on every exit from the batch and reports the first failure of any group. groups publish one at a time, and after a failed flush no later group takes its names, since that flush may have consumed the report of a later group's write-back error. a power cut before a group's flush loses unnamed partial files, never a named object with missing bytes. a second download of the same link in one batch would wait on the first's claim, so `Net::claim` calls `Cache::publish_download_of`, which flushes and names that one download at once. windows, macos and older kernels flush each download on its own. see [deferred.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cache/deferred.rs) and the [measurement](../performance/measurements.md#observations-and-rejected-experiments).

### the 1 MB/s handover

splitting requires at least 64 MiB, range support and proof that all answers belong to one version: a pinned SHA-256, a stated MD5 or an `If-Range` validator. one stream starts normally. after a 2 s window below 1 MB/s, with at least 16 MiB left, it hands its writer and remaining response to the split scheduler. the response continues at exactly the accepted offset; those bytes are not fetched again.

the scheduler adds another stream immediately when the slow stream's rate is known. otherwise it waits for a 2 s measurement window. it checks every 250 ms, adds streams only while the last addition improved total rate by at least 15%, and stops when the host's four slots or the overall permits are taken. extra streams use separate HTTP/1.1 connections so they do not share one HTTP/2 connection's window.

a finished stream takes half the largest remaining range only when that half is at least 8 MiB. a `200` response to a range or three failed streams ends further splitting; remaining work falls back to one stream. a changed validator restarts an unpinned file. a file pinned by a SHA-256, or by a stated MD5 and its size, asks for parts without `If-Range` and keeps a part another server labels with other validators, since servers behind one name may do that for the same bytes; the final hash decides. a split checksum mismatch gets one full single-stream restart before failing.

`FETCHLOOM_RATE` disables the slow-stream judgment. `FETCHLOOM_SPLIT=0` disables splitting; `1` requests it from the first eligible answer. a rate cap leaves forced splitting alone, and a resumed checkpoint containing ranges beyond the prefix enters the split path directly to skip those ranges.

### resumption and revalidation

resumption rehashes saved bytes and needs version proof before joining them to a response. `If-Range` uses a syntactically valid strong `ETag`, or a parsed `Last-Modified` only when there was no `ETag` and the saved response `Date` is at least a minute later. that interval allows for clock uncertainty. records always store the response date, or null when the response has none; without it, a modification date can still revalidate a whole object but cannot guard a range. unsuitable ranges restart from zero. a full `200` response replaces the earlier version and discards every saved range, including a suffix saved before any prefix arrived. a resumed checksum mismatch also gets one full restart. malformed, unquoted or weak entity tags never authorize range resumption or splitting; a pinned hash can still prove the final bytes.

unpinned `get` revalidates URL records with conditional requests. project pins can reuse checked objects without that request. MD5 is computed only when a source requires it and no pinned SHA-256 already identifies the bytes.

validators also keep the `x-amz-version-id` an S3 bucket names for the bytes, never `null`. a range continuing kept bytes keeps the version of the answer that sent them, a split keeps its first answer's, and URL and stated-MD5 records keep it, so `Fetched.version` is the same whether the bytes came over the network or from the cache. a pinned download of a link whose `versionId` is refused or gone asks for the object without it once, and the pinned SHA-256 decides; other bytes are `integrity.mismatch` with the version's refusal as a detail. a public bucket that does not grant `s3:GetObjectVersion` refuses every old version, which cost one refused request per file on every run, so `Net` remembers for the run, by `Bucket::holding`, a bucket whose 401 or 403 for a version was followed by the object there now matching its pin. that bucket's later files ask for the object there now first and for the pinned version only when those bytes differ: three files take four requests rather than six. a 404 says one version is gone and is not remembered.

### request policy

the bounded download queue takes one job from each initial hostname in turn, retaining order within a host and returning results in input order. a grouped slow host cannot fill the waiting task queue before another host is admitted. host grouping uses the initial request destination, not the cache identity or a redirect destination; ports of one hostname share a group.

file bodies use pooled HTTP/1.1 connections, so concurrent bodies do not share a TCP connection. metadata keeps protocol negotiation and HTTP/2 multiplexing. both clients share cookies, checked redirects, credential scope, request permits and the byte-rate budget.

| control | value |
|---|---|
| network operations | `FETCHLOOM_JOBS`, 8 by default, with `FETCHLOOM_HOST_JOBS`, 4 by default, per initial hostname |
| connect timeout | 45 s, or `FETCHLOOM_TIMEOUT` when that is longer |
| send and response-header deadline | the connect timeout plus `FETCHLOOM_TIMEOUT` per hop, 75 s by default |
| body idle timeout | `FETCHLOOM_TIMEOUT`, 30 s by default, between chunks, not counting time held back on this machine by the rate cap, the writer or a question; no total download deadline |
| redirects | at most 10 per attempt |
| metadata body | at most 64 MiB |
| transient attempts | 5 including the first, or `FETCHLOOM_RETRIES` (0 to 20) more than the first; a download try that leaves more of the file held than any try before counts them from the first again, up to 50 tries in all. retries to one host print one warning line, then another only after 30 s of retrying |
| retry delay | jittered exponential backoff; `Retry-After` is a floor; more than 60 s fails |
| quota wait | reset within 120 s; at most 10 waits per request before consuming attempts. the message after the last try counts every request, waits included, and says how many waited |
| diversion pages | at most 3, each at most 4 MiB |

408, 425, 429, 500, 502, 503 and 504 are retryable, as are transient connection and timeout failures. a host name that DNS says does not exist fails at once with `network.unreachable`: the resolver in [address.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/address.rs) tells that answer apart from a temporary lookup failure, which is still retried. redirects and range restarts do not consume the transient-attempt budget. exhausted retries report `network.rate_limited` after a 429, `network.status` after a 5xx and `network.unreachable` otherwise; excessive requested waits report `network.rate_limited` while retries remain.

when a split download remains incomplete, a terminal stream failure keeps priority over transient stream failures in either arrival order. interruption still takes priority.

`Rate` is one shared token bucket across clients, metadata, retries and streams. `Paces` reserves host quota and keeps the lowest remaining count heard within a window. a response that omits a known policy still spends one of its remembered requests. waits remain cancellable.

metadata pagination reads every `Link` header field. angle-bracketed targets and quoted parameters keep their commas and semicolons; the first `rel` parameter may name several relations. links with an `anchor` are skipped because their context is not the requested page.

### trust boundaries

a link or dataverse host typed on the command line can name a private server, and that trust is host-wide, including other paths and ports. hosts in data.toml, data.lock or a `get -i` list get no trust of their own, since a cloned project or a downloaded list could point them at this machine's network: they reach a private server only when the `allowed_origins` setting lists its exact origin, scheme, host and port, which `Net::with_allowed_origins` passes to `Typed`. a name an allowed origin is on may resolve privately, so `Typed::check` refuses its other origins before any lookup. untrusted direct DNS results and address literals reject or filter private addresses. private covers RFC 1918, loopback, link-local, shared `100.64.0.0/10`, `0.0.0.0/8`, `192.0.0.0/24` (oracle cloud's metadata at `192.0.0.192`), documentation, multicast and `240.0.0.0/4`, plus IPv6 loopback, unique-local, link-local, site-local `fec0::/10`, multicast and documentation (`2001:db8::/32` and `3fff::/20`). `198.18.0.0/15` stays public on purpose: clash and mihomo answer every name with a fake address there. IPv4-mapped, IPv4-compatible (`::a.b.c.d`), 6to4 (`2002::/16`) and well-known NAT64 (`64:ff9b::/96`) addresses are judged by the IPv4 address they carry; the local-use NAT64 prefix `64:ff9b:1::/48` lets each network place that address, so every address in it counts as private. behind a proxy, local preflight lookup rejects an untrusted name if any answer is private; an unresolved name is left to the proxy. local preflight cannot bind the destination chosen by the proxy; its administrator owns that egress policy. preflight caches the local verdict for the run, and typed hosts skip it. that lookup happens before the send deadline.

a [mirror](../../user/mirrors.md) a person named in the settings, `FETCHLOOM_MIRRORS` or `--mirror` gets the trust of a typed host: `Context::net_with` passes the host of each web mirror to `Net::trust`, so a lab server on a private address may be one. a mirror's files are pinned downloads through the same pipeline as a source's, built by `mirror::web::object`, and its marker and trees are `Api` requests with the 64 MiB body limit. [fetch::mirrors](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/fetch/mirrors.rs) asks for the marker once a run and skips a mirror whose marker request fails, which costs one request and its retries rather than one per file.

in mirror-only mode `Net::with_mirrors_only` holds the web mirrors' addresses, and `Net::may_ask`, called by `send_on` before the client is made and again for every hop before any proxy check or lookup, refuses with `network.refused` a request that is not a mirror's or not below a mirror's address: the same scheme, host and port, and a path inside the mirror's. a request is a mirror's when its `Fetch` says so, as `mirror::web::object` makes them, or when it goes through `Net::mirror_get`, as the marker and trees do; a source's request to a mirror's host, or a redirect off the mirror, is refused all the same. the commands refuse earlier with their own words: resolving a new entry, `search`, `get`, the key checks of `auth`, and the files no mirror gave, which `fetch::mirrors::sources_allowed` names with what each mirror said.

a request for an S3 bucket the `s3_buckets` setting lists goes without a key first, so a public bucket never sees one. when S3 refuses it with 401 or 403, `send_on` marks the bucket for the run and sends the same hop once more, signed by [bucket/signing.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/bucket/signing.rs), counted as a request; later requests for that bucket are signed from the start. a bucket nobody listed is never signed for, whatever data.toml or data.lock names, and keys reach an S3-compatible endpoint only where the person's AWS settings send them. signing replaces the static credentials of that request, signs `host` and every `x-amz-` header but not `range` or the `if-` headers, never follows a redirect, and skips a link that carries a signature or another secret of its own. the response carries a `Keyed` extension, which `Response.keyed` and download refusals read to say whether a key went with the request. the authorization and the session token are sensitive header values; the secret only derives the signing key in `sign.rs` and is never sent.

redirects are checked hop by hop. credentials attach only to their configured host and port over HTTPS: port 443 unless the credential names another, as a key saved for a dataverse installation on another port does, or a `.netrc` machine written with a port. a key for port 443 never reaches another port of its host, nor the other way round, which is curl's CVE-2022-27776. a source key never returns after a redirect leaves the original origin; a `.netrc` login answers its own host on any hop, as curl does, for earthdata's sign-in round trip. the osf key is the other exception: `Account::credentials` adds it for `osf.io` and `files.osf.io` on every hop, since osf download links redirect between the two. offline mode stops both requests and DNS lookup. OSF retries a forbidden XML storage answer only when it explicitly identifies an expired `X-Amz` request.

shown URLs and account arguments share `mask_query`: it masks userinfo and recognized credential fields in both queries and fragments, including URL control-character normalization and wholly percent-encoded pasted URLs. text without credentials is unchanged. project references reject those query or fragment fields before writing data.toml or data.lock; one-off `get` can use them with masked output.

## invariants and traps

- a pinned cache miss is rechecked while holding the URL claim, including an uncontended claim. otherwise a completed publication between lookup and acquisition can cause a duplicate body request.

- keep byte processing off async workers and queues bounded. a queue size is not a bound on total project memory.
- hold the partial claim until the writer and hash worker stop using its file.
- range support without version proof is insufficient for joining bytes.
- masking is explicit. URL and partial records store only URL fingerprints. cache bodies and other metadata are not sanitized output.
- a shared writable cache trusts its writers; address checks do not isolate cache users. address tests cover radix, percent-encoded and Unicode-dot IPv4 hosts, compressed and embedded IPv6 hosts, scoped-literal refusal, and DNS failure categories.

## tests

idle and byte-rate tests use fixed virtual-clock steps and in-memory response chunks. they assert pending or complete operations and exact bytes, not elapsed wall time. real HTTP tests separately cover transport, redirects and range validation.

| files | cargo filter |
|---|---|
| [download tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/download/tests.rs), [split tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/download/split/tests.rs), [pace tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/pace/tests.rs), [credential tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/credential_tests.rs), [signature tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/net/sign.rs) | `cargo test --locked --lib net::` |
| [hash worker tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cache/partial/hashing/tests.rs), [partial tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cache/partial/tests.rs) | `cargo test --locked --lib cache::partial::` |
| [split.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/split.rs), [proxy.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/proxy.rs), [secrets.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/secrets.rs), [caps.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/caps.rs) | `cargo test --locked --test cli split::`; `cargo test --locked --test cli proxy::`; `cargo test --locked --test cli secrets::`; `cargo test --locked --test cli caps::` |

## related

[cache](cache.md), [sources](sources.md), [measurements](../performance/measurements.md), [environment](../../user/reference/environment.md).
