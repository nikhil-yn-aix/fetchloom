# kaggle

kaggle references pin a dataset version and its selected files.

```sh
fetchloom add kaggle:uciml/breast-cancer-wisconsin-data@2
fetchloom get kaggle:uciml/breast-cancer-wisconsin-data@2
```

## references and pins

use `kaggle:` followed by the owner and dataset name, with `@version` for a specific version. `resolved` records that version. an explicitly versioned reference stays on it under `lock --upgrade`.

each file download uses the kaggle API with `datasetVersionNumber` and `raw=true`. the entire file path is encoded as one URL segment. SHA-256 and size pin its downloaded bytes.

selection patterns match source paths. [manifest patterns](../reference/data-toml.md#patterns) defines matching and [lock records](../reference/data-lock.md#what-each-source-records) defines the stored URL form.

## credentials

public downloads need no token. other access uses existing kaggle credentials:

| preference | location |
|---|---|
| token | `KAGGLE_API_TOKEN` or `~/.kaggle/access_token` |
| fallback account credentials | `KAGGLE_USERNAME` and `KAGGLE_KEY`, or `kaggle.json` in `KAGGLE_CONFIG_DIR` or `~/.kaggle` |

credentials stay out of the manifest, lock, logs and JSON output.

## limits

competitions are refused. fetchloom handles dataset downloads and does not accept competition rules or request access on your behalf.

follow [offline preparation](../concepts.md#offline) before disconnecting.
