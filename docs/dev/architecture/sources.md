# sources

`source` turns a typed reference or pasted link into a listing whose files can be checked and pinned.

## ownership

adapters own source-specific identifiers, versions, listing pages and access errors. they return `Listing` and `Remote` values through the `Api` interface; the shared network layer owns transport, retries and credentials on redirects.

## files

| file | what it holds | types and functions |
|---|---|---|
| [source.rs](../../../src/source.rs) | reference routing, listing checks and transfer customization | `Reference`, `Parsed`, `Listing`, `Remote`, `Unavailable`, `parse`, `resolve`, `fetch_plan`, `credentials` |
| [search.rs](../../../src/source/search.rs), [search/tests.rs](../../../src/source/search/tests.rs) | public catalog queries and interleaved candidates | `Candidate`, `Search`, `search`, `is_search` |
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

github and gitlab file-page URLs are normalized to raw URLs before ordinary URL resolution. [URL references](../../user/sources/urls-and-dois.md) owns accepted forms and HTML warnings.

DOIs ask the JSON handle API at `doi.org` for their registered URL. that URL's host selects an adapter. unknown hosts and another DOI are refused without fetching a landing page; DOI prefixes do not select a repository.

### catalog search

`is_search` recognizes alphanumeric input without a colon, excluding bare DOIs. `search` uses `tokio::join!` for hugging face, kaggle and zenodo, takes up to three matches each and interleaves their rank order: hugging face, kaggle, zenodo, then the next result from each. hugging face is requested with `sort=downloads` and descending direction, zenodo with `sort=mostviewed`, and kaggle with its default catalog ordering. fetchloom preserves each catalog's order; it computes no global score. `Candidate` carries a canonical reference and optional catalog facts. one failing catalog contributes a warning; all failing returns the first error. CLI selection is separate from source resolution.

### keep listing and transfer separate

`Listing.resolved` names the version to resolve again where the source supports one. each `Remote` carries a relative path, URL and any known size, SHA-256 or MD5. unavailable entries retain their paths and reasons so filters can explicitly leave them out.

listings accept at most 100,000 files per dataset before selection; OSF's walk also counts folders. pagination is handled in the adapter and each page is parsed once. this cap does not bound total project size or the number of members inside archives.

`fetch_plan` adds download-page handlers for drive and dropbox and expired-storage-link recognition for OSF. stable source URLs stay in the lock; redirected storage URLs and credentials do not become pins.

### add an adapter

1. declare the adapter module and a `Reference` variant in `source.rs`. implement identifier parsing and display in the adapter, then wire `Reference::parse`, `Display`, pasted links in `link` and the relevant `known_host` cases. keep `Parsed.file` for file-specific links.
2. wire `resolve_direct` and, for a DOI-capable repository, destination routing in [doi.rs](../../../src/source/doi.rs). return a checked `Listing`, preserve unavailable entries, bound pagination and pin the source's version when it offers one.
3. update `fetch_plan` only if transfers need a page diversion or expired-link handler. keep credential origins and redirects in `Net`; add discovery through `source::credentials` when needed.
4. choose existing [error kinds](../../../src/error.rs) for invalid references, refused access, unavailable data and malformed answers. update `source::token_for` and the adapter's access help to name credentials actually read. if a new kind is necessary, update its serialized name, exit mapping and [exit-code reference](../../user/reference/exit-codes.md).
5. add recorded responses under [tests/fixtures/sources](../../../tests/fixtures/sources) and `Recorded` tests for parsing, display, paging, pins, missing files, access failures and malformed replies. cover command or transfer effects in [CLI tests](../../../tests/cli).
6. add an ignored live test against a small public record, stating the host in its ignore reason. keep it separate from deterministic recorded coverage.
7. if the source has a public search catalog, add its query and candidate mapping in [source/search.rs](../../../src/source/search.rs), recorded ranking/error tests in [search/tests.rs](../../../src/source/search/tests.rs), and update the documented catalog order and [CLI search tests](../../../tests/cli/search.rs).
8. add a page under [docs/user/sources](../../user/sources/README.md) with accepted forms, pinning, limits and access rules. link it from that index, the [user guide](../../user/README.md) and [llms.txt](../../../llms.txt). update [environment settings](../../user/reference/environment.md) for credentials, relevant examples and help text in [cli/args.rs](../../../src/cli/args.rs) or [cli/help.rs](../../../src/cli/help.rs), and the affected help snapshots.

## invariants and traps

- credentials come from the context, never a `Remote` field or a serialized lock.
- project references reject recognized secret query keys. masking a URL is explicit through `Shown` or `shown`, not an invariant of `url::Url`.
- a reported MD5 is checked against bytes before its mapping to SHA-256 can be reused.
- mutable services cannot guarantee historical listings. the lock's hashes establish byte identity even when a source changes.
- default tests use recorded responses or local servers. [ignored live tests](../engineering/ci.md#live-source-checks) check current services separately.

## tests

| files | cargo filter |
|---|---|
| [source.rs](../../../src/source.rs), adapter module tests and [source fixtures](../../../tests/fixtures/sources) | `cargo test --locked --lib source::` |
| [search/tests.rs](../../../src/source/search/tests.rs), [CLI search tests](../../../tests/cli/search.rs) | `cargo test --locked --lib source::search::`; `cargo test --locked --test cli search::` |
| [zenodo.rs](../../../src/source/zenodo.rs) | `cargo test --locked --lib source::zenodo::` |
| [DOI tests](../../../src/source/doi/tests.rs), [dataverse tests](../../../src/source/dataverse/tests.rs) | `cargo test --locked --lib source::doi::`; `cargo test --locked --lib source::dataverse::` |
| [drive folder tests](../../../src/source/drive/folder_tests.rs), [OSF walk tests](../../../src/source/osf/walk_tests.rs) | `cargo test --locked --lib source::drive::`; `cargo test --locked --lib source::osf::` |
| [get.rs](../../../tests/cli/get.rs), [project.rs](../../../tests/cli/project.rs), [secrets.rs](../../../tests/cli/secrets.rs) | `cargo test --locked --test cli get::`; `cargo test --locked --test cli project::`; `cargo test --locked --test cli secrets::` |

replace the adapter name in the module selector for figshare, hugging face, kaggle or dropbox. there is no shared adapter conformance suite.

## related

[network](network.md), [sync loop](sync-loop.md), [source behavior](../../user/sources/README.md), [data.lock](../../user/reference/data-lock.md), [fuzzing](../engineering/fuzzing.md).
