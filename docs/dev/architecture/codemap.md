# code map

fetchloom is one rust crate whose files group into the layers the [module calls](README.md#module-calls) diagram draws. each row names what a file owns, the item to read first and its other main items; `Type::name` is a method.

unit tests sit beside their module, in `tests.rs`, `*_tests.rs` or another file declared under `#[cfg(test)]`; this map lists only their shared helpers, under [tests](#tests). a new file under `src/` needs a row here, or the docs test `every_source_file_is_on_the_code_map` fails; it also fails on a row whose file is gone.

## entry

[CLI](cli.md#startup) covers what happens before a command runs.

| file | owns | start at | other items |
|---|---|---|---|
| `src/main.rs` | the binary; linux musl builds also swap in mimalloc as the allocator | `main` |  |
| `src/lib.rs` | the module list, and `main`, which installs the bug-report panic hook and runs the cli | `main` |  |
| `build.rs` | the build script, which stamps the git commit `--version -v` reports into `FETCHLOOM_COMMIT` | `main` |  |

## cli

`cli::run` reads the command line and settings, then `execute` builds the runtime and hands the command to its file through `dispatch`; what that file returns becomes the `result` of `--json`. [CLI](cli.md) covers adding a command or an option.

| file | owns | start at | other items |
|---|---|---|---|
| `src/cli.rs` | startup: settings, the cache, the runtime, dispatch and the exit code; `Context` carries settings, output, counters and the stop signal to every command | `run`, `execute`, `dispatch` | `Context`, `splits`, `open_cache` |
| `src/cli/args.rs` | the clap definition of every command and option | `Args`, `Command`, `Global` | `Filter`, `read` |
| `src/cli/usage.rs` | a command line clap cannot read, answered like any failure, with the command or option meant | `answer` |  |
| `src/cli/help.rs` | the ideas behind fetchloom or one command's help, as text, and `help --json`, which describes every command in one call | `show` |  |
| `src/cli/typed.rs` | commands as a person types them: a suggested command judged with clap for `output::fix`, and this run's own line, which help repeats with a typo fixed | `judged`, `command_line` | `meant` |
| `src/cli/logs.rs` | the `-vv` and `FETCHLOOM_LOG` debug log, and the `FETCHLOOM_TRACE` timeline perfetto opens | `start` | `Logs`, `on_stderr` |
| `src/cli/project.rs` | `sync` and `lock`, and what project commands share: finding the project, taking its lock, the settled-run shortcut, dry runs and running the sync loop | `sync`, `lock`, `sync_all` | `Report`, `Outcome`, `Run`, `preview_all`, `selected`, `find`, `hold`, `known`, `register` |
| `src/cli/project/conflicts.rs` | `lock` on files a git merge left with conflict lines, merged a dataset at a time | `merge` |  |
| `src/cli/add.rs` | `add`: the dataset's name, every byte cached before data.toml or data.lock is written, and a later failure puts back what the add wrote | `add`, `sync_add` | `AddProject`, `BeforeAdd`, `restore`, `again`, `choose_name` |
| `src/cli/add/list.rs` | `add -i` and several links in one add: a dataset per line, resolved, downloaded and placed in one add | `add_list_held` | `new_datasets`, `listed_entry`, `resolve_lines`, `tell_list` |
| `src/cli/import.rs` | `add <path>`: files on this machine, found without following a link, copied into the cache and pinned by their bytes, never linked, moved or changed; then added, or supplied to an import data.toml has, through add's own sync, with a warning when git would take or keeps the folder | `import_held` | `walk`, `into_cache`, `pinned`, `supply`, `warn_in_git` |
| `src/cli/links.rs` | link lists for `add -i` and `get -i`: one link a line with an optional SHA-256, the whole list checked before any request; also several links typed for one command | `read`, `Line` | `parse`, `checked`, `typed` |
| `src/cli/remove.rs` | `remove`: a dataset out of data.toml and data.lock, and its unchanged files out of the data folder | `remove` |  |
| `src/cli/drop.rs` | `drop`: files left out for good, by patterns in data.toml, out of data.lock and, when unchanged, the data folder, asking no server | `run` | `Report`, `in_data_folder`, `asked`, `edits`, `refuse_what_breaks`, `written` |
| `src/cli/path.rs` | `path`: where a dataset or file is, syncing that dataset alone first when its files are missing | `path` | `ready_path`, `planned_path`, `hold_path` |
| `src/cli/status.rs` | `status`: how the data folder differs from data.lock, hashing only files their size and time cannot vouch for, and `unbuilt_here`, the pinned results nothing on this machine holds, with the inputs the cache lacks | `run`, `Status` | `brief`, `unbuilt_here` |
| `src/cli/status/ready.rs` | the readiness rule `status --check` and `sync --strict` share, and its `data.not_ready` error | `of_status` | `Unready`, `ready`, `after_sync` |
| `src/cli/list.rs` | `list`: each dataset's source, size and destination, from data.toml and data.lock alone, and its tables | `run` | `Report`, `Listed`, `listed`, `table`, `columns` |
| `src/cli/export.rs` | `export`: checksums of placed files or links of downloaded ones, for `sha256sum -c`, a link list or aria2c, checked and written whole | `run` | `Report`, `inventory`, `sources`, `link`, `complete`, `render`, `publish` |
| `src/cli/push.rs` | `push`: pinned files copied from the cache into a folder mirror, with the file lists of their archives | `run` | `Report` |
| `src/cli/build.rs` | `build`, with `--prepare` and `--check`: recipes selected and run in order, each step's course decided, results pinned and placed, data.lock written once after the last step | `run`, `course` | `Built` |
| `src/cli/build/preview.rs` | `build --dry-run`: each step decided by `course` against the cache as the build would find it, and the command of each that would or may run, running and fetching nothing | `preview` |  |
| `src/cli/build/room.rs` | the disk room the steps may take, said before the first runs, with a question below `build_floor` | `fits` |  |
| `src/cli/build/accept.rs` | `build --accept`: the checks a kept rebuild must pass, then its pin and its placing, running nothing | `accept` |  |
| `src/cli/get.rs` | `get`: datasets or files into new folders, once, through the cache projects share; a failed link lets the rest finish | `run` | `Report`, `many` |
| `src/cli/get/folder.rs` | the folder `get` fills, refused when it holds someone else's files, then filled by reflink, hardlink or copy | `check`, `put_all` | `Outcome` |
| `src/cli/get/file.rs` | `get -o`: one file at exactly that path, never unpacked: what to do about a file already there, a one-off moved out of the cache, and publication that replaces another file only with `--force` | `one`, `decide` | `usage`, `foreseen`, `Existing`, `Step`, `one_off`, `take`, `publish`, `forget` |
| `src/cli/get/preview.rs` | `get --dry-run`: what a get would fetch and where it would land, planned from the listings and the cache, writing nothing, not even the cache folder | `run` |  |
| `src/cli/get/remembered.rs` | what an earlier `get` kept of a link only its source can list, which stands in for the listing offline | `keep`, `listing` |  |
| `src/cli/search.rs` | `search`, its candidate rows, and the choice among candidates that `add` and `get` offer | `run`, `pick` | `Report`, `find` |
| `src/cli/cache.rs` | `cache`, `cache gc` and `cache verify`, the projects that use the cache, the weekly size reminder, and `Readers`, the objects of a project only builds read, for `fetchloom cache` and `cache gc --inputs` | `info`, `gc`, `verify` | `Info`, `Collected`, `needed_with`, `needed_for_drop`, `Readers` |
| `src/cli/cache/inputs.rs` | `cache gc --inputs`: raw data only built recipes read, freed where a folder mirror holds a copy | `gc` |  |
| `src/cli/auth.rs` | `auth`: signed-in accounts, signing in and out, and the offer to sign in after a refused download | `run`, `offer` | `Report` |
| `src/cli/settings.rs` | `settings`: each value a run would use and where it came from, the `-v` line of what a run spends, and warnings for misnamed or unread variables | `run` | `Report`, `tell_spending`, `warn_misnamed`, `warn_unread` |
| `src/cli/completions.rs` | shell completion scripts, and the hidden `completions <shell> --names=<word> -- <words before it>`, which completes dataset names from data.toml alone | `print`, `script` | `names`, `with_names` |
| `src/cli/fetch.rs` | many downloads at once, a bounded number in flight, with bars or plain lines; the first failure stops the rest and keeps what arrived | `all`, `Job` | `apart`, `listed_job` |
| `src/cli/fetch/data_folder.rs` | pinned files the data folder already holds, cloned or copied into the cache before a mirror or source is asked | `DataFolder` | `found`, `planned`, `holds`, `may_hold`, `take` |
| `src/cli/fetch/mirrors.rs` | pinned files and archive trees asked of a run's mirrors in order, each checked against its pin, and `Mirrored`, what each mirror answered this run, so none is asked twice for one file | `Mirrored` |  |
| `src/cli/fetch/again.rs` | a pinned file whose link died, looked up again through its source, since an id or DOI outlives its links | `recover` | `Again`, `Listings` |

## cli/sync

`cli/project.rs` decides which steps a command takes and `Loop` runs them. [sync loop](sync-loop.md) gives their order and shortcuts, [placing](placing.md) the placement decisions.

| file | owns | start at | other items |
|---|---|---|---|
| `src/cli/sync.rs` | the loop: resolve what data.toml changed, learn every file's hash, fetch what the new lock names, write data.lock, then place; also `tell_stale`, the warning for a recipe not built yet or changed since its build, and `Loop::ready`, which fetches what a step reads | `Loop`, `Loop::relock` | `Scope`, `Upgrade`, `Loop::relock_dry`, `Loop::preview`, `Loop::write_lock`, `Loop::place`, `pinned` |
| `src/cli/sync/draft.rs` | a dataset on its way into data.lock: selected files whose pins are still being learned, with hashes no source states filled in from earlier locks or the bytes | `Draft` | `jobs`, `learn`, `unknown_archives`, `finish`, `filter` |
| `src/cli/sync/rename.rs` | a source or recipe renamed in data.toml, which takes over the old name's pin when the match is unique, asking no source | `carry` | `Rename`, `forecast`, `results`, `pairs_by_key` |
| `src/cli/sync/disk.rs` | the survey of the data folder, read once per command: racy files hashed, aliases, hardlinks someone edited, and what a removal leaves | `Survey`, `audit` | `read_within`, `hash_racy`, `check_doubted`, `retarget`, `smudge_edited`, `retired`, `left_behind` |
| `src/cli/sync/place.rs` | the action for each destination: place, replace, remove, adopt, or leave alone a file someone changed; previews and state updates | `Placing`, `decide` | `Action`, `Reason`, `Placement`, `Preview`, `Changes`, `Seen`, `untouched`, `apply`, `settle`, `unblock`, `settle_aside`, `remove_unwanted`, `adoption`, `link_adopted`, `keep_aside` |
| `src/cli/sync/place/put.rs` | many files placed at once, their tally, and how each looked right after | `put_all`, `Tally` |  |
| `src/cli/sync/place/tell.rs` | the lines placement prints: costs, removals, and each file left alone or put back, with its reason | `tell` | `tell_preview` |
| `src/cli/sync/unpack.rs` | archives unpacked a few at a time, checked against data.lock, then dropped from the cache unless still needed | `unpack_all`, `drop_archives` | `Unpack`, `unpack_drafts`, `unpack_locked` |

## project

data.toml holds what a person asked for, data.lock what it resolved to, and `.fetchloom/state` what was placed; [project](project.md) says who may change each.

| file | owns | start at | other items |
|---|---|---|---|
| `src/project.rs` | the project folder: finding it, `Held`, the lock a command holds while it changes the project, and atomic writes | `Project`, `Held` | `Hold`, `Owner`, `find`, `hold`, `write`, `write_state`, `data_dir` |
| `src/project/manifest.rs` | data.toml: each dataset's `Origin`, a source with `ref`, `select`, `exclude` and `extract`, or a recipe with `from`, `run`, `code` and `env`; every check of those keys and of `place`; edits that keep comments, order and spacing, a new dataset in name order | `Manifest`, `Entry`, `Origin` | `Reader`, `parse`, `with_dataset`, `without_dataset`, `with_dataset_from`, `tables`, `outside` |
| `src/project/lock.rs` | data.lock: each resolved dataset (`Locked`) and the immutable byte pins of its files (`LockedFile`), their checks, and the fixed-order writer that gives the same bytes for the same inputs | `Lock`, `Locked`, `LockedFile` | `Drift`, `parse`, `render`, `spec`, `timestamp` |
| `src/project/lock/canonical.rs` | a fast reader for data.lock as fetchloom writes it; any other TOML goes to the general reader and its exact errors | `read` |  |
| `src/project/lock/archives.rs` | the archives among a dataset's pins, a split archive with its parts | `pinned` | `Archive`, `Pinned`, `archive`, `of` |
| `src/project/lock/derived.rs` | a recipe's result as data.lock pins it: inputs (`Input`), command, code (`Code`), variables, key and tree | `Derived` | `check`, `check_files`, `write` |
| `src/project/lock/lists.rs` | the compressed file lists in `data.lock.d`, never a link, for sources past 50,000 files and every recipe result, each typed by its header line | `Listed`, `read` | `Row`, `texts`, `listed`, `compress`, `is_part`, `inline`, `folder` |
| `src/project/lock/merge.rs` | data.toml and data.lock merged a dataset at a time after a git merge, with the choices a person made | `merge` | `Merged`, `Summary`, `Side`, `Choices`, `decide`, `decide_removals`, `kept`, `reads` |
| `src/project/lock/merge/markers.rs` | each side, and the base, read back from git's conflict lines without the tables a conflict cuts, beside the sides whole from git | `split` | `Sides`, `first` |
| `src/project/state.rs` | `.fetchloom/state`, one JSON document with an index of its records: each placed file's size and time (`Look`), the last sync's inputs (`Synced`), and each rebuild of a recipe that differs from its pin (`State.candidates`) | `State`, `Placed` | `nanos`, `render`, `read_within`, `read_whole` |
| `src/project/git.rs` | git found and run as a bounded reader with no shell: each side of an unfinished merge whole, with the base, from its index or the commits being merged, and the files git tracks | `stages`, `known` | `find`, `Stages`, `committed`, `tracked`, `Known` |

## recipe and build

a recipe makes a dataset by running a command over other datasets, and only `fetchloom build` runs one. [recipes](recipes.md) covers the key, stale pins and each step.

| file | owns | start at | other items |
|---|---|---|---|
| `src/recipe.rs` | a `run` command parsed and expanded (`Template`), the order of steps, a recipe as it stands with its key over everything its result depends on (`Step`), and what moved since its pin (`Change`), which tells a stale pin | `Template`, `key`, `drift` | `Step`, `Change`, `order`, `closure`, `content` |
| `src/recipe/code.rs` | a recipe's `code` hashed alike on every system: CRLF read as LF, a folder as its tree, links refused | `hash` |  |
| `src/build.rs` | the folder in the cache a step runs in, reading `in/<name>` and writing `out` | `Staging` |  |
| `src/build/step.rs` | one step run without a shell in its own process group, its output in a private log with its tail kept, and the whole group ended on a stop | `run` |  |
| `src/build/step/program.rs` | a step's program found on windows as a shell there finds it, so `npm` reaches `npm.cmd` | `find` |  |
| `src/build/inputs.rs` | each input's private copy, made from the cache as a read-only reflink or copy, never a hardlink | `make` |  |
| `src/build/capture.rs` | what a step wrote, taken into the cache without following a link: the walk, the names, the hashes and the publication | `read`, `publish` |  |
| `src/build/compare.rs` | a rerun's files compared with the result data.lock pins: matching, changed, missing and extra | `compare` |  |

## source

most adapters have a type with `parse` for an id, `from_link` for a pasted link and `resolve`, which returns its `Listing`. [sources](sources.md) shows how to add one.

| file | owns | start at | other items |
|---|---|---|---|
| `src/source.rs` | `Reference`, a source's identity, routed from what a person typed; `Listing`, what it resolves to, of `Remote` files to download; the checks every listing passes, the guard paged listings share, and per-source transfer rules | `Reference::parse`, `Reference::resolve` | `Parsed`, `Access`, `Unavailable`, `Pages`, `Found`, `byline`, `fetch_plan`, `may_wrap`, `refused`, `paced_as`, `spacing` |
| `src/source/search.rs` | `search` across public catalogs, its candidates interleaved, and the public dataset of the same owner one typo from a hugging face or kaggle id the source would not show | `search`, `near` | `Candidate`, `Search`, `is_search`, `Near` |
| `src/source/local.rs` | text that names something on this machine, never searched for or sent; the ref of imported files | `is_path`, `IMPORTED` | `reads_as_path`, `here`, `refused`, `is_imported`, `unlisted`, `changed`, `unpinned`, `no_newer`, `imported_elsewhere` |
| `src/source/doi.rs` | DOIs, routed by the host doi.org names, never by their prefix | `Doi::locate` |  |
| `src/source/xml.rs` | a small XML reader for the answers of S3, azure and arXiv, without attributes, that refuses document type declarations | `parse` | `Element`, `unescape` |
| `src/source/zenodo.rs` | zenodo records, with the MD5 zenodo states for every file | `Record` | `Record::resolve` |
| `src/source/figshare.rs` | figshare articles, optionally at a version | `Article` | `Article::resolve` |
| `src/source/dataverse.rs` | datasets on any dataverse install, by host and persistent id | `Dataset` | `Dataset::resolve` |
| `src/source/dataverse/version.rs` | published versions and the original files their checksums describe | `Version` | `Answer::listing`, `Version::listing` |
| `src/source/dataverse/file.rs` | one file named by its page link, at a published version | `resolve` |  |
| `src/source/dryad.rs` | dryad datasets at one version, listed page by page with the MD5 or SHA-256 dryad states; anyone may list them, but a download needs an API account | `Dataset` | `Dataset::resolve`, `refused_download` |
| `src/source/mendeley.rs` | mendeley data at one published version, through the public API its own site reads: folder paths, files read in parts up to the count `Response.total` gives, and the 200 answer that holds an error; no key goes to mendeley | `Dataset` | `Dataset::resolve` |
| `src/source/osf.rs` | OSF projects and components (nodes) by their short id, and the expiry of their storage links | `Node` | `Node::resolve`, `expiry_check` |
| `src/source/osf/walk.rs` | OSF storage walked folder by folder, with the versions that fix each file's bytes | `files` | `Entry` |
| `src/source/hf.rs` | hugging face dataset, model and space repositories, downloaded through `/resolve/<commit>/` | `Repo` | `RepoKind` |
| `src/source/hf/listing.rs` | a repository listed at one commit, its access checked when it is gated | `Repo::resolve` |  |
| `src/source/hf/revision.rs` | what a revision points at: its commit, whether the repository is gated, and its tags | `Repo::revision` | `Info` |
| `src/source/hf/tree.rs` | the files at one commit, read from the tree API page by page | `Repo::tree` |  |
| `src/source/kaggle.rs` | kaggle datasets by owner and name, and competitions by name, downloaded one file at a time | `Dataset`, `parse` |  |
| `src/source/kaggle/listing.rs` | a dataset at one version, its files read page by page | `Dataset::resolve` |  |
| `src/source/kaggle/competition.rs` | competition files, for a signed-in account that accepted the rules | `Competition` | `Competition::resolve` |
| `src/source/kaggle/token.rs` | kaggle's credentials, where the kaggle CLI keeps them | `find` |  |
| `src/source/ena.rs` | ENA runs by run, experiment, sample or study accession; one kind of file per reference, FASTQ unless it names submitted or SRA files; the count asked before the report, which has no pages; runs under controlled access left out | `Accession` | `Files`, `Accession::resolve` |
| `src/source/geo.rs` | GEO series, samples and platforms from the folder indexes of NCBI's file server, read to at most 64 folders, with no stated hash or exact size; the spacing NCBI's hosts ask for, which `source::spacing` gives the network | `Accession` | `Accession::resolve`, `spacing` |
| `src/source/arxiv.rs` | arXiv papers by id, version and pasted link, months and categories' months; the mirror lookup; the pace arXiv's hosts share | `parse`, `Paper` | `Id`, `search`, `paced_as`, `spacing`, `most_at_once` |
| `src/source/arxiv/paper.rs` | one paper at its newest version or the one named: its title, license and withdrawal, and its pdf from the mirror or export.arxiv.org | `Paper::resolve` |  |
| `src/source/arxiv/month.rs` | a month from the mirror's folder, and a category's month paged from the API, joined with the mirror, withdrawals left out | `Month` | `Month::resolve` |
| `src/source/arxiv/feed.rs` | arXiv's answers: the API's atom feed and the OAI-PMH record of a paper | `ask`, `record` | `Entry`, `Page`, `Record`, `search` |
| `src/source/s2.rs` | semantic scholar papers by any id its API takes; its search; the pace of its API, with a key or without; refusals naming the key or the account | `parse`, `search`, `spacing`, `most_at_once` | `answer`, `busy` |
| `src/source/s2/dataset.rs` | a release's dataset: its name, description and license from the release, its files from the links the API gives a key, each kept by its name; a stable link's fresh signed link; S3's word that a link expired | `Dataset`, `Relink` | `expired`, `expired_answer` |
| `src/source/s2/paper.rs` | one paper's open-access pdf, its license and DOI; an arXiv paper through the arXiv source; a paper without a pdf refused with its page | `Paper` | `Paper::resolve`, `license`, `Answer` |
| `src/source/dandi.rs` | DANDI dandisets at a published version or the draft, with the SHA-256 DANDI states | `Dandiset` |  |
| `src/source/openneuro.rs` | OpenNeuro snapshots through its GraphQL API, large files from its S3 bucket | `Dataset` |  |
| `src/source/github.rs` | github release files at a tag or the newest, with the SHA-256 github states; a rate limit worded by whether a key went, and a missing release told from a private repository | `Release` | `Release::resolve`, `limited`, `hidden` |
| `src/source/gitlab.rs` | gitlab.com release links at a tag or the newest, each downloaded from its own address, since gitlab.com answers a program with a warning page for a link to another host | `Release` | `Release::resolve`, `hidden` |
| `src/source/drive.rs` | google drive links: files from drive's download host, items shared with an account through its api | `Item`, `fetch_plan` | `Item::resolve` |
| `src/source/drive/link.rs` | an open link, found to be a file or a folder by drive's redirect | `Item::opened` |  |
| `src/source/drive/folder.rs` | shared folders, read from the page drive embeds them in, one request per subfolder | `list` |  |
| `src/source/drive/page.rs` | the pages drive sends instead of a file: the virus-scan confirmation and its refusals | `divert` |  |
| `src/source/drive/html.rs` | the little HTML reading drive's pages need | `between`, `text` | `attribute`, `unescape` |
| `src/source/drive/private.rs` | items shared with an account, read through drive's api once a page refused them | `refused`, `list` | `opened`, `search`, `about`, `file` |
| `src/source/dropbox.rs` | dropbox shared links, fetched with `dl=1`; a shared folder arrives as one zip | `Link` | `Link::listing`, `fetch_plan` |
| `src/source/bucket.rs` | S3, S3-compatible, google cloud storage and azure buckets and prefixes, unsigned until a listed bucket refuses; refusal messages and version links | `Bucket`, `Prefix` | `Store`, `Bucket::resolve`, `with_version`, `unversioned` |
| `src/source/bucket/s3.rs` | S3's `ListObjectsV2`, following a bucket to its region | `list` |  |
| `src/source/bucket/gcs.rs` | google cloud storage's JSON object list | `list` |  |
| `src/source/bucket/azure.rs` | azure's `List Blobs`, with a SAS only for a container `azure_containers` lists | `list` |  |
| `src/source/bucket/signing.rs` | which S3 requests go signed, with which keys and region | `Signing` | `Located`, `Signer`, `Keyed` |

## auth

a key lives where the source's own tools keep it, so one key serves both. `net` decides which requests carry one, as [network](network.md#trust-boundaries) describes.

| file | owns | start at | other items |
|---|---|---|---|
| `src/auth.rs` | accounts at the sources: which key each takes, where it is kept, how to check it and how to get one, and how a message names a key, by where it comes from | `Account`, `credentials` | `Origin::named` |
| `src/auth/store.rs` | keys for sources whose tools keep no file: the system's credential store, or a private file the person agreed to | `read`, `save` |  |
| `src/auth/netrc.rs` | `.netrc`, which curl, wget and python's requests also read | `read`, `save` |  |
| `src/auth/aws.rs` | AWS keys from the environment, the shared profile files or the program a profile names, bound to an endpoint: an S3-compatible one gets them only where the AWS settings say | `find` | `Identity`, `Source` |
| `src/auth/aws/program.rs` | a profile's `credential_process`, or the AWS CLI's `configure export-credentials`, run by full path without a shell and read as version 1 JSON | `Program` | `Issued`, `words`, `installed` |
| `src/auth/azure.rs` | azure storage's SAS and account key, where the az cli reads them, kept to one account, or the sign-in `AZURE_TOKEN_CREDENTIALS` chooses: the az cli's token or this machine's managed identity; what `fetchloom auth` lists of them | `find` | `Storage`, `AccountKey`, `SignIn`, `Token`, `cli_token`, `describe` |
| `src/auth/google.rs` | google's application default credentials, where google's own libraries look, the grant a token request sends, and the token in the answer | `find` | `Credentials`, `Token`, `grant`, `answer` |
| `src/auth/process.rs` | a program that prints a credential: no shell, stdout read up to 64 KiB, stderr never | `printed` | `Ran`, `said` |
| `src/auth/lease.rs` | a credential that expires, such as a google token or a program's AWS keys, held until shortly before it does, or for the run when it never does, with one fetch for every caller that needs a new one at once | `Lease` | `get`, `forget` |

## net

[network](network.md) explains the download pipeline, retries, pacing and which requests carry a credential.

| file | owns | start at | other items |
|---|---|---|---|
| `src/net.rs` | the run's network: lazy clients, redirect policy, concurrency, and which credential a request carries | `Net`, `Options` | `Wire`, `Credential`, `Offline`, `Splitting`, `Split`, `send_on`, `permit`, `pace_holding` |
| `src/net/api.rs` | small answers read whole and bounded, such as listings and metadata; `Api::get_once`, one try with no retry and no wait for a host's rate limit, for a hint a person can do without; `Api::get_with`, a GET with extra headers | `Api`, `Response` | `json`, `text` |
| `src/net/api/memo.rs` | bounded reuse of metadata answers within a run, and one request shared by callers that ask at once | `Memo` | `Key`, `Flight`, `Claim` |
| `src/net/cookies.rs` | the one cookie jar every client of a run sends from, its generation keeping memoized answers apart | `Cookies` |  |
| `src/net/download.rs` | one file into the cache: a cache hit, else a resumed or fresh download, retried, every byte hashed on its way to disk and checked against what was expected; `Fetch` asks, `Fetched` is the checked result | `download`, `Fetch` | `Expect`, `Fetched`, `Origin`, `Event`, `Resumable` |
| `src/net/download/split.rs` | one large file over several ranged streams, a stream added only while it makes the whole faster | `split` | `Job`, `Board`, `Share`, `Streamed` |
| `src/net/headers.rs` | the headers downloads read: lengths, ranges, validators and `Content-Disposition` file names | `file_name`, `validators` | `if_range`, `changed`, `range_start`, `range_end` |
| `src/net/failure.rs` | which failures get another try, and how long to wait | `classify`, `Failure` | `Retry`, `retry_after` |
| `src/net/pace.rs` | how often a host lets fetchloom ask, from its rate-limit headers or a spacing such as NCBI's, longer after it refuses for load, and which waits get a line | `Paces` | `Limit`, `Hold`, `Ticket`, `admit`, `heard`, `answered`, `pause`, `busy`, `told` |
| `src/net/rate.rs` | the one byte budget every response in a run shares | `Rate` | `read`, `throttle` |
| `src/net/relink.rs` | links a source gives only for a while: a fresh one by the stable link data.lock keeps, asked again once when it expired, and during a download only while it holds no slot | `Net::relinked`, `Net::refreshed` |  |
| `src/net/address.rs` | where a request may go: private addresses only for origins a person typed, named as a mirror or listed in `allowed_origins`; DNS; and proxies with the variables that name them | `Reach`, `Proxies` | `Typed`, `Resolver`, `is_private`, `named` |
| `src/net/address/reach.rs` | whether the proxy carrying a request can be reached, so a dead proxy is named within seconds | `Proxies::carrier` | `Carrier`, `unreachable`, `watch`, `reached` |
| `src/net/shown.rs` | which query names hold secrets, and links as messages and logs show them, every query value masked unless its name is known to be harmless | `Shown`, `mask_query` | `secret`, `depends_on_path` |
| `src/net/sign.rs` | AWS signature version 4: the canonical request and the authorization header, on `hmac` and `sha2`, so the secret itself is never sent | `authorization` | `Keys`, `Scope`, `Request`, `canonical`, `amz_date` |
| `src/net/google.rs` | which requests carry a google token: listed buckets once refused, and the drive requests `drive` lets through; and the token request | `Google` | `Located`, `Bearer`, `Authorized`, `google_token`, `post_grant` |
| `src/net/google/drive.rs` | which drive api requests carry a token: items `drive_folders` lists and what google lists inside them | `Drive` |  |
| `src/net/azure.rs` | which azure blob requests carry a SAS, a key's signature or a token, never after a redirect | `Azure` | `Located`, `Sent`, `Blobbed` |
| `src/net/azure/shared_key.rs` | azure's shared key signature; the key is never sent | `authorization` |  |
| `src/net/azure/identity.rs` | the managed identity's token from 169.254.169.254, through a client of its own | `Net::identity_token` |  |
| `src/net/roots.rs` | the one TLS configuration every client of a run shares on linux: the system's roots, read once, else the mozilla roots fetchloom carries, and an error when `SSL_CERT_FILE` or `SSL_CERT_DIR` names unreadable ones | `tls` |  |

## mirror

a run asks its mirrors before any source and checks every file one gives against its pin; [cache](cache.md#copies-from-mirrors) covers the copies.

| file | owns | start at | other items |
|---|---|---|---|
| `src/mirror.rs` | the layout folder and web mirrors share; `Mirrors`, the mirrors a run asks in order and whether it may ask nothing else; `Wanted`, a pinned file asked of them; nothing a mirror gives is trusted | `Mirrors`, `Location` | `Wanted` |
| `src/mirror/folder.rs` | a mirror on a disk this machine reads, its files copied and never linked; `Target` writes one for `push`, checking each file before it takes its name, and `There` is what a mirror holds already | `probe`, `Target` | `Item` |
| `src/mirror/web.rs` | a mirror on a web server: files through the download pipeline, marker and trees read whole up to 64 MiB | `probe`, `object` |  |

## cache

most files here add methods to `Cache`. [cache](cache.md) gives the layout and when an object may be trusted.

| file | owns | start at | other items |
|---|---|---|---|
| `src/cache.rs` | the root layout and object lookup: one read-only file per object named by its SHA-256, plus URL and MD5 records and files imported from a person's folder | `Cache`, `Cache::find` | `Stored`, `Imported`, `Validators`, `UrlRecord`, `verify`, `write_hint`, `import_file`, `link_object` |
| `src/cache/partial.rs` | unfinished downloads: the bytes so far, their record and a lock keeping each to one process; never opened through a link | `Cache::claim`, `PartialWriter` | `Claim`, `Partial`, `read_unfollowed`, `open_unfollowed` |
| `src/cache/partial/hashing.rs` | ordered prefix hashing, inline or on another thread behind a bounded queue, so disk writes go on while earlier bytes are hashed | `HashPipe` | `update`, `range`, `sha256_so_far`, `finish` |
| `src/cache/partial/hashing/lanes.rs` | the optional MD5 lane, which shares a batch's bytes | `Lane` |  |
| `src/cache/deferred.rs` | finished downloads that take their names only after a flush covers them, one flush per group on linux | `Cache::defer_downloads`, `Finished` | `Cache::publish_downloads`, `Cache::publish_due`, `Cache::publish_download_of` |
| `src/cache/object.rs` | objects written in one go, as archive members or mirror copies: sealed read-only in `tmp/`, hashed and flushed before publishing, in batches; clones or copies of files other programs keep | `NewObject`, `Batch` | `Sealed`, `Copied`, `batch_in`, `clone_from`, `copy_from`, `seal`, `publish`, `copy_in`, `holds_pinned`, `syncfs_reports_errors` |
| `src/cache/held.rs` | the cache-wide lock: runs hold it shared, `cache gc` exclusive | `Cache::hold` | `Access` |
| `src/cache/staging.rs` | archive staging folders and recipe step folders, whose locks tell a live writer from a killed run | `Cache::staging`, `Staging` | `Cache::build_folder`, `Cache::sweep_staging`, `clear_tree` |
| `src/cache/unpacked.rs` | what each archive unpacked to, its member list and stable tree identity, keyed by its SHA-256 or a split archive's parts | `Cache::record_unpacked`, `tree_hash` | `Member`, `parts_key`, `Cache::record_tree_bytes`, `Cache::unpacked`, `Cache::holds_unpacked` |
| `src/cache/results.rs` | recipe results under `builds/<key>/<tree>`, their key locks, and captured files moved into `objects/` | `Cache::record_result`, `Cache::hold_key` | `Built`, `KeyHold`, `Seen`, `Cache::results`, `seal_captured`, `Cache::publish_captured` |
| `src/cache/changed.rs` | objects found changed, remembered after repair so projects can check their old hardlinks | `Cache::mark_changed`, `Doubt` | `Cache::trusted`, `Cache::mark_replaced`, `Cache::changed` |
| `src/cache/upkeep.rs` | the projects that use the cache, scans, verification and gc, and how each project uses the objects it needs | `Cache::register`, `Needed` | `Use`, `Supply`, `Freed`, `Checked`, `Cache::scan_objects`, `Cache::sweep_with_stop`, `sweep_results`, `Cache::verify_scanned`, `Cache::free_input` |
| `src/cache/urls.rs` | what each link served last: records appended to 16 shards with an index of where each starts, found by a run's first read of a shard's index, and written again by gc | `Shards::find`, `Shards::save` | `Shards::rewrite`, `SHARDS` |
| `src/cache/usage.rs` | where the cache's space goes, for `fetchloom cache` and the weekly reminder alike | `Usage::measure` | `Usage` |
| `src/cache/failure.rs` | the filesystem failures inspection and cleanup keep to report | `Failure` |  |

## archive

[archives](archives.md) gives the checks every member passes before it is published.

| file | owns | start at | other items |
|---|---|---|---|
| `src/archive.rs` | archives unpacked into the cache member by member, refused whole if anything is unsafe: which files are archives (`Packed`), documents that are zips inside, as `.xlsx` and `.npz`, kept whole; the expansion `Limits`; the member tree (`Unpacked`); and the next step for an archive left packed | `unpack`, `Limits` | `Unpacked`, `Skipped`, `System`, `by_name`, `unsupported`, `stem`, `unpack_cached`, `unpack_cached_parts`, `unpack_elsewhere`, `unpack_inner`, `join_elsewhere` |
| `src/archive/detect.rs` | an archive's format from its first bytes, whatever its name says; a rar is told apart as unsupported | `detect` | `Format`, `Detected` |
| `src/archive/input.rs` | an archive's bytes as every reader takes them: one file, or a split archive's parts read in order with one file open; buffered, seekable and metered for zip and 7z | `Input` | `Joined`, `Source` |
| `src/archive/parts.rs` | split archives, rar volumes and spanned zips, found by their names; only split archives are joined, the others stay packed | `split` | `Split`, `Volumes`, `whole` |
| `src/archive/guard.rs` | one guard per archive: every name checked, entries counted, every byte written metered against the limits, and cancellation | `Guard` | `Meter`, `entry`, `copy`, `check_stopped` |
| `src/archive/names.rs` | member names made portable, checked, and compared as disks compare them to catch collisions | `Names` | `Named`, `add` |
| `src/archive/zip.rs` | zip through its central directory, every entry checked before the first byte is written | `unpack` |  |
| `src/archive/zip/directory.rs` | bounds on central-directory work before the decoder reads it | `check` |  |
| `src/archive/tar.rs` | tar, plain or compressed, read in one pass with its decompressors | `unpack` | `decompressed` |
| `src/archive/sevenz.rs` | 7z: header limits, encryption and link checks, and decoding a block at a time with the memory the limits allow and no password | `unpack` |  |
| `src/archive/store.rs` | writer threads storing members while one thread reads: queues, staging, and publication only once the whole archive passed | `store_all`, `Writers` | `keep`, `publish_all` |
| `src/archive/store/stream.rs` | bounded reusable buffers between a large member's decoder and its writer | `channel` | `Sending`, `Receiving` |
| `src/archive/shards.rs` | numbered tar shards that webdataset loaders read packed, and the hint to keep them so | `shards` | `Shards`, `hint` |

## plan, place and volume

`plan` decides where each pinned file goes and `place` does the filesystem work; [placing](placing.md) explains each action.

| file | owns | start at | other items |
|---|---|---|---|
| `src/plan.rs` | where every pinned file lands: destinations expanded, clashes found before anything is written, names compared as a disk that ignores case does | `Plan`, `Target` | `Patterns`, `folder`, `from_cache`, `inputs`, `foresee`, `expand`, `unpacked_paths`, `check` |
| `src/place.rs` | the filesystem work that fills destinations: reflink, else hardlink, else copy, and copies made durable | `Placer`, `Method` | `Order`, `Unlinked`, `place_all`, `relink`, `copy_file`, `move_aside`, `move_back`, `batches_copies`, `flush_copies`, `run_all`, `run_coarse`, `run_hashes` |
| `src/place/parents.rs` | on unix, directory handles that keep later path swaps out of placement | `Parents` |  |
| `src/place/parents_windows.rs` | the windows twin: folder handles that deny deletion while placement uses their paths | `Parents` |  |
| `src/place/schedule.rs` | a cap on concurrent changes to one folder's entries, apart from the worker ceiling | `Folders` |  |
| `src/place/synced.rs` | folders onedrive, dropbox, icloud drive or google drive keep in sync, where files are copied rather than linked | `client` |  |
| `src/volume.rs` | what a volume allows: hardlinks, which FAT32 and exFAT lack, and the safe removal of read-only files, which they refuse on windows | `Volume` | `replace`, `remove`, `remove_hardlink`, `can_hardlink`, `is_plain`, `create_hidden` |

## output

[output](output.md) covers which stream carries what, the palette and terminal fallbacks.

| file | owns | start at | other items |
|---|---|---|---|
| `src/output.rs` | everything fetchloom prints: the answer on stdout; the story, prompts and errors on stderr; the JSON document and the buffers it is encoded into | `Output` | `Level`, `Document`, `Problem`, `Pieces`, `step`, `done`, `warn`, `answer`, `report`, `document`, `verb_line` |
| `src/output/style.rs` | what the terminal can show, decided once at start: color, unicode, links, and `FETCHLOOM_COLORS` | `Style`, `Palette` | `ColorChoice`, `detect`, `stdout_color` |
| `src/output/progress.rs` | progress while work runs: byte bars, waits, the steps before a command's work and item counts, as bars on a terminal a person watches and plain lines now and then anywhere else | `Bar`, `Ticker` | `Wait`, `Setup`, `start`, `advance`, `sized` |
| `src/output/picker.rs` | the arrow-key dataset choice and its cancellation, and the numbered choice of accessible output | `choose` | `numbered`, `selection_key`, `row` |
| `src/output/fit.rs` | lines wrapped to the terminal, and help examples continued as a shell reads them | `fit` | `example`, `MARK` |
| `src/output/raw.rs` | a question's hold on its terminal: raw from before the prompt until the answer, so no typed key is echoed, and put back on every way out | `question` | `release` |
| `src/output/warning.rs` | each warning's `kind`, which `--json` names so a script can act without reading words, held to the JSON reference by a test | `Warning` | `Warning::name` |

## primitives

every layer shares these small types and limits; [primitives](primitives.md) gives their rules.

| file | owns | start at | other items |
|---|---|---|---|
| `src/digest.rs` | typed digests: SHA-256, the identity of a file, and MD5, which is checked but never trusted as one; incremental hashing | `Sha256`, `Hasher` | `Md5`, `from_hex`, `of_reader`, `sha256` |
| `src/name.rs` | names windows and linux can both store: suggested names, path checks, dataset names, typo suggestions, alias folding, and `folder/name` as listings write it | `sanitize`, `fold` | `dataset_name`, `title_name`, `reference_name`, `escapes`, `metadata`, `unstorable`, `closest`, `joined` |
| `src/glob.rs` | `select` and `exclude` patterns, matched against a whole path inside a dataset | `Glob` | `Glob::new`, `matches`, `is_top_level_only` |
| `src/units.rs` | one way to write sizes, rates, durations, counts, lists and paths everywhere: lists of names cut to the first few, and sizes typed as `512M` read back | `size`, `duration` | `rate`, `speed`, `count`, `clip`, `grouped`, `listed`, `some_of`, `bytes` |
| `src/settings.rs` | every setting, read once at start with where it came from: variable, settings file, `resources` level, machine or default | `Settings` | `File`, `Set`, `Origin`, `read`, `defaults`, `hash_batches` |
| `src/settings/resources.rs` | `resources` levels: what lean, default and max give the knobs they derive from the machine, and what a run spends once every knob is set, which `-v` says | `Level`, `Spending` | `Limits`, `limits`, `spending` |
| `src/machine.rs` | what the machine allows: processors, available memory on linux inside cgroup limits and whether a cgroup holds it, open files, the processors online, and programs on PATH | `Machine` | `read`, `processors_online` |
| `src/space.rs` | free and total space on the disk holding a folder, statvfs on unix and the volume list on windows, checked before a download starts; what that disk is formatted as, and the 4 GiB less one byte FAT32 holds in one file; the room a build needs | `free`, `Needs` | `short`, `Filesystem`, `cache_holds`, `peak`, `floor` |
| `src/threads.rs` | the optional ceiling on local worker groups, and the scoped worker helper | `Threads`, `scoped` | `read`, `limit`, `archives` |
| `src/error.rs` | failures with stable kinds, exit codes and advice, for people and scripts: details cut to the first few, a command's report so far, and the help for an answer only a newer fetchloom can read | `Error`, `Kind` | `FileFailure`, `exit_code`, `with_result`, `name`, `advice`, `details_up_to`, `with_report`, `report_answer`, `ISSUES` |
| `src/stop.rs` | cancellation every task shares, as an `Error` or, for work on files, an `io::Error`; the first ctrl-c asks every task to wind down and keep what it has | `Stop` | `ask`, `asked`, `is_asked`, `check`, `check_io` |
| `src/suggested.rs` | the commands hints name, carrying the folder a `-C` run worked in, and words quoted for a shell, read back as a shell reads them | `command`, `shell_word` | `set_directory`, `command_words` |
| `src/work.rs` | atomic counters of the work a run did, apart from output, which tests assert, `-v` prints and `--json` carries | `Work`, `Counts` | `Work::add`, `Work::counts` |

## tests

the cli tests run the real binary in a sandbox; [building and testing](../engineering/building-and-testing.md) says which to run.

| file | owns | start at | other items |
|---|---|---|---|
| `tests/cli/main.rs` | the cli test binary: one module per scenario file in `tests/cli/` |  |  |
| `tests/cli/support.rs` | the sandbox every cli test runs in: a clean folder, a private cache and home, and a server that answers with scripted bytes | `Sandbox`, `Server` |  |
| `tests/cli/support/command.rs` | a command with a deadline, whose failure shows its stderr and the end of its debug log | `Fetchloom` |  |
| `tests/cli/support/running.rs` | a command running beside its test, its stderr read as it comes | `Running` |  |
| `tests/cli/support/server.rs` | listeners and connections, stopped and joined with their test server | `ServerTask` |  |
| `tests/cli/support/programs.rs` | the fetchloom binary and the `recipe_step` example the tests run | `executable`, `recipe_step` |  |
| `tests/support/links.rs` | a folder that links to another, a junction on windows, shared by unit and cli tests | `link_folder` |  |
| `tests/support/paths.rs` | the workspace folder, shared by unit and cli tests | `workspace` |  |
| `tests/support/ports.rs` | an address where every connection is refused, shared by unit and cli tests | `Refusing` |  |
| `tests/support/skips.rs` | a skip that fails instead where `FETCHLOOM_TEST_REQUIRE` names the missing capability | `skip` |  |
| `examples/recipe_step.rs` | the step program cli tests put in recipes | `main` |  |
| `src/net/scripted.rs` | a local server that answers each connection with scripted bytes, for network unit tests | `serve` |  |
| `src/source/recorded.rs` | answers recorded from real services, replayed in source unit tests, and the real network the `live_` tests reach | `Recorded` | `on`, `fixture`, `next`, `total`, `asked`, `block_on`, `live` |
| `src/archive/samples.rs` | archives built at test time, and a scratch cache to unpack them into | `Scratch` |  |
| `src/output/screen.rs` | a terminal that records what progress bars draw, for tests that wait on a drawn line | `Screen` | `records`, `text`, `shows` |
