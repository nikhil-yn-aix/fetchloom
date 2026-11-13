# changelog

what changed for people who use fetchloom, newest first. each change adds its line under "unreleased"; a release turns that heading into its version and date.

## unreleased

the first release.

- `fetchloom add` pins a dataset's files by SHA-256 in `data.toml` and `data.lock`, from links and DOIs; zenodo, figshare, dataverse, dryad, mendeley data and OSF; hugging face and kaggle; ENA, GEO, DANDI and OpenNeuro; github and gitlab releases; google drive and dropbox; and S3, google cloud storage and azure blob storage.
- `fetchloom sync` puts the same bytes in `data/` on every machine, from one shared cache, by reflink, hardlink or copy, and never loses an edit you made to a placed file.
- `fetchloom get` downloads a link once, without a project; `search` finds datasets on hugging face, kaggle, zenodo, GEO and OpenNeuro, and on ENA and DANDI with `--source`.
- `fetchloom build` runs recipe steps that make data from data, and pins their results like any other.
- mirrors, offline runs, `--json` output for scripts, a python helper, shell completions and a manual page.
- installers for linux, macos and windows, wheels for python 3.10 or later, debian packages and a signed apt repository, each file listed in `SHA256SUMS` with a build attestation.
