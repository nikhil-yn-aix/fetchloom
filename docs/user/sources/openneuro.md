# OpenNeuro

OpenNeuro references list the files of a dataset's snapshot, a fixed version of a neuroimaging dataset, through openneuro's GraphQL API, and check each large file against the SHA-256 or MD5 its git-annex key states. public datasets need no account; private ones are refused.

```sh
fetchloom add openneuro:ds000001@1.0.0
fetchloom add openneuro:ds000001@1.0.0 --select "sub-01/anat/*"
fetchloom get https://openneuro.org/datasets/ds000001/versions/1.0.0 --select "*.tsv"
```

## references

| spelling | behavior |
|---|---|
| `openneuro:ds000001` | the newest snapshot |
| `openneuro:ds000001@1.0.0` | that snapshot |
| `https://openneuro.org/datasets/ds000001` | the dataset's page: the newest snapshot |
| `https://openneuro.org/datasets/ds000001/versions/1.0.0`, with `/download` or not | a snapshot's page |
| `doi:10.18112/openneuro.ds000001.v1.0.0` | a snapshot's DOI, which leads to its page |

an accession is `ds` and six digits, in any case. a draft, which a dataset has before its first snapshot, cannot be downloaded: `openneuro:<id>` of a dataset without a snapshot says so.

```console
$ fetchloom add openneuro:ds000001@1.0.0 --select "sub-01/anat/*" --as bart
  Resolving openneuro:ds000001@1.0.0
   Resolved openneuro:ds000001@1.0.0 · Balloon Analog Risk-taking Task · 2 files in sub-01/anat · 6.0 MiB · CC0
Downloading 2 files · 6.0 MiB
 Downloaded 2 files · 6.0 MiB in 767 ms
      Added bart, its files are in data/bart
```

## pins

`data.lock` keeps the snapshot it read, as `resolved = "openneuro:ds000001@1.0.0"`, with the dataset's name, license and DOI, and each file's path, size and link. openneuro keeps a dataset in git, with its large files in git-annex, whose key names each one by its bytes: `SHA256E-s1757261330--7cfba0….nii.gz` states the file's size and SHA-256, and the `MD5E` keys of older datasets state an MD5. the downloaded bytes must match what the key states before they are pinned. a key of another kind states no digest, and a file kept in git itself, such as a small `.tsv` or `README`, has none either; the lock's SHA-256 is then its only check.

a file in git-annex comes from openneuro's S3 bucket at the object version the snapshot pins, as `https://s3.amazonaws.com/openneuro.org/ds000001/sub-01/anat/sub-01_T1w.nii.gz?versionId=…`, so a later snapshot cannot change its bytes. a file in git, or one openneuro gives no S3 link for, comes from openneuro by its git object or key, `https://openneuro.org/crn/datasets/ds000001/objects/…`.

## listing

openneuro lists a snapshot one folder at a time, so the top folder takes a request, and each folder in it another, up to `most_files`. `--select` patterns that name folders, as `sub-01/anat/*` does, list only those folders and the ones on the way to them, so a dataset of hundreds of subjects takes a few requests. a folder the snapshot does not have has the whole snapshot listed instead, so the pattern that matches nothing is told against all it holds. DataLad's own `.datalad` folder, which holds none of the data, is left out.

the API refuses a query sent by a plain GET as a request another site may have forged, so each query carries the header it asks for, `apollo-require-preflight`. the run keeps the answers it reads by the query's link and that header, so the same query asked again is answered from them.

`s3://openneuro.org/ds000001/` lists the same files from the bucket as they are now, the newest snapshot, without its digests or a pin to one snapshot.

## access and limits

a private dataset, which openneuro shows only to the people its owners let in, is refused with `auth.required`, exit 40: fetchloom does not sign in to openneuro, and no key goes to it. a snapshot its owners deprecated is read with a warning that gives their reason. openneuro's search is not part of `fetchloom search`.
