# dataverse

a dataverse reference pins a published version of a dataset, or of one file, on the installation it names.

```sh
fetchloom add dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP
fetchloom get dataverse:dataverse.harvard.edu/file:6867336
```

## references

| spelling | behavior |
|---|---|
| `dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP` | list a dataset by its persistent id |
| `dataverse:dataverse.harvard.edu/file:6867336` | list one file by its number |
| a dataset's or a file's page | read the dataset or file it shows |
| a `doi:` or `hdl:` persistent id on a dataverse host | list that dataset or file at its published version |

a pasted link keeps its `version`. a draft, an empty value or text that is no version is an error, never a request for the newest version; leave `version` out for that.

## pins

`resolved` holds the host, the dataset's persistent id and its `@major.minor` version, as `@5.0`, or for a file the host, its number and the version; a file's persistent id becomes its number. a reference without a version takes the newest published one, also for an account that can see an unpublished draft; drafts are never listed.

each file downloads from `/api/access/datafile/<number>?format=original`. the MD5 or SHA-256 dataverse states is checked; under another algorithm, the SHA-256 comes from the download alone. a change of filters keeps the resolved version, unless the reference changes or `lock --upgrade` asks for a newer one.

| source | `resolved` | file `url` |
|---|---|---|
| dataverse dataset | `dataverse:<host>/<persistent id>@<major.minor>`; the id includes `doi:` or `hdl:` | `/api/access/datafile/<id>?format=original` on that host |
| dataverse file | `dataverse:<host>/file:<numeric id>@<major.minor>`; a file PID resolves to its numeric id | `/api/access/datafile/<id>?format=original` on that host |

## credentials and limits

public files need no account. for restricted files you have access to, sign in for that installation:

```sh
fetchloom auth login dataverse
fetchloom auth login dataverse --host demo.dataverse.org
```

the first line is for harvard's installation. `DATAVERSE_API_TOKEN` also works when `DATAVERSE_URL` names the same installation, as pyDataverse reads them. `DATAVERSE_TOKEN` is not read.

a link to the api that carries the key in its query, as `https://dataverse.harvard.edu/api/access/datafile/6867336?key=<key>`, is refused by `add` and the other project commands, since data.toml and data.lock are shared: sign in instead, and the key goes in a header to that installation alone.

an installation on another port than 443 is signed in to with its port, as references name it: `fetchloom auth login dataverse --host dv.example.edu:8443`, or `DATAVERSE_URL=https://dv.example.edu:8443`. that key goes to that port only, and a key for port 443 never goes to another port.

signing in and `fetchloom auth` check the key with the installation, even one on a private network, such as a university intranet, since you named that installation yourself. `add` reaches an installation you type on the command line for that run, at its https origin alone; a later sync, or a clone of the project, reaches one on a private network only when the [`allowed_origins`](../reference/environment.md) setting lists it, as `https://dv.example.edu:8443`. a sign-in offer after a dataset or file metadata refusal uses that same installation and port.

without a key for its host, a dataset with any restricted file is refused before selection, naming those files, so `--exclude` cannot leave them out. sign in with an account that has access, or request access on the dataset's page. a key the installation does not allow for a file fails at that file's download. selection cannot bypass the listing-size limit.
