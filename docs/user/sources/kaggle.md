# kaggle

kaggle references pin a dataset version and its selected files, or the current data files of a competition.

```sh
fetchloom add kaggle:uciml/breast-cancer-wisconsin-data@2
fetchloom get kaggle:uciml/breast-cancer-wisconsin-data@2
fetchloom add kaggle:competitions/titanic --select "*.csv"
```

## references and pins

use `kaggle:` followed by the owner and dataset name, with `@version` for a specific version. `resolved` records that version. an explicitly versioned reference stays on it under `lock --upgrade`.

a kaggle DOI such as `doi:10.34740/kaggle/dsv/2736749` names one dataset version. fetchloom follows the `kaggle.com/dsv/` link it points to, one extra request, and resolves the dataset at that version.

each file download uses the kaggle API with `datasetVersionNumber` and `raw=true`. the entire file path is encoded as one URL segment. SHA-256 and size pin its downloaded bytes.

selection patterns match source paths. [manifest patterns](../reference/data-toml.md#patterns) defines matching and [lock records](../reference/data-lock.md#what-each-source-records) defines the stored URL form.

## competitions

use `kaggle:competitions/` followed by the name in the competition's link, such as `kaggle:competitions/titanic`. links under `kaggle.com/competitions/` and `kaggle.com/c/` work too, and `?select=` in a data page link picks that file.

kaggle sends competition data only to a signed-in account, and for most competitions only after it accepted the rules. getting-started ones such as titanic need no rules accepted:

1. sign in with `fetchloom auth login kaggle`, or keep the kaggle credentials described below.
2. open `https://www.kaggle.com/competitions/<name>/rules` while signed in to kaggle and accept the rules if the page asks. kaggle may ask you to verify a phone number first.
3. run the fetchloom command again.

without an account kaggle answers 401 and fetchloom stops with `auth.required` and the sign-in command. kaggle lists the files of a competition whose rules the account has not accepted, so fetchloom reads the smallest one first to ask, if it is 4 MiB or less, even for a dry run, and keeps nothing of it. if that probe answers 401, fetchloom stops with `auth.required` and the sign-in command; a 403 names the rules page. both stop before any other download. fetchloom cannot accept rules for you.

a competition has no versions. fetchloom lists its data files with the sizes kaggle states, downloads each file on its own through the kaggle API, and pins the SHA-256 and size of the bytes. the listed sizes are shown but not checked, since kaggle may send a file zipped. `resolved` records `kaggle:competitions/<name>` without a version, so if the organizers change the data, `sync` from `data.lock` stops at the SHA-256 check instead of placing the new bytes.

competition downloads have no `raw=true` option, and [kaggle's CLI](https://github.com/Kaggle/kaggle-api/blob/v2.2.5/src/kaggle/api/kaggle_api_extended.py) notes that kaggle sends a large file as a zip of that one file, `train.csv` as `train.csv.zip`. when the downloaded bytes are a zip whose only member has the requested file's name, fetchloom records the file in `data.lock` as `train.csv.zip` with the zip's SHA-256, and unpacks it as it unpacks any archive: `unpacks_to` pins the tree that holds the CSV's own SHA-256 and size. `data/<name>/train.csv` is then the CSV itself, and a later `sync` places the same bytes from the lock. with `extract = false` or `--no-extract`, the file stays packed as `train.csv.zip`. a competition that also lists a file named `train.csv.zip` would then pin two files on one path, so fetchloom stops before writing `data.lock` and asks for an exclude pattern that leaves one of them out.

## credentials

public dataset downloads need no token. competitions and other access use existing kaggle credentials:

| preference | location |
|---|---|
| token | `KAGGLE_API_TOKEN` or `~/.kaggle/access_token` |
| fallback account credentials | `KAGGLE_USERNAME` and `KAGGLE_KEY`, or `kaggle.json` in `KAGGLE_CONFIG_DIR` or `~/.kaggle` |

credentials stay out of the manifest, lock, logs and JSON output.

## limits

follow [offline preparation](../concepts.md#offline) before disconnecting.
