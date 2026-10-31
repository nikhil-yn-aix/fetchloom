# vision

a research result can depend on data that changed after the code was published. a list of download links does not record which bytes the analysis used.

when you share research code, collaborators need the same published inputs without repeating browser downloads and folder renames. [project locks](../formats.md#datalock) keep the input identities with the code; the cache can be replaced without changing those identities.

a lock cannot restore bytes that every host and cache has lost. long-term preservation still needs someone to retain the data. [scope](../scope.md#outside-the-project) sets the boundaries of retrieval, and [plan](plan.md) tracks pending release work.

## existing tools

checks ran on 2026-10-29 on windows 11 with NTFS, without admin rights or symlink privileges. local HTTP tests used forced stops and altered responses; other backends may behave differently.

| tool | version tested | hosts and inputs | pinning and download checks | shared cache and disk reuse | resume | offline rerun |
|---|---|---|---|---|---|---|
| [gdown](https://github.com/wkentaro/gdown) | 6.5.1 | drive files and folders; HTTP URLs | python `cached_download(hash=...)` rejects mismatches | API returns a shared path; separate outputs duplicate bytes | `download(resume=True)` resumed HTTP | cached API path |
| [zenodo_get](https://github.com/dvolgyes/zenodo_get) | 3.1.0 | zenodo records | record id; checks MD5; `-m` exports sums without downloading | separate output copies | restarted partial file | needs record metadata |
| [huggingface_hub](https://huggingface.co/docs/huggingface_hub/guides/manage-cache) | 2.3.0 | hugging face hub | commit pin; HTTP test accepted altered bytes | shared snapshot path; `local_dir` copies | HTTP test restarted after a forced stop | cached revision |
| [kagglehub](https://github.com/Kaggle/kagglehub) | 1.0.3 | kaggle datasets | version pin; checks server MD5 when supplied | shared version path; `output_dir` copies | transfer resumed HTTP | pinned cached version; latest needs the API |
| [pooch](https://www.fatiando.org/pooch/latest/protocols.html) | 1.9.0 | URLs and DOI repositories | registry or `known_hash`; rejects mismatches | shared directory chosen by the caller | restarted HTTP | cached file with its hash |
| [datahugger](https://github.com/J535D165/datahugger) | 0.13 | DOI repositories, including zenodo, figshare and OSF | metadata files; `--checksum` reports a mismatch but exits 0 | separate output copies | transfer rerun skipped partial file | needs source metadata |
| [dvc](https://doc.dvc.org/command-reference/import-url) | 3.67.1 | file URLs; local and generated data | MD5 pins; [remote verification is configurable](https://doc.dvc.org/command-reference/remote/modify#available-parameters-for-all-remotes) | shared cache and hardlinks after configuration | `get-url` restarted HTTP | `checkout` from cache |
| [datalad](https://handbook.datalad.org/en/latest/intro/installation.html) | 1.7.2 | git/annex datasets and file URLs | annex key checked on `get`; iris used MD5E | per-repository cache; unlocked copies here | public rerun completed; byte range unverified | `get` with local content |
