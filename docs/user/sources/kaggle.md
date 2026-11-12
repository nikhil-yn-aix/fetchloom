# kaggle

a kaggle reference pins a dataset's version and its selected files, or a competition's data files as they are now.

```sh
fetchloom add kaggle:uciml/breast-cancer-wisconsin-data@2
fetchloom get kaggle:uciml/breast-cancer-wisconsin-data@2
fetchloom add kaggle:competitions/titanic --select "*.csv"
```

## references and pins

write `kaggle:` and the owner and dataset name, with `@<version>` for one version; without it, the newest. `resolved` records the version, and a reference that names one keeps it under `lock --upgrade`.

a kaggle DOI such as `doi:10.34740/kaggle/dsv/2736749` names one dataset version. fetchloom follows the `kaggle.com/dsv/` link it points to, one extra request, and resolves the dataset at that version.

each file downloads through the kaggle API with `datasetVersionNumber` and `raw=true`, its whole path encoded as one part of the link, and SHA-256 and size pin the bytes.

## competitions

use `kaggle:competitions/` followed by the name in the competition's link, such as `kaggle:competitions/titanic`. links under `kaggle.com/competitions/` and `kaggle.com/c/` work too, and `?select=` in a data page link picks that file.

kaggle sends competition data only to a signed-in account, and for most competitions only after it accepted the rules. getting-started ones such as titanic need no rules accepted:

1. sign in with `fetchloom auth login kaggle`, or keep the kaggle credentials described below.
2. open `https://www.kaggle.com/competitions/<name>/rules` while signed in to kaggle and accept the rules if the page asks. kaggle may ask you to verify a phone number first.
3. run the fetchloom command again.

without an account kaggle answers 401 and fetchloom stops with `auth.required` and the sign-in command. kaggle lists the files of a competition whose rules the account has not accepted, so fetchloom reads the smallest one first to ask, if its listed size is 4 MiB or less, even for a dry run, and keeps nothing of it. this threshold chooses the file; a zip wrapper or inaccurate listing can make the response larger. the API reader refuses responses over 64 MiB. if that probe answers 401, fetchloom stops with `auth.required` and the sign-in command; a 403 names the rules page. both stop before any other download. fetchloom cannot accept rules for you.

a competition has no versions. fetchloom lists its data files with the sizes kaggle states, downloads each file on its own through the kaggle API, and pins the SHA-256 and size of the bytes. the listed sizes are shown but not checked, since kaggle may send a file zipped. `resolved` records `kaggle:competitions/<name>` without a version, so if the organizers change the data, `sync` from `data.lock` stops at the SHA-256 check instead of placing the new bytes.

competition downloads have no `raw=true` option, and [kaggle's CLI](https://github.com/Kaggle/kaggle-api/blob/v2.2.5/src/kaggle/api/kaggle_api_extended.py) notes that kaggle sends a large file as a zip of that one file, `train.csv` as `train.csv.zip`. when the downloaded bytes are a zip whose only member has the requested file's name, fetchloom records the file in `data.lock` as `train.csv.zip` with the zip's SHA-256, and unpacks it as it unpacks any archive: `unpacks_to` pins the tree that holds the CSV's own SHA-256 and size. `data/<name>/train.csv` is then the CSV itself, and a later `sync` places the same bytes from the lock. with `extract = false` or `--no-extract`, the file stays packed as `train.csv.zip`. a competition that also lists a file named `train.csv.zip` would then pin two files on one path, so fetchloom stops before writing `data.lock` and asks for an exclude pattern that leaves one of them out.

## credentials

public datasets need no key. competitions take a key from `fetchloom auth login kaggle`, or the kaggle command's own credentials, `KAGGLE_API_TOKEN`, `~/.kaggle/access_token` or `kaggle.json`, in the [order environment gives](../reference/environment.md#credentials).

## limits

kaggle's api does not send the files of every dataset it lists. for those it answers `This dataset type is not supported through the API.`, and fetchloom stops with `reference.unresolved`, exit 10, saying the dataset cannot be fetched through kaggle's api. that answer does not change on a later try; the dataset's page shows how kaggle offers it.

follow [offline preparation](../concepts.md#offline) before disconnecting.
