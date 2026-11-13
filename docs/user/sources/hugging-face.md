# hugging face

a hugging face reference pins a repository at one commit: a branch or a tag is turned into its full commit before any file is listed.

```sh
fetchloom add hf:datasets/openai/openai_humaneval@7dce6050a7d6d172f3cc5c32aa97f52fa1a2e544
```

## references and pins

use `hf:datasets/` followed by the repository's organization and name, with `@revision` to choose a branch, tag or commit. `hf:org/name` is shorthand for a dataset. `hf:models/org/name` and `hf:spaces/org/name` use the same pinning rules for models and spaces; [search](../commands/search.md) finds datasets only. a branch or tag is resolved before file listing. a revision the hub does not show stops the run with exit 10, naming it and the repository, as in `huggingface.co has no revision v2 of dataset scikit-learn/iris, or the dataset is private`, with the repository's files page to check it on. the hub answers this way for a private repository a key went with but cannot read, too, so the message does not say the repository exists. a link to an individual commit (`/commit/<commit>`) uses that commit. a commit-history page (`/commits/<revision>`) names the repository without pinning that view.

a link without `/datasets/` or `/spaces/`, such as `https://huggingface.co/scikit-learn/iris`, names a model, as the hub's own links do. when the hub shows no such model but has a dataset of that name, fetchloom stops with `reference.unresolved`, exit 10, and says `for the dataset, use https://huggingface.co/datasets/scikit-learn/iris`.

a pasted link to a file, such as `https://huggingface.co/datasets/org/name/blob/main/data.csv`, selects that file. a link to a folder, such as `https://huggingface.co/datasets/HuggingFaceFW/fineweb/tree/main/sample/10BT`, selects everything below it, as `--select "sample/10BT/**"` would. `add` writes that pattern to `data.toml`, and an explicit `--select` replaces it. a file or folder link written by hand as a `ref` in `data.toml`, without `select`, narrows the dataset the same way.

`resolved` records the repository kind, name and full commit. download URLs use the repository's `/resolve/` route with that commit and file path. stated LFS SHA-256 is checked, and the lock records each selected file's SHA-256 and size. a dataset's title is the `pretty_name` its card gives, when that is one line of at most 100 characters; its license comes from the card's license tags. a generated dataset name still comes from the repository id, such as `multi_nli`, not from the title.

a reference that names a commit keeps it under `lock --upgrade`.

a repository may hold one large archive cut into parts, such as `train.tar.gz.aa` and `train.tar.gz.ab`. they unpack as that one archive when the dataset lists every part, so a folder link or a `--select` should take all of them; [split archives](../concepts.md#split-archives) explains the rules.

when every select pattern starts with a folder, as `sample/10BT/**` or `data/train/*.parquet` do, fetchloom lists only those folders, up to eight of them. the hub sends a thousand files a page, so a repository of a million files costs a few requests instead of a thousand, and its rate limit lasts. `Resolved` then says what it listed, as `3 files in cola`. a pattern such as `*.csv` or `**/x.csv` can match anywhere, so the whole repository is listed. a folder the hub does not have, or one without files, is listed with everything else, so a pattern that matches nothing is told against all the repository holds.

## credentials

public repositories need no account. a gated or private one needs a key whose account may read it: sign in with `fetchloom auth login hf`, set `HF_TOKEN`, or keep the token the hf command saved, in the [order environment gives](../reference/environment.md#credentials). a gated repository also needs its owner's approval, which you ask for on its page; fetchloom cannot grant it. a key the hub refuses, as an old or wrong one, is named by where it comes from, `HF_TOKEN` or the token file, never by its value.
