# changelog

## 0.1.0

the first release. fetchloom puts the exact data a project needs onto any machine with one command.

- `fetchloom add` pins a dataset's files by SHA-256 in `data.toml` and `data.lock`, from a link, a DOI or a source such as zenodo, hugging face, kaggle or S3.
- `fetchloom sync` places the same bytes in `data/` on every machine from one shared cache, and never loses an edit you made to a placed file.
- `get` downloads one link without a project, and `search` finds datasets.
- installers for linux, macos and windows, python wheels, debian packages and an apt repository, each file with a checksum and a build attestation.
