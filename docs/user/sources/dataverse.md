# dataverse

dataverse references pin published dataset or file versions from the named host.

```sh
fetchloom add dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP
fetchloom get dataverse:dataverse.harvard.edu/file:6867336
```

## references

| spelling | behavior |
|---|---|
| `dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP` | resolve a dataset's persistent id |
| `dataverse:dataverse.harvard.edu/file:6867336` | resolve a numeric file id |
| a dataset or file page | read its dataset or file reference |
| a `doi:` or `hdl:` persistent id on a dataverse host | resolve the published dataset or file version |

## pins

resolved dataset references contain the host, persistent id and `@major.minor` version. resolved file references contain the host, numeric file id and version. a file PID is converted to its numeric id.

a reference without a version resolves the newest published version, also when you are signed in with an account that can see an unpublished draft. drafts are never listed.

file URLs use `/api/access/datafile/` followed by the numeric id and `?format=original`. fetchloom checks stated MD5 or SHA-256. other checksum algorithms are ignored; SHA-256 is learned from the download instead.

[lock records](../reference/data-lock.md#what-each-source-records) defines the stored forms. changing filters keeps the resolved reference unless the manifest reference changes or an upgrade is requested.

## credentials and limits

public files need no account. for restricted files you have access to, sign in for that installation:

```sh
fetchloom auth login dataverse
fetchloom auth login dataverse --host demo.dataverse.org
```

the first line is for harvard's installation. `DATAVERSE_API_TOKEN` also works when `DATAVERSE_URL` names the same installation, as pyDataverse reads them. `DATAVERSE_TOKEN` is not read.

an installation on another port than 443 is signed in to with its port, as references name it: `fetchloom auth login dataverse --host dv.example.edu:8443`, or `DATAVERSE_URL=https://dv.example.edu:8443`. that key goes to that port only, and a key for port 443 never goes to another port.

signing in and `fetchloom auth` check the key with the installation, even one on a private network, such as a university intranet, since you named that host yourself.

without a key for its host, a dataset with any restricted file is refused before selection, naming those files, so `--exclude` cannot leave them out. sign in with an account that has access, or request access on the dataset's page. a key the installation does not allow for a file fails at that file's download. selection cannot bypass the listing-size limit.

follow [offline preparation](../concepts.md#offline) before disconnecting.
