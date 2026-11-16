# github and gitlab releases

release references pin the files a release of a github repository or a gitlab project holds, on gitlab.com or another gitlab server. a tag can be moved to another commit and a release's files replaced, so what names the bytes is the SHA-256 the lock records for each file, not the tag.

```sh
fetchloom add github:jqlang/jq@jq-1.8.2 --select "jq-linux-amd64"
fetchloom add https://github.com/jqlang/jq/releases/tag/jq-1.8.2 --as jq --select "*.tar.gz"
fetchloom get gitlab:gitlab-org/gitlab-runner@v19.3.6 --select "release.sha256"
```

## references

| spelling | behavior |
|---|---|
| `github:jqlang/jq@jq-1.8.2` | list the release at that tag |
| `github:jqlang/jq` | list the release github calls the latest: the newest that is neither a draft nor a prerelease |
| `https://github.com/jqlang/jq/releases/tag/jq-1.8.2`, or `/releases/latest` | accept a release's page, or the latest release's |
| `gitlab:gitlab-org/gitlab-runner@v19.3.6` | list the release at that tag; a project in nested groups keeps every group, as `gitlab:group/subgroup/project@v1` |
| `gitlab:gitlab-org/gitlab-runner` | list the newest release |
| `https://gitlab.com/gitlab-org/gitlab-runner/-/releases/v19.3.6`, or `/-/releases/permalink/latest` | accept a release's page, or the newest release's permalink |
| `gitlab://code.videolan.org/videolan/dav1d@1.5.3` | list the release at that tag on another gitlab server, named after `//` by its host, with `:port` when that is not 443 |
| `https://code.videolan.org/videolan/dav1d/-/releases/1.5.3`, or `/-/releases/permalink/latest` | accept a release's page on another gitlab server, over https |

everything after the first `@` is the tag, so a tag may hold `/` or `@`, as `github:owner/repo@release/1.0`. no project path on gitlab.com starts with `/`, so `gitlab://` always names another server, while `gitlab:code.videolan.org/videolan/dav1d` stays a project in a group of that name on gitlab.com. other github.com and gitlab.com links keep what they did: a release file's own link, as `https://github.com/jqlang/jq/releases/download/jq-1.8.2/jq-linux-amd64`, is an ordinary [URL](urls-and-dois.md), and a file's page goes to its raw bytes.

## pins

`resolved` is the reference with the release's tag, `github:<owner>/<repo>@<tag>`, `gitlab:<project>@<tag>` or `gitlab://<host>/<project>@<tag>`: the latest release's tag when the reference names none. such a reference moves to a newer release only through [`lock --upgrade`](../commands/lock.md#upgrading); one that names a tag keeps naming it.

a github file keeps its name, its size and the SHA-256 github states for it, which downloads check. github states one for files uploaded since june 2025; an older file, as in jq's release `jq-1.6`, has none, and the lock pins the bytes its first download receives. its `url` is its download link on github.com, which sends the download on to github's storage. a file still uploading is listed as one fetchloom cannot download.

a gitlab release lists links, each with the path the release gives it, as `binaries/gitlab-runner-linux-amd64`, or for a link the release gives no path, the name its address ends with, as `dav1d-1.5.3.tar.xz`. gitlab states no size or hash for them, so the bytes the first download receives are what the lock pins. each `url` is the address the link points to, in the project's packages on gitlab.com or on another host: gitlab.com answers a program that asks it for a link to another host with a page warning of leaving gitlab, not the file. a link whose address is not https, or holds a secret, is listed as one fetchloom cannot download. the archives of the source code gitlab makes for every release are left out, since gitlab makes them on request rather than storing them.

downloads record each file's SHA-256 and size; bytes that differ from a stated SHA-256 or from the lock stop with `integrity.mismatch` and are never placed. a release has no license or DOI to record, and `access` is left out, since neither site says who may read a release.

| source | `resolved` | file `url` |
|---|---|---|
| github release | `github:<owner>/<repo>@<tag>` | the file's download link on github.com, `/releases/download/<tag>/<name>` |
| gitlab release | `gitlab:<project>@<tag>`, or `gitlab://<host>/<project>@<tag>` on another server | the address the link points to, on the server or another host |

## keys and limits

public releases need no key. github answers 60 requests an hour from one address without a key; listing a release takes one. once they are used up, fetchloom stops with `network.rate_limited`, exit 20, saying so, and at a terminal offers to sign in. a key raises the limit to 5,000 an hour: set `GH_TOKEN` or `GITHUB_TOKEN`, as the gh command reads them, or run `fetchloom auth login github`. a gitlab key, from `GITLAB_TOKEN` or `fetchloom auth login gitlab`, lets fetchloom list a private project's releases and download the links stored on gitlab.com. another gitlab server's key comes from `fetchloom auth login <host> --token` alone, as `fetchloom auth login code.videolan.org --token`, which saves it for that host.

a github key goes to api.github.com only, a gitlab key to gitlab.com only, and another gitlab server's key to that server alone, never on to the storage a download is sent to. since a github key never goes to github.com, a private repository's release files cannot be downloaded: download them with the gh command, then [`fetchloom add`](../commands/add.md#files-on-this-machine) the folder to pin them by their bytes.

releases are not in [search](../commands/search.md).
