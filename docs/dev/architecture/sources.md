# sources

`source` turns a typed reference or pasted link into a listing whose files can be checked and pinned.

## ownership

adapters own source-specific identifiers, versions, listing pages and access errors. they return `Listing` and `Remote` values through the `Api` interface; the shared network layer owns transport, retries and credentials on redirects.

## files

| file | what it holds | types and functions |
|---|---|---|
| [source.rs](../../../src/source.rs) | reference routing, listing checks and transfer customization | `Reference`, `Parsed`, `Listing`, `Remote`, `Unavailable`, `parse`, `resolve`, `fetch_plan`, `credentials` |
| [recorded.rs](../../../src/source/recorded.rs) | in-memory answers for adapter tests | `Recorded`, `on`, `fixture`, `next`, `asked`, `block_on` |
| [doi.rs](../../../src/source/doi.rs) | handle resolution and destination routing | `Doi`, `locate` |
| [zenodo.rs](../../../src/source/zenodo.rs) | record API and stated MD5 values | `Record`, `resolve` |
| [figshare.rs](../../../src/source/figshare.rs) | articles and explicit versions | `Article`, `resolve` |
| [dataverse.rs](../../../src/source/dataverse.rs), [dataverse/version.rs](../../../src/source/dataverse/version.rs), [dataverse/file.rs](../../../src/source/dataverse/file.rs) | install host, published versions and individual file links | `Dataset`, `Version`, `listing`, `resolve` |
| [osf.rs](../../../src/source/osf.rs), [osf/walk.rs](../../../src/source/osf/walk.rs) | nodes, folder pages and storage-link expiry | `Node`, `Entry`, `resolve`, `files`, `expiry_check` |
| [hf.rs](../../../src/source/hf.rs), [hf/listing.rs](../../../src/source/hf/listing.rs), [hf/revision.rs](../../../src/source/hf/revision.rs), [hf/tree.rs](../../../src/source/hf/tree.rs) | repositories resolved and listed at one commit | `Repo`, `RepoKind`, `Info`, `resolve`, `revision`, `tree` |
| [hf/token.rs](../../../src/source/hf/token.rs) | hugging face token discovery | `find` |
| [kaggle.rs](../../../src/source/kaggle.rs), [kaggle/listing.rs](../../../src/source/kaggle/listing.rs), [kaggle/token.rs](../../../src/source/kaggle/token.rs) | dataset versions, paged files and credentials | `Dataset`, `resolve`, `find` |
| [drive.rs](../../../src/source/drive.rs), [drive/folder.rs](../../../src/source/drive/folder.rs), [drive/link.rs](../../../src/source/drive/link.rs) | shared files, recursive folders and ambiguous open links | `Item`, `resolve`, `list`, `opened` |
| [drive/page.rs](../../../src/source/drive/page.rs), [drive/html.rs](../../../src/source/drive/html.rs) | confirmation forms and access-error pages | `divert`, `between`, `attribute`, `text`, `unescape` |
| [dropbox.rs](../../../src/source/dropbox.rs) | shared links, including folders served as zip | `Link`, `listing`, `fetch_plan` |

## how it works

### parse before resolving

`Reference::parse` recognizes identifiers and known pasted-link shapes. `Parsed.file` keeps an individual file selection embedded in a link. an unrecognized page on a known source host fails instead of being mistaken for a plain downloadable file.

DOIs ask the JSON handle API at `doi.org` for their registered URL. that URL's host selects an adapter. unknown hosts and another DOI are refused without fetching a landing page; DOI prefixes do not select a repository.

### keep listing and transfer separate

`Listing.resolved` names the version to resolve again where the source supports one. each `Remote` carries a relative path, URL and any known size, SHA-256 or MD5. unavailable entries retain their paths and reasons so filters can explicitly leave them out.

listings accept at most 100,000 files per dataset before selection; OSF's walk also counts folders. pagination is handled in the adapter and each page is parsed once. this cap does not bound total project size or the number of members inside archives.

`fetch_plan` adds download-page handlers for drive and dropbox and expired-storage-link recognition for OSF. stable source URLs stay in the lock; redirected storage URLs and credentials do not become pins.

### add an adapter

1. implement parsing, display and listing resolution in a source module.
2. add a `Reference` variant and register identifier parsing, pasted-link routing, display and resolution in `source.rs`.
3. preserve unavailable entries, bound listing work and pin the source's version when it offers one.
4. add credential discovery or a response handler only if the source needs it; keep origin policy in `Net`.
5. cover listing success, paging, versioning and refusal with `Recorded`; add a real-binary case when download or command behavior changes.

## invariants and traps

- credentials come from the context, never a `Remote` field or a serialized lock.
- project references reject recognized secret query keys. masking a URL is explicit through `Shown` or `shown`, not an invariant of `url::Url`.
- a reported MD5 is checked against bytes before its mapping to SHA-256 can be reused.
- mutable services cannot guarantee historical listings. the lock's hashes establish byte identity even when a source changes.
- tests use recorded responses or local servers. they do not prove a live service still follows its API.

## tests

| files | cargo filter |
|---|---|
| [source.rs](../../../src/source.rs), adapter module tests and [source fixtures](../../../tests/fixtures/sources) | `cargo test --locked --lib source::` |
| [zenodo.rs](../../../src/source/zenodo.rs) | `cargo test --locked --lib source::zenodo::` |
| [DOI tests](../../../src/source/doi/tests.rs), [dataverse tests](../../../src/source/dataverse/tests.rs) | `cargo test --locked --lib source::doi::`; `cargo test --locked --lib source::dataverse::` |
| [drive folder tests](../../../src/source/drive/folder_tests.rs), [OSF walk tests](../../../src/source/osf/walk_tests.rs) | `cargo test --locked --lib source::drive::`; `cargo test --locked --lib source::osf::` |
| [get.rs](../../../tests/cli/get.rs), [project.rs](../../../tests/cli/project.rs), [secrets.rs](../../../tests/cli/secrets.rs) | `cargo test --locked --test cli get::`; `cargo test --locked --test cli project::`; `cargo test --locked --test cli secrets::` |

replace the adapter name in the module selector for figshare, hugging face, kaggle or dropbox. there is no shared adapter conformance suite.

## related

[network](network.md), [sync loop](sync-loop.md), [source behavior](../../user/sources/README.md), [data.lock](../../user/reference/data-lock.md), [fuzzing](../engineering/fuzzing.md).
