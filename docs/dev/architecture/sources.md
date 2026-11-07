# sources

`source` turns a typed reference or pasted link into a listing whose files can be checked and pinned.

## ownership

adapters own source-specific identifiers, versions, listing pages and access errors. they return `Listing` and `Remote` values through the `Api` interface; the shared network layer owns transport, retries and credentials on redirects.

## files

| file | what it holds | types and functions |
|---|---|---|
| [source.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source.rs) | reference routing, listing checks and transfer customization | `Reference`, `Parsed`, `Listing`, `Remote`, `Unavailable`, `parse`, `resolve`, `fetch_plan`, `may_wrap`, `refused` |
| [search.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/search.rs), [search/tests.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/search/tests.rs) | public catalog queries and interleaved candidates | `Candidate`, `Search`, `search`, `is_search` |
| [recorded.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/recorded.rs) | in-memory answers for adapter tests | `Recorded`, `on`, `fixture`, `next`, `asked`, `block_on` |
| [doi.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/doi.rs) | handle resolution and destination routing | `Doi`, `locate` |
| [zenodo.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/zenodo.rs) | record API and stated MD5 values | `Record`, `resolve` |
| [figshare.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/figshare.rs) | articles and explicit versions | `Article`, `resolve` |
| [dataverse.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/dataverse.rs), [dataverse/version.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/dataverse/version.rs), [dataverse/file.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/dataverse/file.rs) | install host, published versions and individual file links | `Dataset`, `Version`, `listing`, `resolve` |
| [osf.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/osf.rs), [osf/walk.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/osf/walk.rs) | nodes, folder pages and storage-link expiry | `Node`, `Entry`, `resolve`, `files`, `expiry_check` |
| [hf.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/hf.rs), [hf/listing.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/hf/listing.rs), [hf/revision.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/hf/revision.rs), [hf/tree.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/hf/tree.rs) | repositories resolved and listed at one commit | `Repo`, `RepoKind`, `Info`, `resolve`, `revision`, `tree` |
| [kaggle.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/kaggle.rs), [kaggle/listing.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/kaggle/listing.rs), [kaggle/competition.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/kaggle/competition.rs), [kaggle/token.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/kaggle/token.rs) | dataset versions, competition data files for a signed-in account, paged files and credentials | `Dataset`, `Competition`, `resolve`, `find` |
| [drive.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/drive.rs), [drive/folder.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/drive/folder.rs), [drive/link.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/drive/link.rs) | shared files, recursive folders and ambiguous open links | `Item`, `resolve`, `list`, `opened` |
| [drive/page.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/drive/page.rs), [drive/html.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/drive/html.rs) | confirmation forms and access-error pages | `divert`, `between`, `attribute`, `text`, `unescape` |
| [dropbox.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/dropbox.rs) | shared links, including folders served as zip | `Link`, `listing`, `fetch_plan` |
| [bucket.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/bucket.rs), [bucket/s3.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/bucket/s3.rs), [bucket/gcs.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/bucket/gcs.rs) | public S3 and cloud storage prefixes, listed anonymously | `Prefix`, `Store`, `resolve`, `list` |
| [bucket/xml.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/bucket/xml.rs) | the small XML reader for S3 answers | `Element`, `parse`, `unescape` |

## how it works

### parse before resolving

`Reference::parse` recognizes identifiers and known pasted-link shapes. dataverse link parsing returns explicit errors for invalid version queries, so routing cannot discard the version or fall back to downloading the page. figshare distinguishes a modern article type segment from legacy title/id links, then reads the id and version by position rather than guessing from their values. `Parsed.file` keeps an individual file selection embedded in a link. an unrecognized page on a known source host fails instead of being mistaken for a plain downloadable file.

github and gitlab file-page URLs are normalized to raw URLs before ordinary URL resolution. [URL references](../../user/sources/urls-and-dois.md) owns accepted forms and HTML warnings.

DOIs ask the JSON handle API at `doi.org` for their registered URL. that URL's host selects an adapter. unknown hosts and another DOI are refused without fetching a landing page; DOI prefixes do not select a repository.

