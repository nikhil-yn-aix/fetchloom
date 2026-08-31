# Vision

Fetchloom turns a dataset reference into exact, ready-to-use local files, and proves what it delivered.

```
fetchloom get silesia
```

One command resolves the reference, reuses valid local content, downloads what is missing, verifies it, materializes it safely, and records what happened. No account, no daemon, no project setup, no language runtime.

## The core idea

Pinning is the product. Verification is the mechanism.

Most public datasets publish no checksums, so the reference truth is usually the user's own first successful fetch. Fetchloom captures that moment in a lock, and every later run is verified against it. This is what Cargo.lock does for code, applied to data.

## Promises

Exact results. A name or alias resolves to immutable content digests. A lock preserves that resolution across machines and time.

Safe repetition. The same request twice does no redundant work, creates no numbered folders, and never overwrites files the user changed.

Safe failure. Cancellation, power loss, full disks, and dropped connections leave either the previous valid result or resumable staging state. Never a half-written destination.

Honest verification. Every result carries a trust class with a mechanical definition. Size, timestamps, and ETags never masquerade as content identity.

Locating repair. Damage is found at the byte range, not just the file. A corrupt region in a large object is repaired by refetching that region.

Useful errors. Every failure names the layer, the dataset, the artifact, the source, the retry state, and the next safe action.

Automation without surprises. Machine-readable output, stable exit codes, and strict non-interactive behavior are contracts, not conveniences.

Durable artifacts. From 1.0, a lock, receipt, manifest, plan, or bundle written by any release is readable by every later release. Content addressing makes that a property of the design rather than a burden on the code.

## Freedoms

These override feature requests. A feature that breaks one does not ship.

Freedom from install. One static binary, no root, no runtime, no daemon, no account.

Freedom from the network. Every operation declares its network need. Offline means no DNS, no probe, no credential call, no update check.

Freedom from lock-in. Nothing Fetchloom writes needs Fetchloom to read. Locks, receipts and plans are readable text, manifests use open formats, and every byte in the cache is either a file of its own or a span of a pack that states, ahead of each object, the digest it is under and how long it is. Deleting Fetchloom leaves the data usable.

Freedom from lies. Trust class is always named. Degradation is always announced.

Freedom from tuning. Defaults are measured at runtime, not guessed by the user. Every knob exists; nobody should need it.

## Who this is for

Researchers on unreliable networks who lose large downloads to corruption and restarts.

Cluster and air-gapped users who must plan on one machine and execute on another with no internet.

Reproducibility reviewers who need to prove a tree years later.

CI pipelines that need hermetic, cached, non-interactive fetches.

Anyone pulling from the long tail: repositories, archives, government portals, and lab web servers where no good tool exists.

## Where data lives

The cache holds immutable verified objects and safe partial transfers, in the operating system's per-user cache location. One object serves many projects. It is inspectable, verifiable, pinnable, and prunable.

The destination is the user-visible dataset directory. It is reconciled, never clobbered.

Staging holds incomplete work. Staged content never appears as a cache object or a destination until it is verified.

Locks are portable and contain no machine paths. Receipts are local and may.

## Not part of Fetchloom

Data cleaning, conversion, or any post-download code execution.

Workflow scheduling, experiment tracking, data version control, backup, or sync.

Mandatory cloud accounts, hosted registries, daemons, or a proprietary lock format.

General web scraping, custom peer-to-peer transport, or a new storage protocol.

Automatic license acceptance, or any claim that metadata proves legal permission.

Optimizations whose correctness depends on mutable cache files or undocumented filesystem behavior.

## The one hard rule

The same lock produces a byte-identical tree on Windows and Linux, or Fetchloom fails and names the exact reason. It never produces a quietly different tree.
