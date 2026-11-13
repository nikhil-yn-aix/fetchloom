# sources

a reference names data at a source: a link, a DOI, or an id such as `zenodo:3608990`. `get` downloads what it names once, and `add` pins it in a project, each file by its SHA-256 and size in `data.lock`. where a source states a checksum, the download is checked against it before anything is pinned.

```sh
fetchloom get zenodo:3608990
fetchloom add zenodo:3608990 --select "*.csv"
```

a dataset's page works as well as its id, so you can paste the link from your browser. words instead of a reference [search](../commands/search.md) hugging face, kaggle, zenodo, GEO and openneuro; `fetchloom search --source` reaches ENA, dandi and arXiv too.

| source | what it holds | write it as | an account is needed for |
|---|---|---|---|
| [URLs and DOIs](urls-and-dois.md) | any file a link serves; a DOI leads to a source below | `https://…/iris.csv`, `doi:10.5281/zenodo.3608990` | a server that asks for a login |
| [zenodo](zenodo.md) | research data of any field | `zenodo:3608990` | restricted, embargoed and closed records |
| [figshare](figshare.md) | research data, also on institutions' own figshare sites | `figshare:34744689@1` | nothing public; embargoed articles are refused |
| [dataverse](dataverse.md) | datasets on harvard's and other dataverse installations | `dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP` | restricted files |
| [dryad](dryad.md) | data behind published papers | `dryad:10.5061/dryad.j1fd7` | every download |
| [mendeley data](mendeley.md) | research data | `mendeley:rscbjbr9sj@2` | nothing; an embargo stops the download |
| [OSF](osf.md) | the files of OSF projects | `osf:3eunr` | private projects |
| [hugging face](hugging-face.md) | machine learning datasets, models and spaces | `hf:datasets/scikit-learn/iris` | gated and private repositories |
| [kaggle](kaggle.md) | datasets and competitions | `kaggle:uciml/iris`, `kaggle:competitions/titanic` | competitions |
| [ENA](ena.md) | sequencing reads, SRA's and DDBJ's included | `ena:SRR390728` | nothing; controlled-access runs are left out |
| [GEO](geo.md) | gene expression series, samples and platforms | `geo:GSE1000` | nothing |
| [DANDI](dandi.md) | neurophysiology dandisets | `dandi:000027` | nothing; embargoed dandisets are refused |
| [OpenNeuro](openneuro.md) | neuroimaging datasets | `openneuro:ds000001@1.0.0` | nothing; private datasets are refused |
| [arXiv](arxiv.md) | papers' pdfs: one paper, a month's or one category's month | `arxiv:1706.03762`, `arxiv:2401`, `arxiv:cs.LG/2401` | nothing |
| OpenAlex, through [cloud buckets](buckets.md) | OpenAlex's snapshot of scholarly works, authors and venues | `s3://openalex/data/jsonl/works/` | nothing |
| [github and gitlab releases](releases.md) | the files of a release | `github:jqlang/jq@jq-1.8.2`, `gitlab:gitlab-org/gitlab-runner@v19.3.6` | private gitlab projects, and on github more than 60 requests an hour |
| [google drive](google-drive.md) | shared files and folders | a shared link, `drive:<id>` or `drive:folders/<id>` | items shared only with some people |
| [dropbox](dropbox.md) | shared files and folders | a shared link | nothing |
| [cloud buckets](buckets.md) | S3, services that speak its API, and google cloud storage | `s3://<bucket>/<prefix>/`, `gs://<bucket>/<prefix>/` | private buckets |
| [azure blob storage](azure.md) | azure containers | `azure:<account>/<container>/<prefix>/` | private containers |

[signing in](../signing-in.md) says how to give fetchloom a key. accepting a reference does not promise access or a download: hosts move and remove files, and [locked URL recovery](../concepts.md#recovery) can find a file's new link while keeping its pinned hashes.

## references and paths

a reference is never a path on this machine. `add` given a path, such as `./flowers`, `/data/flowers.csv`, `~/flowers.csv`, `C:\data\flowers.csv` or `file:///data/flowers.csv`, imports the files there, [pinned by their bytes](../commands/add.md#files-on-this-machine) with `ref = "local"`, and sends nothing anywhere. `get` refuses a path with `reference.invalid`, exit 10, before any request, and so does sync for a path written as `ref` in data.toml; [choosing a dataset](../commands/search.md#choosing-a-dataset) says what a plain word that names a folder here does.

a link to a folder or a site's page, which answers with a web page rather than a file, is refused with `source.invalid` before anything is cached or pinned; [URLs and DOIs](urls-and-dois.md) says when.

## choosing files

`--select` and `--exclude` match the paths a source lists, before anything downloads. quote them so your shell passes them unchanged. each select pattern must match at least one path before exclusions, and some files must remain. patterns do not reach members inside an archive; a [members table](../reference/data-toml.md#files-inside-archives) in data.toml does, and [`fetchloom drop`](../commands/drop.md) writes one.

the limit on how many files a dataset lists applies before filtering, and filters cannot get around a source's access checks, such as zenodo's, figshare's or dataverse's. a selected google docs entry is different: exclude it, and the fixed files around it still download.

## what a pin records

`Resolved` names what a reference resolved to, with the title, file count, size and license a source states; a plain link has no such line. a license holding `-NC` or `-ND`, in any case, gets a warning, and so does arXiv's own license, which does not allow sharing copies, so check the license before you use or share those files. [lock records](../reference/data-lock.md#what-each-source-records) gives what each source writes in data.lock.
