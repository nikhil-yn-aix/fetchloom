# hugging face

hugging face references resolve a repository revision to a full commit before listing files.

```sh
fetchloom add hf:datasets/openai/openai_humaneval@7dce6050a7d6d172f3cc5c32aa97f52fa1a2e544
```

## references and pins

use `hf:datasets/` followed by the repository's organization and name, with `@revision` to choose a branch, tag or commit. a branch or tag is resolved before file listing.

`resolved` records the repository kind, name and full commit. download URLs use the repository's `/resolve/` route with that commit and file path. stated LFS SHA-256 is checked, and the lock records each selected file's SHA-256 and size.

use quoted `--select` and `--exclude` patterns to choose listed files. patterns do not filter members inside a downloaded archive. an explicit commit still names that commit under `lock --upgrade`.

## credentials

fetchloom chooses credentials in this order, ignoring empty environment values:

1. `HF_TOKEN`, used directly.
2. the file named by `HF_TOKEN_PATH`.
3. `token` inside `HF_HOME`.
4. `huggingface/token` inside `XDG_CACHE_HOME`.
5. `~/.cache/huggingface/token`.

the token or file contents are trimmed. once a location is chosen, an unreadable file or blank token yields no credential. lower-priority locations are not tried.

tokens stay out of project files, logs and JSON. [environment](../reference/environment.md#credentials) also lists other sources' credentials.

## limits

gated data requires approval from the repository owner before a token can download it. fetchloom cannot grant that approval.

repository listings are not cached for offline resolution. prepare the project online, then keep the manifest and lock unchanged for [offline sync](../concepts.md#offline).