### catalog search

`is_search` recognizes alphanumeric input without a colon, excluding bare DOIs. `search` uses `tokio::join!` for hugging face, kaggle and zenodo, asks the `--source` catalogs, or all three, for up to `--limit` matches each, three by default, and interleaves their rank order: hugging face, kaggle, zenodo, then the next result from each. exact title or last-id matches then move to the front, keeping that order. hugging face is requested with `sort=downloads` and descending direction, zenodo with `sort=mostviewed`, and kaggle with its default catalog ordering. it computes no global score. `Candidate` carries a canonical reference and optional catalog facts. one failing catalog contributes a warning; all failing returns the first error. CLI selection is separate from source resolution.

### keep listing and transfer separate

`Listing.resolved` names the version to resolve again where the source supports one. zenodo accepts a different concrete id only when the answer names the requested concept id; an unrelated record is an invalid listing. each `Remote` carries a relative path, URL and any known size, SHA-256 or MD5. unavailable entries retain their paths and reasons so filters can explicitly leave them out. `Listing.warnings` carries what a person should know about a listing that is not an error, such as the components an OSF project leaves out; `tell_resolved` in cli/sync.rs prints each one as a warning after the `Resolved` line, so it shows without `-vv` and reaches `--json` `warnings`.

listings accept at most 100,000 files per dataset before selection; OSF's walk also counts folders. pagination is handled in the adapter and each page is parsed once. this cap does not bound total project size or the number of members inside archives.

`fetch_plan` adds download-page handlers for drive and dropbox and expired-storage-link recognition for OSF. stable source URLs stay in the lock; redirected storage URLs and credentials do not become pins. `may_wrap` names the links whose file may arrive as a zip of itself, kaggle competition files: `Remote::expected_size` then checks no listed size, and `cli::fetch::wrapped` renames a file whose bytes are a zip of only that file to `<path>.zip`, so draft and `get` unpack it through the archive path and the lock records it with `unpacks_to`.

### public buckets

`bucket::Prefix` holds a store, a bucket and a key prefix. `s3://` and `gs://` references parse directly; `Prefix::from_link` takes only HTTP and HTTPS links without a query whose path ends in `/` or names the bucket alone, so a single object link stays `Reference::Url`.

S3 listing uses ListObjectsV2 with `encoding-type=url`, so keys with control characters survive the XML. a dotted bucket name goes path-style on `s3.amazonaws.com`, since the wildcard certificate does not cover it. a 301 or 400 that names a region, through `Response.region` (the `x-amz-bucket-region` header) or the error body, moves the listing to that regional endpoint once. [bucket/xml.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/bucket/xml.rs) reads elements, text, the five named entities and character references; it refuses document types and CDATA, so nothing can expand, and stops at 32 levels of nesting. cloud storage uses the JSON API with `fields` trimmed to name, size, `md5Hash` and generation, and pins each object URL to its generation.

no credential is read for either store and none is attached to these hosts; `no_credential_is_kept_for_a_storage_host` checks `auth::credentials`. an S3 ETag becomes an MD5 only without a `-`. in debug builds, `FETCHLOOM_TEST_BUCKETS` sends every request for an `amazonaws.com` or `storage.googleapis.com` host, listings and object bytes alike, to `/<host><path>` on a local server, which [CLI bucket tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/buckets.rs) serve with `Routes`.

### add an adapter

