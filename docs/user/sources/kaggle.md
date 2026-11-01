# kaggle

kaggle references pin a dataset version and its selected files, or the current data files of a competition.

```sh
fetchloom add kaggle:uciml/breast-cancer-wisconsin-data@2
fetchloom get kaggle:uciml/breast-cancer-wisconsin-data@2
fetchloom add kaggle:competitions/titanic --select "*.csv"
```

## references and pins

use `kaggle:` followed by the owner and dataset name, with `@version` for a specific version. `resolved` records that version. an explicitly versioned reference stays on it under `lock --upgrade`.

each file download uses the kaggle API with `datasetVersionNumber` and `raw=true`. the entire file path is encoded as one URL segment. SHA-256 and size pin its downloaded bytes.

selection patterns match source paths. [manifest patterns](../reference/data-toml.md#patterns) defines matching and [lock records](../reference/data-lock.md#what-each-source-records) defines the stored URL form.

## competitions

use `kaggle:competitions/` followed by the name in the competition's link, such as `kaggle:competitions/titanic`. links under `kaggle.com/competitions/` and `kaggle.com/c/` work too, and `?select=` in a data page link picks that file.

kaggle sends competition data only to a signed-in account that accepted the competition's rules:

1. sign in with `fetchloom auth login kaggle`, or keep the kaggle credentials described below.
2. open `https://www.kaggle.com/competitions/<name>/rules` while signed in to kaggle and accept the rules. kaggle may ask you to verify a phone number first.
3. run the fetchloom command again.

without an account kaggle answers 401 and fetchloom stops with `auth.required` and the sign-in command. an account that has not accepted the rules stops with `auth.required` and the rules page. fetchloom cannot accept rules for you.

a competition has no versions. fetchloom lists its data files with the sizes kaggle states, downloads each file on its own through the kaggle API, and pins the SHA-256 and size of the bytes. `resolved` records `kaggle:competitions/<name>` without a version, so if the organizers change the data, `sync` from `data.lock` stops at the SHA-256 check instead of placing the new bytes.

## credentials

public dataset downloads need no token. competitions and other access use existing kaggle credentials:

| preference | location |
|---|---|
| token | `KAGGLE_API_TOKEN` or `~/.kaggle/access_token` |
| fallback account credentials | `KAGGLE_USERNAME` and `KAGGLE_KEY`, or `kaggle.json` in `KAGGLE_CONFIG_DIR` or `~/.kaggle` |

credentials stay out of the manifest, lock, logs and JSON output.

## limits

competition downloads have no `raw=true` option. [kaggle's CLI](https://github.com/Kaggle/kaggle-api/blob/v2.2.5/src/kaggle/api/kaggle_api_extended.py) notes that kaggle can send a large file as a zip of that one file, `train.csv` as `train.csv.zip`. such a download does not match the listed size, and fetchloom refuses it with `integrity.mismatch`.

follow [offline preparation](../concepts.md#offline) before disconnecting.
