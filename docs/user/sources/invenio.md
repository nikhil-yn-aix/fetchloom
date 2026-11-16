# invenioRDM

an invenioRDM reference names a record on an install of invenioRDM, the repository software zenodo runs, as CaltechDATA and TU Wien's research data do. its files are pinned by size and SHA-256, each checked against the MD5 the install states. zenodo's own records keep their [zenodo](zenodo.md) spelling.

```sh
fetchloom add invenio:data.caltech.edu/dt797-nsy79
fetchloom add https://researchdata.tuwien.ac.at/records/hg0hy-kxc20 --as horst --select "*.jar"
fetchloom get doi:10.48436/hg0hy-kxc20
```

## references

| reference | behavior |
|---|---|
| `invenio:data.caltech.edu/dt797-nsy79` | list the record with that id on the install at that host, with `:port` after the host when that is not 443 |
| `https://data.caltech.edu/records/dt797-nsy79` | a record's page: the record, once the install answers for it |
| a DOI the install registered, as `doi:10.48436/hg0hy-kxc20` | doi.org points to the install's page for the DOI, `/doi/<doi>`: the record whose id is the DOI's suffix, once the install answers for it with that DOI |

fetchloom knows no list of installs, so a record's page or a DOI's page on another host is known as invenioRDM's by exactly one request for the record at that origin, `/api/records/<id>`, which lists it too. that request goes where the link goes, under the rules for any link in data.toml. a host that answers for no such record leaves a pasted link a plain [URL](urls-and-dois.md), and a DOI one fetchloom cannot list. data.toml keeps a pasted link as you wrote it, and data.lock's `resolved` names the record, as `invenio:data.caltech.edu/dt797-nsy79`.

## pins

the files come from the install's records api, each at its link there, `https://<host>/api/records/<id>/files/<name>/content`. the install states each file's size and MD5, so a download is checked against both while its SHA-256 is learned. `license` is the first license the record states, as `cc-by-4.0`, and `doi` its DOI. `access` is `public` for a record anyone may read, and `login` for one whose files the install lists only to a key that may read them.

| source | `resolved` | file `url` |
|---|---|---|
| invenioRDM | `invenio:<host>/<id>` | `https://<host>/api/records/<id>/files/<name>/content` |

## keys

public records need no key. a restricted record needs a token the install made, as a personal access token from its applications settings: save it with `fetchloom auth login <host> --token`, which keeps it for that host alone and sends it as `Authorization: Bearer`. no variable gives such a key, and `ZENODO_TOKEN` or `fetchloom auth login zenodo` stay zenodo.org's. a refusal names the command, and at a terminal offers to sign in; when this project's data.toml chose the host, or named a DOI that led there, the offer first says so, as `Check that you trust data.caltech.edu: this project's data.toml named it`, since anyone can write a data.toml.

## limits

an install's records are not in [search](../commands/search.md). a link to one file of a record stays a plain link, and a record's parent id, which names its versions, is not read: name the version's own record.
