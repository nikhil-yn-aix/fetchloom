# dryad

dryad references pin one published version of a dataset, and check each file against the MD5 or SHA-256 dryad states for it. anyone can list a dataset, but dryad sends files only to an API account, so downloads need its key.

```sh
fetchloom add dryad:10.5061/dryad.j1fd7
fetchloom add https://datadryad.org/dataset/doi:10.5061/dryad.j1fd7 --select "*.csv"
fetchloom get doi:10.5061/dryad.j1fd7
```

## references

| spelling | behavior |
|---|---|
| `dryad:10.5061/dryad.j1fd7` | list the newest published version |
| `dryad:10.5061/dryad.j1fd7@1` | list published version 1 |
| `https://datadryad.org/dataset/doi:10.5061/dryad.j1fd7` | accept a dataset page, also under `/stash/dataset/` |
| a DOI dryad registered | follow doi.org to the dataset's page, then list it the same way |

a dataset is named by its DOI, whose case does not matter: `dryad:10.5061/DRYAD.J1FD7` names the same dataset. dryad also holds datasets under other DOI prefixes, such as `dryad:10.6078/d1h13d`. the version is the number dryad gives each published version, counted from 1. a version dryad does not list as published is refused with `reference.unresolved`.

other datadryad.org pages, such as a search or a file's download link, are refused with `reference.invalid`; paste the dataset's page instead.

## pins

`resolved` is `dryad:<doi>@<version>`: the newest published version when the reference names none. such a reference moves to a newer version only through [`lock --upgrade`](../commands/lock.md#upgrading); one that names a version keeps naming it. each file's `url` is its download link in dryad's API, `https://datadryad.org/api/v2/files/<id>/download`.

downloads check the MD5 or SHA-256 dryad states before the lock records each file's SHA-256 and size. bytes that differ stop with `integrity.mismatch` and are never placed. the license dryad states, CC0 for the datasets it publishes, is recorded as `license = "CC0-1.0"`, and `access` is `public` for a version dryad shows to anyone.

a listing that holds fewer files than dryad counts for the version is refused with `source.invalid` rather than shortened.

| source | `resolved` | file `url` |
|---|---|---|
| dryad | `dryad:<doi>@<version number>` | `https://datadryad.org/api/v2/files/<id>/download` |

## credentials and limits

dryad lets anyone list a dataset, but downloads only for an [API account](https://github.com/datadryad/dryad-app/blob/main/documentation/apis/api_accounts.md):

1. sign in at datadryad.org with your ORCID, which makes your dryad account.
2. on your account page, https://datadryad.org/account, make an API account and get a token.
3. run `fetchloom auth login dryad` and paste it, or set `DRYAD_TOKEN`.

a token lasts 10 hours; after that, get a new one the same way. a download without a working token stops with `auth.required`, exit 40, saying how to get one.

dryad allows 30 requests a minute without a token and 240 with one. dryad is not in [search](../commands/search.md), since most of what it would find cannot be downloaded without an account.