1. declare the adapter module and a `Reference` variant in `source.rs`. implement identifier parsing and display in the adapter, then wire `Reference::parse`, `Display`, pasted links in `link` and the relevant `known_host` cases. keep `Parsed.file` for file-specific links.
2. wire `resolve_direct` and, for a DOI-capable repository, destination routing in [doi.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/doi.rs). return a checked `Listing`, preserve unavailable entries, bound pagination and pin the source's version when it offers one.
3. update `fetch_plan` only if transfers need a page diversion or expired-link handler. keep credential origins and redirects in `Net`. a source with an account adds an `Account` in [auth.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/auth.rs): where its key lives, its header and host, its token page and a check request.
4. choose existing [error kinds](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/error.rs) for invalid references, refused access, unavailable data and malformed answers. return refusals through `source::refused` or `Error::needs`, so the error names `fetchloom auth login` and a person at a terminal is offered to sign in. if a new kind is necessary, update its serialized name, exit mapping and [exit-code reference](../../user/reference/exit-codes.md).
5. add recorded responses under [tests/fixtures/sources](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/fixtures/sources) and `Recorded` tests for parsing, display, paging, pins, missing files, access failures and malformed replies. cover command or transfer effects in [CLI tests](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/cli).
6. add an ignored live test against a small public record, stating the host in its ignore reason. keep it separate from deterministic recorded coverage.
7. if the source has a public search catalog, add its query and candidate mapping in [source/search.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/search.rs), recorded ranking/error tests in [search/tests.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/search/tests.rs), and update the documented catalog order and [CLI search tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/search.rs).
8. add a page under [docs/user/sources](../../user/sources/README.md) with accepted forms, pinning, limits and access rules. link it from that index, the [user guide](../../user/README.md), the book's [SUMMARY.md](https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/SUMMARY.md) and [llms.txt](https://github.com/nikhil-yn-aix/fetchloom/blob/main/llms.txt). update [environment settings](../../user/reference/environment.md) for credentials, relevant examples and help text in [cli/args.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/args.rs) or [cli/help.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/cli/help.rs), and the affected help snapshots.

## invariants and traps

- credentials come from the context, never a `Remote` field or a serialized lock. dataverse attaches its installation account to both HTTP refusals and restricted-file metadata errors, including the port when it is not 443.
- project references reject recognized secret query keys. masking a URL is explicit through `Shown` or `shown`, not an invariant of `url::Url`.
- a reported MD5 is checked against the bytes a URL served before its mapping to SHA-256 can be reused, and only for that URL. the remembered SHA-256 must agree with any SHA-256 the source states, including when its listing omits the size.
- mutable services cannot guarantee historical listings. the lock's hashes establish byte identity even when a source changes.
- default tests use recorded responses or local servers. [ignored live tests](../engineering/ci.md#live-source-checks) check current services separately.

## tests

| files | cargo filter |
|---|---|
| [source.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source.rs), adapter module tests and [source fixtures](https://github.com/nikhil-yn-aix/fetchloom/tree/main/tests/fixtures/sources) | `cargo test --locked --lib source::` |
| [search/tests.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/search/tests.rs), [CLI search tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/search.rs) | `cargo test --locked --lib source::search::`; `cargo test --locked --test cli search::` |
| [zenodo.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/zenodo.rs) | `cargo test --locked --lib source::zenodo::` |
| [DOI tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/doi/tests.rs), [dataverse tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/dataverse/tests.rs) | `cargo test --locked --lib source::doi::`; `cargo test --locked --lib source::dataverse::` |
| [drive folder tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/drive/folder_tests.rs), [OSF walk tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/osf/walk_tests.rs) | `cargo test --locked --lib source::drive::`; `cargo test --locked --lib source::osf::` |
| [get.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/get.rs), [project.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/project.rs), [secrets.rs](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/secrets.rs) | `cargo test --locked --test cli get::`; `cargo test --locked --test cli project::`; `cargo test --locked --test cli secrets::` |
| [bucket tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/source/bucket/tests.rs), [CLI bucket tests](https://github.com/nikhil-yn-aix/fetchloom/blob/main/tests/cli/buckets.rs) | `cargo test --locked --lib source::bucket::`; `cargo test --locked --test cli buckets::` |

replace the adapter name in the module selector for figshare, hugging face, kaggle or dropbox. there is no shared adapter conformance suite.

## related

[network](network.md), [sync loop](sync-loop.md), [source behavior](../../user/sources/README.md), [data.lock](../../user/reference/data-lock.md), [fuzzing](../engineering/fuzzing.md).
