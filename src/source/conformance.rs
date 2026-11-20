//! the conformance suite every built-in adapter runs. a row is a source's reference and the
//! answers its own tests recorded. each property resolves every row, on those answers as they
//! were or made hostile, and holds what any adapter must keep: the origins it asks, the work it
//! bounds, the names it refuses, errors that say what to do, sizes and digests read strictly,
//! and keys read by its own code alone. a new source adds its descriptor and a row.

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};

use percent_encoding::percent_decode_str;
use serde_json::Value;
use url::Url;

use super::access::{self, ALL, Access, Origins};
use super::recorded::{Recorded, block_on, fixture_bytes};
use super::{Listing, Reference, Remote};
use crate::account::Account;
use crate::error::{Error, Kind};

/// a row: a source's network, as one of its tests recorded it, with the reference it resolves and
/// the variables of the run that resolves it, as a key it signs in with.
struct Row {
    reference: &'static str,
    recorded: Recorded,
    keys: Keys,
}

/// variables a run has, by name and value.
type Keys = &'static [(&'static str, &'static str)];

/// a github key, which shows a private repository to a run.
const GITHUB_KEY: Keys = &[("GH_TOKEN", "ghp_conformance_key")];

fn row(reference: &'static str, answers: &[(&str, &str)]) -> Row {
    let recorded = answers
        .iter()
        .fold(Recorded::default(), |recorded, (url, fixture)| {
            recorded.fixture(url, fixture)
        });
    Row {
        reference,
        recorded,
        keys: &[],
    }
}

/// gives the sources on this thread `keys` until it is dropped, even by a panic.
struct Keyed;

fn keyed(keys: Keys) -> Keyed {
    super::TEST_VARIABLES.with_borrow_mut(|set| {
        set.clear();
        set.extend_from_slice(keys);
    });
    Keyed
}

impl Drop for Keyed {
    fn drop(&mut self) {
        super::TEST_VARIABLES.with_borrow_mut(Vec::clear);
    }
}

/// a row for every source that asks anything, each answered as its own tests record it.
#[expect(clippy::too_many_lines, reason = "a row for each source")]
fn rows() -> Vec<Row> {
    let openneuro = |query: &str| {
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("query", query)
            .finish();
        format!("https://openneuro.org/crn/graphql?{encoded}")
    };
    let snapshot = |rest: &str| {
        openneuro(&format!(
            r#"query{{snapshot(datasetId:"ds000001",tag:"1.0.0"){{{rest}}}}}"#
        ))
    };
    let folder = |tree: &str| {
        snapshot(&format!(
            r#"files(tree:"{tree}"){{id filename size directory annexed urls}}"#
        ))
    };
    let ena = |path: &str| format!("https://www.ebi.ac.uk/ena/portal/api/{path}");
    let kaggle = "https://api.kaggle.com/v1";
    let geo = "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/";
    let dandi = "https://api.dandiarchive.org/api/dandisets/000027/";
    let hf = "https://huggingface.co/api/datasets/stanfordnlp/imdb";
    let mirror = "https://storage.googleapis.com/storage/v1/b/arxiv-dataset/o?prefix=arxiv%2Farxiv%2Fpdf%2F1706%2F1706.03762v7.pdf&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken";
    let paper = "https://export.arxiv.org/api/query?id_list=1706.03762";
    let license = "https://oaipmh.arxiv.org/oai?verb=GetRecord&identifier=oai%3AarXiv.org%3A1706.03762&metadataPrefix=arXivRaw";
    let s2 = "https://api.semanticscholar.org";
    let s2_paper = format!(
        "{s2}/graph/v1/paper/ARXIV:1706.03762?fields=paperId%2CcorpusId%2CexternalIds%2Curl%2Ctitle%2Cyear%2Cauthors%2CopenAccessPdf"
    );
    let held = r#"{"items":[{"name":"arxiv/arxiv/pdf/1706/1706.03762v7.pdf","size":"2215244","md5Hash":"F+Ni5+W6b/tiSMSi6SPmPg==","generation":"7"}]}"#;
    let mut rows = vec![
        row(
            "zenodo:3608990",
            &[("https://zenodo.org/api/records/3608990", "zenodo/iris.json")],
        ),
        row(
            "invenio:data.caltech.edu/dt797-nsy79",
            &[(
                "https://data.caltech.edu/api/records/dt797-nsy79",
                "invenio/caltech-record.json",
            )],
        ),
        row(
            "doi:10.5281/zenodo.3608990",
            &[
                (
                    "https://doi.org/api/handles/10.5281%2Fzenodo.3608990",
                    "doi/zenodo.json",
                ),
                ("https://zenodo.org/api/records/3608990", "zenodo/iris.json"),
            ],
        ),
        row(
            "dryad:10.5061/dryad.j1fd7",
            &[
                (
                    "https://datadryad.org/api/v2/datasets/doi:10.5061%2Fdryad.j1fd7",
                    "dryad/dataset.json",
                ),
                (
                    "https://datadryad.org/api/v2/versions/26724/files?page=1&per_page=100",
                    "dryad/files.json",
                ),
            ],
        ),
        row(
            "github:jqlang/jq@jq-1.8.2",
            &[(
                "https://api.github.com/repos/jqlang/jq/releases/tags/jq-1.8.2",
                "github/release.json",
            )],
        ),
        row(
            "gitlab:gitlab-org/gitlab-runner@v19.3.6",
            &[(
                "https://gitlab.com/api/v4/projects/gitlab-org%2Fgitlab-runner/releases/v19.3.6",
                "gitlab/release.json",
            )],
        ),
        row(
            "gitlab://code.videolan.org/videolan/dav1d@1.5.3",
            &[(
                "https://code.videolan.org/api/v4/projects/videolan%2Fdav1d/releases/1.5.3",
                "gitlab/dav1d-release.json",
            )],
        ),
        row(
            "https://gitlab.com/gitlab-org/gitlab-test/-/tree/master/files",
            &[
                (
                    "https://gitlab.com/api/v4/projects/gitlab-org%2Fgitlab-test/repository/commits/master",
                    "gitlab/test-commit.json",
                ),
                (
                    "https://gitlab.com/api/v4/projects/gitlab-org%2Fgitlab-test/repository/tree?path=files&ref=ddd0f15ae83993f5cb66a927a28673882e99100b&recursive=true&per_page=100&pagination=keyset",
                    "gitlab/test-tree.json",
                ),
            ],
        ),
        row(
            "ena:SRR390728",
            &[
                (
                    &ena("filereportcount?accession=SRR390728&result=read_run&format=json"),
                    "ena/count.json",
                ),
                (
                    &ena(&format!(
                        "filereport?accession=SRR390728&result=read_run&fields={}&format=json&limit=1",
                        crate::source::ena::FIELDS
                    )),
                    "ena/run.json",
                ),
            ],
        ),
        row(
            "geo:GSE1000",
            &[
                (geo, "geo/series.html"),
                (&format!("{geo}matrix/"), "geo/matrix.html"),
                (&format!("{geo}miniml/"), "geo/miniml.html"),
                (&format!("{geo}soft/"), "geo/soft.html"),
                (&format!("{geo}suppl/"), "geo/suppl.html"),
                (&format!("{geo}suppl/filelist.txt"), "geo/filelist.txt"),
            ],
        ),
        row(
            "dandi:000027",
            &[
                (dandi, "dandi/set.json"),
                (
                    &format!("{dandi}versions/0.210831.2033/info/"),
                    "dandi/info.json",
                ),
                (
                    &format!(
                        "{dandi}versions/0.210831.2033/assets/?metadata=true&order=path&page_size=1000"
                    ),
                    "dandi/assets.json",
                ),
            ],
        ),
        row(
            "openneuro:ds000001@1.0.0",
            &[
                (
                    &snapshot(
                        "tag deprecated{reason} description{Name License DatasetDOI} files{id filename size directory annexed urls}",
                    ),
                    "openneuro/top.json",
                ),
                (
                    &folder("db72baeb3665a309db35d6ef649e117eb45a8a87"),
                    "openneuro/sub-01.json",
                ),
                (
                    &folder("26d2ff4f763308d462f151c53e3e06f6b7d2aaaa"),
                    "openneuro/anat.json",
                ),
                (
                    &folder("4113b1609e2b0ea2e0a04e0800204d2ee6aab578"),
                    "openneuro/func.json",
                ),
            ],
        ),
        row(
            "figshare:34744689",
            &[(
                "https://api.figshare.com/v2/articles/34744689",
                "figshare/article.json",
            )],
        ),
        row(
            "ia:iris_20240626",
            &[("https://archive.org/metadata/iris_20240626", "ia/iris.json")],
        ),
        row(
            "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP",
            &[(
                "https://dataverse.harvard.edu/api/datasets/:persistentId/versions/:latest-published?persistentId=doi%3A10.7910%2FDVN%2FTJCLKP",
                "dataverse/version.json",
            )],
        ),
        row(
            "osf:ezcuj",
            &[
                (
                    "https://api.osf.io/v2/nodes/ezcuj/?embed=license&embed=identifiers&related_counts=children",
                    "osf/embedded.json",
                ),
                (
                    "https://api.osf.io/v2/nodes/ezcuj/files/osfstorage/?page%5Bsize%5D=100",
                    "osf/root.json",
                ),
            ],
        ),
        row(
            "hf:datasets/stanfordnlp/imdb",
            &[
                (
                    &format!(
                        "{hf}/revision/main?expand=sha&expand=gated&expand=private&expand=tags&expand=cardData"
                    ),
                    "hf/imdb-revision.json",
                ),
                (
                    &format!("{hf}/tree/e6281661ce1c48d982bc483cf8a173c1bbeb5d31?recursive=true"),
                    "hf/imdb-tree.json",
                ),
            ],
        ),
        row(
            "kaggle:uciml/iris",
            &[
                (
                    &format!("{kaggle}/datasets/view/uciml/iris"),
                    "kaggle/iris-view.json",
                ),
                (
                    &format!(
                        "{kaggle}/datasets/list/uciml/iris?datasetVersionNumber=2&pageSize=200"
                    ),
                    "kaggle/iris-files.json",
                ),
            ],
        ),
        row(
            "arxiv:1706.03762",
            &[(paper, "arxiv/paper.xml"), (license, "arxiv/license.xml")],
        ),
        row(
            "s2:ARXIV:1706.03762",
            &[
                (&s2_paper, "s2/paper-arxiv.json"),
                (paper, "arxiv/paper.xml"),
                (license, "arxiv/license.xml"),
            ],
        ),
        row(
            "s2:tldrs",
            &[
                (
                    &format!("{s2}/datasets/v1/release/latest"),
                    "s2/release.json",
                ),
                (
                    &format!("{s2}/datasets/v1/release/2026-11-10/dataset/tldrs"),
                    "s2/dataset.json",
                ),
            ],
        ),
        row("https://www.dropbox.com/s/abc123/iris.csv?dl=0", &[]),
        row("https://example.org/iris.csv", &[]),
    ];
    // the answers whose bodies a test makes rather than records
    let competition = Recorded::default()
        .on(
            &format!("{kaggle}/competitions/get/titanic"),
            200,
            entered(),
        )
        .fixture(
            &format!("{kaggle}/competitions/data/list/titanic?pageSize=200"),
            "kaggle/titanic-files.json",
        );
    rows.push(Row {
        reference: "kaggle:competitions/titanic",
        recorded: competition,
        keys: &[],
    });
    let seaborn = "https://api.github.com/repos/mwaskom/seaborn-data";
    let seaborn_tree = Recorded::default()
        .on(
            &format!("{seaborn}/commits/master"),
            200,
            "71e2436a092d714350de0fc409ca8a8714e7e78f",
        )
        .fixture(
            &format!("{seaborn}/git/trees/71e2436a092d714350de0fc409ca8a8714e7e78f?recursive=1"),
            "github/seaborn-tree.json",
        )
        .fixture(
            &format!(
                "{seaborn}/git/trees/71e2436a092d714350de0fc409ca8a8714e7e78f:raw?recursive=1"
            ),
            "github/seaborn-raw-tree.json",
        )
        .fixture(
            &format!(
                "{seaborn}/git/trees/71e2436a092d714350de0fc409ca8a8714e7e78f:process?recursive=1"
            ),
            "github/seaborn-process-tree.json",
        );
    rows.push(Row {
        reference: "https://github.com/mwaskom/seaborn-data/tree/master/raw",
        recorded: seaborn_tree.clone(),
        keys: &[],
    });
    // a private repository, which a key reads: its files pin at github's api, which takes the key
    rows.push(Row {
        reference: "https://github.com/mwaskom/seaborn-data/tree/master/process",
        recorded: seaborn_tree.on(
            seaborn,
            200,
            r#"{"full_name":"mwaskom/seaborn-data","private":true}"#,
        ),
        keys: GITHUB_KEY,
    });
    rows.push(Row {
        reference: "https://github.com/jqlang/jq/releases/tag/jq-1.8.2",
        recorded: Recorded::default()
            .fixture(
                "https://api.github.com/repos/jqlang/jq/releases/tags/jq-1.8.2",
                "github/release.json",
            )
            .on(
                "https://api.github.com/repos/jqlang/jq",
                200,
                r#"{"full_name":"jqlang/jq","private":true}"#,
            ),
        keys: GITHUB_KEY,
    });
    let open = "https://drive.google.com/open?id=1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb";
    rows.push(Row {
        reference: open,
        recorded: Recorded::default()
            .fixture(open, "drive/open-file.html")
            .redirected(
                open,
                "https://drive.google.com/file/d/1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb/view",
            ),
        keys: &[],
    });
    for row in &mut rows {
        if row.reference.contains("1706.03762") {
            row.recorded =
                std::mem::take(&mut row.recorded).on(mirror, 200, held.as_bytes().to_vec());
        }
    }
    let mendeley = "https://data.mendeley.com/public-api/datasets/rscbjbr9sj";
    let root = format!("{mendeley}/files?folder_id=root&version=2&$start=0");
    rows.push(Row {
        reference: "mendeley:rscbjbr9sj@2",
        recorded: Recorded::default()
            .fixture(&format!("{mendeley}?version=2"), "mendeley/dataset.json")
            .fixture(&format!("{mendeley}/folders/2"), "mendeley/folders.json")
            .fixture(&root, "mendeley/files.json")
            .total(&root, 3),
        keys: &[],
    });
    let s3 =
        "https://s3.amazonaws.com/openneuro.org/?list-type=2&prefix=ds000001%2F&encoding-type=url";
    rows.push(row(
        "s3://openneuro.org/ds000001/",
        &[
            (s3, "bucket/openneuro-page1.xml"),
            (
                &format!(
                    "{s3}&continuation-token=1a626583KTlgb7ONlPzl9YLS39I6ZpxwWL65%2Fcvpy0Frgusxmb5cxvg%3D%3D"
                ),
                "bucket/openneuro-page2.xml",
            ),
        ],
    ));
    let gcs = "https://storage.googleapis.com/storage/v1/b/cloud-samples-data/o?prefix=bigquery%2Fus-states%2F&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken";
    rows.push(row(
        "gs://cloud-samples-data/bigquery/us-states/",
        &[
            (gcs, "bucket/gcs-page1.json"),
            (
                &format!(
                    "{gcs}&pageToken=ClNiaWdxdWVyeS91cy1zdGF0ZXMvMjAyMS0wNy0wMlQxNjowNDo0OF83MDM0NC9hbGxfbmFtZXNwYWNlcy9raW5kX3VzLXN0YXRlcy9vdXRwdXQtMA%3D%3D"
                ),
                "bucket/gcs-page2.json",
            ),
        ],
    ));
    rows.push(Row {
        reference: "https://example.org/data/SHA256SUMS",
        recorded: Recorded::default().on(
            "https://example.org/data/SHA256SUMS",
            200,
            "e54cc4d2ce1bff65d32ca60b3e4b802e06bde1d7e7caf6f796f6bf7370e863b0  tips.csv\nSHA256 (sub/iris.csv) = 9cc1c345c71bcc9b486b74cbf6063fa66f4bb5e0f603a4b3c3471ec2e5e8e355\n",
        ),
        keys: &[],
    });
    rows.push(row(
        "https://example.org/data/",
        &[
            ("https://example.org/data/", "folder/data.html"),
            ("https://example.org/data/sub/", "folder/data-sub.html"),
        ],
    ));
    rows.extend(walks());
    let azure = "https://azureopendatastorage.blob.core.windows.net/mnist?restype=container&comp=list&prefix=processedManifest%2F";
    rows.push(row(
        "azure:azureopendatastorage/mnist/processedManifest/",
        &[
            (azure, "azure/folder-1.xml"),
            (
                &format!(
                    "{azure}&marker=2%21120%21MDAwMDQ1IXByb2Nlc3NlZE1hbmlmZXN0L3NwZHhfMi4yL21hbmlmZXN0LnNwZHguanNvbiEwMDAwMjghOTk5OS0xMi0zMVQyMzo1OTo1OS45OTk5OTk5WiE-"
                ),
                "azure/folder-2.xml",
            ),
        ],
    ));
    rows
}

/// rows whose listing takes pages or folders one request each, as their sources' tests record
/// them: osf's three pages and two folders, a drive folder of folders, dandi's two pages, and
/// arXiv's months, of the mirror's folder and of a category's pages.
#[expect(clippy::too_many_lines, reason = "a row for each walk")]
fn walks() -> Vec<Row> {
    let osf = "https://api.osf.io/v2/nodes/ytpuq";
    let storage = format!("{osf}/files/osfstorage");
    let drive = |id: &str| format!("https://drive.google.com/embeddedfolderview?id={id}");
    let dandi = "https://api.dandiarchive.org/api/dandisets/001512/versions/0.250624.1820";
    let mirror = |key: &str| {
        format!(
            "https://storage.googleapis.com/storage/v1/b/arxiv-dataset/o?prefix={}&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken",
            key.replace('/', "%2F")
        )
    };
    let held = |numbers: &[&str]| {
        let items: Vec<Value> = numbers
            .iter()
            .map(|number| {
                serde_json::json!({
                    "name": format!("arxiv/arxiv/pdf/2401/2401.{number}.pdf"),
                    "size": "2215244",
                    "md5Hash": "F+Ni5+W6b/tiSMSi6SPmPg==",
                    "generation": "7",
                })
            })
            .collect();
        serde_json::to_vec(&serde_json::json!({ "items": items })).unwrap()
    };
    let feed = r#"<?xml version='1.0' encoding='UTF-8'?>
<feed xmlns:opensearch="http://a9.com/-/spec/opensearch/1.1/" xmlns="http://www.w3.org/2005/Atom"><opensearch:totalResults>2</opensearch:totalResults><entry><id>http://arxiv.org/abs/2401.00001v1</id><title>Paper one</title><published>2024-01-10T10:00:00Z</published><author><name>Ada Lovelace</name></author></entry><entry><id>http://arxiv.org/abs/2401.00002v1</id><title>Paper two</title><published>2024-01-11T10:00:00Z</published><author><name>Ada Lovelace</name></author></entry></feed>"#;
    let category = "https://export.arxiv.org/api/query?search_query=cat%3Acs.LG+AND+submittedDate%3A%5B202401010000+TO+202402010000%5D&sortBy=submittedDate&sortOrder=ascending&start=0&max_results=1000";
    let folder = mirror("arxiv/arxiv/pdf/2401/");
    vec![
        row(
            "osf:ytpuq",
            &[
                (
                    &format!("{osf}/?embed=license&embed=identifiers&related_counts=children"),
                    "osf/analysis-node.json",
                ),
                (&format!("{storage}/?page%5Bsize%5D=100"), "osf/page1.json"),
                (
                    &format!("{storage}/?page=2&page%5Bsize%5D=2"),
                    "osf/page2.json",
                ),
                (
                    &format!("{storage}/?page=3&page%5Bsize%5D=2"),
                    "osf/page3.json",
                ),
                (
                    &format!("{storage}/55a7dfbb8c5e4a66f26dcac4/?page%5Bsize%5D=100"),
                    "osf/folder-55a7dfbb8c5e4a66f26dcac4.json",
                ),
                (
                    &format!("{storage}/55d386c68c5e4a2d0b438d39/?page%5Bsize%5D=100"),
                    "osf/folder-55d386c68c5e4a2d0b438d39.json",
                ),
            ],
        ),
        row(
            "drive:folders/1oow464Z4Et95RAwtRZn6dLOFb_YYpLwL",
            &[
                (
                    &drive("1oow464Z4Et95RAwtRZn6dLOFb_YYpLwL"),
                    "drive/folder-deep.html",
                ),
                (
                    &drive("1TMZoSAu4ecs_Q3GEEWfiyrQIPtj3DJAC"),
                    "drive/folder-deep-1.html",
                ),
                (
                    &drive("1RRfB3nFcMtg_ibfCjoU6P6DbsrXkwnPF"),
                    "drive/folder-deep-2.html",
                ),
                (
                    &drive("1-Z2wzjaaqqB1lBiPXNGfgkICA2Pc9q_U"),
                    "drive/folder-deep-1-1.html",
                ),
            ],
        ),
        row(
            "dandi:001512@0.250624.1820",
            &[
                (&format!("{dandi}/info/"), "dandi/paged-info.json"),
                (
                    &format!("{dandi}/assets/?metadata=true&order=path&page_size=1000"),
                    "dandi/page-1.json",
                ),
                (
                    &format!("{dandi}/assets/?metadata=true&order=path&page=2&page_size=2"),
                    "dandi/page-2.json",
                ),
            ],
        ),
        Row {
            reference: "arxiv:2401",
            recorded: Recorded::default().on(
                &folder,
                200,
                held(&["00001v1", "00002v1", "00002v2"]),
            ),
            keys: &[],
        },
        Row {
            reference: "arxiv:cs.LG/2401",
            recorded: Recorded::default().on(category, 200, feed).on(
                &folder,
                200,
                held(&["00001v1", "00002v1"]),
            ),
            keys: &[],
        },
    ]
}

/// kaggle's answer about titanic as an account that accepted its rules gets it.
fn entered() -> Vec<u8> {
    let mut value: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("kaggle/titanic-competition.json")).unwrap();
    value["userHasEntered"] = true.into();
    serde_json::to_vec(&value).unwrap()
}

/// whether `url`, which a resolve asked, is one its reference's source or the source it led to
/// may ask.
fn asked_inside(reference: &Reference, listing: &Listing, url: &Url) -> bool {
    [reference, &listing.resolved].iter().any(|named| {
        named
            .access()
            .api
            .holds(url, named.written_origin().as_ref())
    })
}

/// the conformance check of every adapter: each url a resolve asked lies inside the descriptor of
/// the source that asked it, and each file it listed links where its source's files are kept.
#[test]
fn every_adapter_asks_and_links_only_inside_its_descriptor() {
    let rows = rows();
    let mut sources: Vec<&str> = rows
        .iter()
        .map(|row| {
            Reference::parse(row.reference)
                .unwrap()
                .reference
                .access()
                .name
        })
        .collect();
    sources.sort_unstable();
    sources.dedup();
    let every: Vec<&str> = ALL
        .iter()
        .map(|access| access.name)
        .filter(|name| *name != "imported files")
        .collect();
    for name in &every {
        assert!(sources.contains(name), "no row asks {name}");
    }
    for row in rows {
        let reference = Reference::parse(row.reference).unwrap().reference;
        let listing = {
            let _keyed = keyed(row.keys);
            block_on(reference.resolve(&row.recorded))
        }
        .unwrap_or_else(|error| panic!("{}: {}", row.reference, error.message));
        assert!(!listing.files.is_empty(), "{}", row.reference);
        for asked in row.recorded.asked() {
            let url = Url::parse(asked.split(' ').next().unwrap()).unwrap();
            assert!(
                asked_inside(&reference, &listing, &url),
                "{} asked {url}",
                row.reference
            );
        }
        let from = listing.resolved.access();
        let written = listing.resolved.written_origin();
        for Remote { url, path, .. } in &listing.files {
            assert!(
                from.files.holds(url, written.as_ref()),
                "{} links {path} at {url}",
                row.reference
            );
        }
    }
}

/// a row with keys resolves as a run that has them: github's private repository, read by a key,
/// pins every file at its api, which takes the key, so the properties hold that path too.
#[test]
fn rows_with_keys_resolve_as_runs_with_keys() {
    let keyed_rows: Vec<Row> = rows()
        .into_iter()
        .filter(|row| !row.keys.is_empty())
        .collect();
    assert!(!keyed_rows.is_empty());
    for row in keyed_rows {
        let reference = Reference::parse(row.reference).unwrap().reference;
        let listing = {
            let _keyed = keyed(row.keys);
            block_on(reference.resolve(&row.recorded))
        }
        .unwrap_or_else(|error| panic!("{}: {}", row.reference, error.message));
        assert_eq!(listing.access, super::Access::Login, "{}", row.reference);
        assert!(
            listing
                .files
                .iter()
                .all(|file| file.url.host_str() == Some("api.github.com")),
            "{}",
            row.reference
        );
    }
}

/// the requests past which an adapter counts as going round a loop: far above what any row
/// asks, since its pages are bounded by the few files a hostile run lets it list.
const MOST_ASKED: usize = 1_000;

/// an html page a sign-in wall sends with 200 in place of the answer asked for.
const SIGN_IN: &str = "<!DOCTYPE html><html><head><title>Sign in</title></head><body><form method=\"post\" action=\"/login\"><input name=\"email\"><input name=\"password\" type=\"password\"><button>Sign in</button></form></body></html>";

/// names a listing may carry that no file may be placed at: each refused on every machine, or
/// before placing on the machines that cannot store it.
const HOSTILE: [&str; 17] = [
    "../hostile.csv",
    "hostile/../../x.csv",
    "/hostile.csv",
    "a\\hostile.csv",
    "a//hostile.csv",
    "a/./hostile.csv",
    "C:hostile.csv",
    "hostile\u{202e}vsc.exe",
    "hostile\u{7}.csv",
    "",
    ".",
    "..",
    "CON",
    "hostile:x.csv",
    "hostile.csv.",
    "hostile<x>.csv",
    ".git",
];

/// what resolving a row came to.
#[expect(
    clippy::large_enum_variant,
    reason = "a row's outcome is judged at once, one row at a time"
)]
enum Outcome {
    Listed(Listing),
    Failed(Error),
    /// the adapter panicked, or asked more than [`MOST_ASKED`], with the panic's message.
    Panicked(String),
}

/// a row ready to resolve, with what it lists on its answers as recorded.
struct Subject {
    text: &'static str,
    reference: Reference,
    recorded: Recorded,
    keys: Keys,
    listed: Listing,
    /// the requests the listing took.
    asks: usize,
}

impl Subject {
    fn of(row: Row) -> Subject {
        let reference = Reference::parse(row.reference).unwrap().reference;
        let recorded = row.recorded.clone();
        let listed = {
            let _keyed = keyed(row.keys);
            block_on(reference.resolve(&recorded))
        }
        .unwrap_or_else(|error| panic!("{}: {}", row.reference, error.message));
        Subject {
            text: row.reference,
            reference,
            recorded: row.recorded,
            keys: row.keys,
            listed,
            asks: recorded.asked().len(),
        }
    }

    /// the files and unavailable entries it lists.
    fn count(&self) -> usize {
        self.listed.files.len() + self.listed.unavailable.len()
    }

    /// its network as a hostile run has it: a link it never recorded is not found, a loop is
    /// stopped, and a listing holds few more files than its own.
    fn hostile(&self) -> Recorded {
        self.recorded
            .clone()
            .otherwise(404)
            .at_most(MOST_ASKED)
            .most(self.count() + 10)
    }

    fn resolve(&self, recorded: &Recorded) -> Outcome {
        let _keyed = keyed(self.keys);
        let resolved = catch_unwind(AssertUnwindSafe(|| {
            block_on(self.reference.resolve(recorded))
        }));
        match resolved {
            Ok(Ok(listing)) => Outcome::Listed(listing),
            Ok(Err(error)) => Outcome::Failed(error),
            Err(panic) => Outcome::Panicked(
                panic
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| panic.downcast_ref::<&str>().map(|text| (*text).to_owned()))
                    .unwrap_or_default(),
            ),
        }
    }

    /// whether `listing` lists the same files as the answers as recorded do.
    fn same_files(&self, listing: &Listing) -> bool {
        listing.files == self.listed.files
    }
}

fn subjects() -> Vec<Subject> {
    rows().into_iter().map(Subject::of).collect()
}

/// findings in sources other builders are changing, handed to them: the property, the row, a
/// part of the change and the start of the problem. a fix deletes its line, which the property
/// then holds; a line no finding matches fails, so it cannot outlive its fix.
const HANDED_OFF: [(&str, &str, &str, &str); 0] = [];

/// what a property found wrong, one line each, told together so one run shows them all.
struct Found {
    property: &'static str,
    problems: Vec<String>,
    /// which lines of [`HANDED_OFF`] a finding matched.
    handed: [bool; HANDED_OFF.len()],
}

impl Found {
    fn of(property: &'static str) -> Found {
        Found {
            property,
            problems: Vec::new(),
            handed: [false; HANDED_OFF.len()],
        }
    }

    fn add(&mut self, subject: &Subject, change: &str, problem: &str) {
        let handed = HANDED_OFF.iter().position(|(property, row, part, start)| {
            *property == self.property
                && *row == subject.text
                && change.contains(part)
                && problem.starts_with(start)
        });
        if let Some(line) = handed {
            self.handed[line] = true;
            return;
        }
        self.problems.push(format!(
            "{}: {}: {}",
            subject.text,
            change.escape_debug(),
            problem
        ));
    }

    fn none(mut self) {
        for (line, (property, row, part, _)) in HANDED_OFF.iter().enumerate() {
            if *property == self.property && !self.handed[line] {
                self.problems.push(format!(
                    "{row}: {part}: handed off, and no longer found: delete its line"
                ));
            }
        }
        assert!(
            self.problems.is_empty(),
            "{}: {} problems\n{}",
            self.property,
            self.problems.len(),
            self.problems.join("\n")
        );
    }
}

/// what is wrong with how an adapter ended on an answer that was not what it asked for: a
/// panic, a loop, or an error that says nothing to do.
fn misbehaved(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Panicked(message) => Some(format!("panicked: {message}")),
        Outcome::Failed(error) if error.help.is_none() => Some(format!(
            "failed without help: {:?} {}",
            error.kind, error.message
        )),
        _ => None,
    }
}

/// a size or digest `listing` states for a file that the answers as recorded stated otherwise, or
/// who may read it, which a malformed answer must never make it trust: what it cannot read
/// stays unstated, and access unknown.
fn untrusted(subject: &Subject, listing: &Listing) -> Option<String> {
    if listing.access != subject.listed.access && listing.access != super::Access::Unknown {
        return Some(format!(
            "said access is {}, where its answers said {}",
            listing.access.word(),
            subject.listed.access.word()
        ));
    }
    let before: HashMap<&str, &Remote> = subject
        .listed
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    listing.files.iter().find_map(|file| {
        let was = before.get(file.path.as_str())?;
        let differs = |now: bool, same: bool| now && !same;
        if differs(file.size.is_some(), file.size == was.size)
            || differs(file.sha256.is_some(), file.sha256 == was.sha256)
            || differs(file.md5.is_some(), file.md5 == was.md5)
        {
            Some(format!(
                "trusted {:?} for {}, recorded as {:?}",
                (
                    file.size,
                    file.sha256.map(|d| d.hex()),
                    file.md5.map(|d| d.hex())
                ),
                file.path,
                (
                    was.size,
                    was.sha256.map(|d| d.hex()),
                    was.md5.map(|d| d.hex())
                )
            ))
        } else {
            None
        }
    })
}

/// one change to one answer: what it was, and the body it made.
struct Change {
    what: String,
    body: Vec<u8>,
}

/// the pointer to each value of a json document, each array's first item standing for the rest.
fn pointers(value: &Value, at: &str, found: &mut Vec<String>) {
    found.push(at.to_owned());
    match value {
        Value::Object(members) => {
            for (key, member) in members {
                let key = key.replace('~', "~0").replace('/', "~1");
                pointers(member, &format!("{at}/{key}"), found);
            }
        }
        Value::Array(items) => {
            if let Some(first) = items.first() {
                pointers(first, &format!("{at}/0"), found);
            }
        }
        _ => {}
    }
}

/// a number written out as json can hold no other way, in place of the string it marks.
const RAW: &str = "\u{1}raw:";

/// `document` with the value at `pointer` taken out, or replaced by `value`; a string value
/// starting with [`RAW`] is written as the bare text after it.
fn changed(document: &Value, pointer: &str, value: Option<Value>) -> Vec<u8> {
    let mut document = document.clone();
    if let Some(value) = value {
        *document.pointer_mut(pointer).unwrap() = value;
    } else {
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        let key = key.replace("~1", "/").replace("~0", "~");
        match document.pointer_mut(parent).unwrap() {
            Value::Object(members) => {
                members.remove(&key);
            }
            Value::Array(items) => {
                items.remove(0);
            }
            _ => unreachable!("only containers hold values"),
        }
    }
    let text = serde_json::to_string(&document).unwrap();
    let text = text.replace("\"\\u0001raw:", "\u{1}raw:");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(at) = rest.find(RAW) {
        out.push_str(&rest[..at]);
        let after = &rest[at + RAW.len()..];
        let end = after.find('"').unwrap();
        out.push_str(&after[..end]);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out.into_bytes()
}

/// whole answers that are no answer: a sign-in page sent with 200, and json cut short, empty
/// or of the wrong kind.
fn wrong_answers(body: &[u8]) -> Vec<Change> {
    let mut changes = vec![
        Change {
            what: "a sign-in page".to_owned(),
            body: SIGN_IN.as_bytes().to_vec(),
        },
        Change {
            what: "cut in half".to_owned(),
            body: body[..body.len() / 2].to_vec(),
        },
        Change {
            what: "empty".to_owned(),
            body: Vec::new(),
        },
    ];
    for other in ["null", "{}", "[]", "\"text\"", "7"] {
        changes.push(Change {
            what: format!("the json {other}"),
            body: other.as_bytes().to_vec(),
        });
    }
    changes
}

/// each value of a json answer missing, null or of another kind.
fn wrong_values(body: &[u8]) -> Vec<Change> {
    let Ok(document) = serde_json::from_slice::<Value>(body) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    pointers(&document, "", &mut found);
    let mut changes = Vec::new();
    for pointer in found.iter().skip(1) {
        let value = document.pointer(pointer).unwrap();
        let mut instead: Vec<(&str, Option<Value>)> =
            vec![("missing", None), ("null", Some(Value::Null))];
        match value {
            Value::Object(_) => instead.push(("an array", Some(Value::Array(Vec::new())))),
            Value::Array(_) => {
                instead.push(("an object", Some(Value::Object(serde_json::Map::new()))));
            }
            // text of digits becomes the same number, as an answer may give either
            Value::String(text) => instead.push((
                "a number",
                Some(text.parse::<u64>().map_or(Value::from(7), Value::from)),
            )),
            Value::Number(number) => {
                instead.push(("its digits as text", Some(Value::from(number.to_string()))));
            }
            Value::Bool(_) => instead.push(("a number", Some(Value::from(1)))),
            Value::Null => instead.push(("text", Some(Value::from("text")))),
        }
        if !matches!(value, Value::String(_)) {
            instead.push(("text", Some(Value::from("text"))));
        }
        for (what, value) in instead {
            changes.push(Change {
                what: format!("{pointer} {what}"),
                body: changed(&document, pointer, value),
            });
        }
    }
    changes
}

/// whether `text` reads as a digest: hex of an MD5's, a SHA-1's or a SHA-256's length, or the
/// base64 of an MD5 or a SHA-256, after any `md5:` or `sha256:` before it.
fn digest_like(text: &str) -> bool {
    let digits = text.rsplit(':').next().unwrap_or(text);
    let hex =
        [32, 40, 64].contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_hexdigit());
    let base64 = [24, 44].contains(&digits.len())
        && digits.ends_with('=')
        && digits
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte));
    hex || base64
}

/// the malformed forms of a stated number or digest `text`: negative, past u64, fractional or
/// in exponent form, and a digest a digit short, a digit long or holding a letter past `f`.
fn malformed(text: &str) -> Vec<(&'static str, String)> {
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return vec![
            ("negative", "-1".to_owned()),
            ("past u64", "18446744073709551616".to_owned()),
            ("fractional", format!("{text}.5")),
            ("in exponent form", "1e3".to_owned()),
        ];
    }
    if digest_like(text) {
        let short = &text[..text.len() - 1];
        let start = text.len() - text.rsplit(':').next().unwrap_or(text).len();
        // `z` is no hex digit and `!` no base64 one
        let lettered = format!("{}z!{}", &text[..start], &text[start + 2..]);
        return vec![
            ("a digit short", short.to_owned()),
            ("a digit long", format!("{text}0")),
            ("not a digit", lettered),
        ];
    }
    Vec::new()
}

/// each number, each number written as text and each digest of an answer made malformed, in
/// json values or in the text between an xml answer's tags.
fn malformed_values(body: &[u8]) -> Vec<Change> {
    let mut changes = Vec::new();
    if let Ok(document) = serde_json::from_slice::<Value>(body) {
        let mut found = Vec::new();
        pointers(&document, "", &mut found);
        for pointer in &found {
            let value = document.pointer(pointer).unwrap();
            let forms: Vec<(&str, Value)> = match value {
                Value::Number(number) => vec![
                    ("negative", Value::from(-1)),
                    (
                        "past u64",
                        Value::from(format!("{RAW}18446744073709551616")),
                    ),
                    ("fractional", Value::from(format!("{RAW}{number}.5"))),
                    ("past f64", Value::from(format!("{RAW}1e400"))),
                ],
                Value::String(text) => malformed(text)
                    .into_iter()
                    .map(|(what, text)| (what, Value::from(text)))
                    .collect(),
                _ => Vec::new(),
            };
            for (what, value) in forms {
                changes.push(Change {
                    what: format!("{pointer} {what}"),
                    body: changed(&document, pointer, Some(value)),
                });
            }
        }
        return changes;
    }
    let text = String::from_utf8_lossy(body);
    for (at, _, inner) in texts(&text) {
        let value = inner.trim_matches('"').replace("&quot;", "");
        let value = value.trim_matches('"');
        let Some(offset) = inner.find(value) else {
            continue;
        };
        for (what, form) in malformed(value) {
            let at = at + offset;
            let body = format!("{}{form}{}", &text[..at], &text[at + value.len()..]);
            changes.push(Change {
                what: format!("{value} {what}"),
                body: body.into_bytes(),
            });
        }
    }
    changes
}

/// each text between an xml or html answer's tags that holds more than spaces, where it starts
/// and the tag before it, at most two under each tag.
fn texts(text: &str) -> Vec<(usize, &str, &str)> {
    let mut each: HashMap<&str, usize> = HashMap::new();
    let mut found = Vec::new();
    for (start, _) in text.match_indices('>') {
        let inner = &text[start + 1..];
        let Some(end) = inner.find('<') else {
            continue;
        };
        let value = &inner[..end];
        if value.trim().is_empty() {
            continue;
        }
        let open = text[..start]
            .rfind('<')
            .map_or("", |at| &text[at + 1..start]);
        let tag = open.split_whitespace().next().unwrap_or_default();
        let seen = each.entry(tag).or_default();
        if *seen < 2 {
            *seen += 1;
            found.push((start + 1, tag, value));
        }
    }
    found
}

/// `name` as it is written inside a json string, or between xml tags.
fn written(name: &str, json: bool) -> String {
    if json {
        let quoted = serde_json::to_string(name).unwrap();
        quoted[1..quoted.len() - 1].to_owned()
    } else {
        name.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
}

/// the names of up to three files a row lists that an answer's body holds.
fn names_in<'a>(subject: &'a Subject, body: &str) -> Vec<&'a str> {
    let mut names: Vec<&str> = subject
        .listed
        .paths()
        .filter_map(|path| path.rsplit('/').next())
        .filter(|name| name.len() >= 4 && body.contains(*name))
        .collect();
    names.dedup();
    names.truncate(3);
    names
}

/// the problem with a listing that holds `hostile`, written where a file's name was: a name
/// changed rather than refused, or a file left out without a warning. a name may differ from
/// `hostile` only where the answer's own links held it: `linked` has the last part of each.
fn placed(
    subject: &Subject,
    listing: &Listing,
    hostile: &str,
    linked: &[String],
) -> Option<String> {
    let before: std::collections::HashSet<&str> = subject.listed.paths().collect();
    if let Some(path) = listing.paths().find(|path| {
        let name = path.rsplit('/').next().unwrap_or(path);
        !before.contains(path)
            && path.contains("hostile")
            && !path.contains(hostile)
            && !linked.iter().any(|last| made_portable(last, name))
    }) {
        return Some(format!("listed {path:?}, not the name the answer gave"));
    }
    if crate::name::escapes(hostile).is_some()
        && listing.files.len() + listing.unavailable.len() < subject.count()
        && listing.warnings.len() <= subject.listed.warnings.len()
    {
        return Some("left a file out without refusing its name".to_owned());
    }
    None
}

/// whether `made` is `name`, a link's last part, as a file may be named on any disk: each
/// character windows refuses, a control one or one of `<>:"|?*`, as `_`, without the dots and
/// spaces windows drops at its end, and after a `_` where windows keeps the name for a device.
fn made_portable(name: &str, made: &str) -> bool {
    let name = name.trim_end_matches(['.', ' ']);
    let same = |made: &str| {
        made.chars().count() == name.chars().count()
            && made.chars().zip(name.chars()).all(|(made, was)| {
                made == was || made == '_' && (was.is_control() || "<>:\"|?*".contains(was))
            })
    };
    same(made) || made.strip_prefix('_').is_some_and(same)
}

/// whether `value` is a link: one with a scheme, or one written without it whose first part
/// names a host, as ENA's `ftp.sra.ebi.ac.uk/vol1/…`. a key such as `data/a.csv` is none.
fn is_link(value: &str) -> bool {
    value.contains("://")
        || value.split_once('/').is_some_and(|(host, _)| {
            host.contains('.')
                && host.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte)
                })
        })
}

/// `text` with the five entities xml and html name read as what they stand for.
fn unescaped(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// the last part of each link an answer holds, as the url crate reads it, decoded: each value
/// that [`is_link`], each part of one that lists several with `;`, as ENA's do, and each html
/// `href`, read against `page`, the answer's own link.
fn link_names(text: &str, page: &Url) -> Vec<String> {
    let last = |url: Url| {
        url.path_segments()
            .and_then(|mut parts| parts.next_back().map(str::to_owned))
            .map(|part| percent_decode_str(&part).decode_utf8_lossy().into_owned())
    };
    let parsed = |value: &str| {
        let value = value.trim();
        if value.contains("://") {
            Url::parse(value).ok()
        } else {
            Url::parse(&format!("https://{value}")).ok()
        }
    };
    let mut values: Vec<(String, bool)> = Vec::new();
    if let Ok(document) = serde_json::from_str::<Value>(text) {
        strings(&document, &mut |value| {
            values.push((value.to_owned(), false));
        });
    } else {
        for (token, href) in tokens(text) {
            values.push((unescaped(token), href));
        }
    }
    let mut names = Vec::new();
    for (value, href) in values {
        if href {
            names.extend(page.join(&value).ok().and_then(last));
            continue;
        }
        for part in value.split(';').filter(|part| is_link(part)) {
            names.extend(parsed(part).and_then(last));
        }
    }
    names
}

/// calls `each` with every string in a json document.
fn strings(value: &Value, each: &mut dyn FnMut(&str)) {
    match value {
        Value::String(text) => each(text),
        Value::Array(items) => items.iter().for_each(|item| strings(item, each)),
        Value::Object(members) => members.values().for_each(|member| strings(member, each)),
        _ => {}
    }
}

/// the words of an xml or html answer, between quotes, tags and spaces, each with whether it is
/// an html `href`.
fn tokens(text: &str) -> Vec<(&str, bool)> {
    let is_edge = |c: char| matches!(c, '"' | '\'' | '<' | '>') || c.is_whitespace();
    let mut found = Vec::new();
    let mut start = 0;
    for (at, c) in text.char_indices().chain([(text.len(), ' ')]) {
        if is_edge(c) {
            if at > start {
                let href = text[..start].ends_with("href=\"") || text[..start].ends_with("href='");
                found.push((&text[start..at], href));
            }
            start = at + c.len_utf8();
        }
    }
    found
}

/// `body` with `name` written as `hostile` where the answer gives it as a name, or where it is
/// part of a link, as `in_links` says; none when the answer holds it in no such place.
fn written_in(body: &str, name: &str, hostile: &str, in_links: bool) -> Option<String> {
    if let Ok(mut document) = serde_json::from_str::<Value>(body) {
        let mut changed = false;
        rewrite(&mut document, &mut |value: &mut String| {
            if value.contains(name) && is_link(value) == in_links {
                *value = value.replace(name, hostile);
                changed = true;
            }
        });
        return changed.then(|| serde_json::to_string(&document).unwrap());
    }
    // markup writes `<`, `&` and quotes as entities; a checksum list holds a name as it is
    let hostile = if is_markup(body) {
        written(hostile, false)
    } else {
        hostile.to_owned()
    };
    let mut out = String::with_capacity(body.len());
    let mut done = 0;
    let mut changed = false;
    for (token, href) in tokens(body) {
        if !token.contains(name) || (href || is_link(token)) != in_links {
            continue;
        }
        let at = token.as_ptr() as usize - body.as_ptr() as usize;
        out.push_str(&body[done..at]);
        out.push_str(&token.replace(name, &hostile));
        done = at + token.len();
        changed = true;
    }
    out.push_str(&body[done..]);
    changed.then_some(out)
}

/// whether `body` is html or xml rather than plain text.
fn is_markup(body: &str) -> bool {
    body.trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with('<')
}

/// calls `each` with every string in a json document, to change it.
fn rewrite(value: &mut Value, each: &mut dyn FnMut(&mut String)) {
    match value {
        Value::String(text) => each(text),
        Value::Array(items) => items.iter_mut().for_each(|item| rewrite(item, each)),
        Value::Object(members) => members
            .values_mut()
            .for_each(|member| rewrite(member, each)),
        _ => {}
    }
}

/// the error the network gives for an answer longer than it reads whole.
fn too_large(url: &str) -> Error {
    crate::net::too_large_answer(&Url::parse(url).unwrap())
}

/// the error the network gives for a request redirected more times than it follows.
fn redirected_too_often(url: &str) -> Error {
    Error::new(
        Kind::NetworkRefused,
        format!(
            "fetchloom will not follow {}: more than 10 redirects",
            crate::shown::Shown(&Url::parse(url).unwrap())
        ),
    )
    .help("open the link in a browser to see where it leads")
}

/// (a) a listing of more files than a run allows ends in the error that names the setting,
/// without asking more than the whole listing asks. a source may count more than files, as
/// osf's walk counts its folders and an arXiv month the mirror's objects, so one of exactly
/// that many files may be refused too.
#[test]
fn every_adapter_lists_at_most_the_files_a_run_allows() {
    let mut found = Found::of("most files");
    for subject in subjects() {
        let count = subject.count();
        // a run allows at least one file, so a listing of one has no fewer to be held to
        if count < 2 {
            continue;
        }
        let recorded = subject.hostile().most(count - 1);
        let change = format!("most {}", count - 1);
        match subject.resolve(&recorded) {
            Outcome::Failed(error)
                if error.kind == Kind::SourceInvalid
                    && error
                        .help
                        .as_deref()
                        .is_some_and(|help| help.contains("most_files")) =>
            {
                let asked = recorded.asked().len();
                if asked > subject.asks {
                    found.add(
                        &subject,
                        &change,
                        &format!(
                            "asked {asked} times, where its listing asks {}",
                            subject.asks
                        ),
                    );
                }
            }
            Outcome::Failed(error) => found.add(
                &subject,
                &change,
                &format!("failed otherwise: {:?} {}", error.kind, error.message),
            ),
            Outcome::Listed(listing) => found.add(
                &subject,
                &change,
                &format!("listed {} files", listing.files.len()),
            ),
            Outcome::Panicked(message) => {
                found.add(&subject, &change, &format!("panicked: {message}"));
            }
        }
    }
    found.none();
}

/// (a) an answer the network would not read, as one past 64 MiB or past ten redirects, ends
/// the listing in the network's own error, or leaves out only what the listing can do without,
/// and one that came from another host after redirects ends in an error with help or a listing.
#[test]
fn every_adapter_ends_in_the_error_the_network_gives() {
    let mut found = Found::of("network refusals");
    for subject in subjects() {
        for link in subject.recorded.links() {
            for error in [too_large(&link), redirected_too_often(&link)] {
                let recorded = subject.hostile().fail(&link, error.clone());
                let change = format!("{link} {}", error.message);
                match subject.resolve(&recorded) {
                    Outcome::Failed(given) if given.message == error.message => {}
                    Outcome::Listed(listing) if subject.same_files(&listing) => {}
                    Outcome::Listed(_) => {
                        found.add(&subject, &change, "listed other files without it");
                    }
                    outcome => {
                        if let Some(problem) = misbehaved(&outcome) {
                            found.add(&subject, &change, &problem);
                        }
                    }
                }
            }
            let recorded = subject
                .hostile()
                .redirected(&link, "https://example.org/elsewhere");
            if let Some(problem) = misbehaved(&subject.resolve(&recorded)) {
                found.add(&subject, &format!("{link} redirected elsewhere"), &problem);
            }
        }
    }
    found.none();
}

/// the statuses the network gives back to an adapter rather than trying again, each on the
/// answer's own body.
const STATUSES: [u16; 8] = [400, 401, 403, 404, 410, 418, 451, 501];

/// (c) an answer of the wrong shape, as a sign-in page sent with 200, json cut short or of
/// another kind, a value missing, null or of another kind, or a status the network does not
/// try again, ends in an error that says what to do, never a panic or a loop, and a sign-in
/// page is never read as a listing.
#[test]
fn every_adapter_ends_a_wrong_answer_in_an_error_with_help() {
    let mut found = Found::of("wrong answers");
    for subject in subjects() {
        for link in subject.recorded.links() {
            let body = subject.recorded.body(&link).to_vec();
            for status in STATUSES {
                let recorded = subject.hostile().answer(&link, status, body.clone());
                if let Some(problem) = misbehaved(&subject.resolve(&recorded)) {
                    found.add(&subject, &format!("{link} answered {status}"), &problem);
                }
            }
            let wrong = wrong_answers(&body).into_iter().chain(wrong_values(&body));
            for change in wrong {
                let recorded = subject.hostile().answer(&link, 200, change.body);
                let what = format!("{link} {}", change.what);
                let outcome = subject.resolve(&recorded);
                if let Some(problem) = misbehaved(&outcome) {
                    found.add(&subject, &what, &problem);
                }
                if let Outcome::Listed(listing) = &outcome {
                    if change.what == "a sign-in page" && !subject.same_files(listing) {
                        found.add(&subject, &what, "read a sign-in page as a listing");
                    }
                    if let Some(problem) = untrusted(&subject, listing) {
                        found.add(&subject, &what, &problem);
                    }
                }
            }
        }
    }
    found.none();
}

/// (d) a stated size or digest that is malformed, as a negative size, one past u64 or a digest
/// of the wrong length, is refused or left unstated, never trusted.
#[test]
fn every_adapter_reads_sizes_and_digests_strictly() {
    let mut found = Found::of("sizes and digests");
    for subject in subjects() {
        for link in subject.recorded.links() {
            let body = subject.recorded.body(&link).to_vec();
            for change in malformed_values(&body) {
                let recorded = subject.hostile().answer(&link, 200, change.body);
                let what = format!("{link} {}", change.what);
                match subject.resolve(&recorded) {
                    Outcome::Listed(listing) => {
                        if let Some(problem) = untrusted(&subject, &listing) {
                            found.add(&subject, &what, &problem);
                        }
                    }
                    outcome => {
                        if let Some(problem) = misbehaved(&outcome) {
                            found.add(&subject, &what, &problem);
                        }
                    }
                }
            }
        }
    }
    found.none();
}

/// (b) a name no file may be placed at, written in an answer where it gives a file's name, is
/// refused with an error that says what to do, or listed as written for the checks before
/// placing to refuse, never changed into another name or left out without a word. written
/// inside the answer's links instead, it may also come back as the name the links themselves
/// give, as a source that names a file by its link does.
#[test]
fn every_adapter_refuses_a_hostile_name() {
    let mut found = Found::of("hostile names");
    for subject in subjects() {
        for link in subject.recorded.links() {
            let page = Url::parse(&link).unwrap();
            let body = String::from_utf8_lossy(subject.recorded.body(&link)).into_owned();
            for name in names_in(&subject, &body) {
                for hostile in HOSTILE {
                    for in_links in [false, true] {
                        let Some(changed) = written_in(&body, name, hostile, in_links) else {
                            continue;
                        };
                        let where_ = if in_links {
                            "in its links"
                        } else {
                            "as a name"
                        };
                        let what = format!("{link} {name} as {hostile} {where_}");
                        let linked = if in_links {
                            link_names(&changed, &page)
                        } else {
                            Vec::new()
                        };
                        let recorded = subject.hostile().answer(&link, 200, changed.into_bytes());
                        let outcome = subject.resolve(&recorded);
                        if let Some(problem) = misbehaved(&outcome) {
                            found.add(&subject, &what, &problem);
                        }
                        if let Outcome::Listed(listing) = &outcome {
                            assert!(
                                listing
                                    .paths()
                                    .all(|path| crate::name::escapes(path).is_none())
                            );
                            if let Some(problem) = placed(&subject, listing, hostile, &linked) {
                                found.add(&subject, &what, &problem);
                            }
                        }
                    }
                }
            }
        }
    }
    found.none();
}

/// the values an answer may name a page or a folder by: each recorded link, each value in their
/// queries, each id inside a value, as a graphql query names a folder's, and each part of their
/// paths long enough to be an id, and one no page has.
fn page_names(subject: &Subject) -> Vec<String> {
    let mut names = vec!["zz-loop".to_owned()];
    for link in subject.recorded.links() {
        let url = Url::parse(&link).unwrap();
        for (_, value) in url.query_pairs() {
            names.extend(
                value
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
                    .filter(|id| id.len() >= 16)
                    .map(str::to_owned),
            );
            names.push(value.into_owned());
        }
        names.extend(
            url.path_segments()
                .into_iter()
                .flatten()
                .filter(|part| part.len() >= 8)
                .map(str::to_owned),
        );
        names.push(link);
    }
    names.sort_unstable();
    names.dedup();
    names
}

/// the link's answer made to name a page of its listing again, each on a server that sends
/// the first page of a link's path for any page it lacks: by another page's link, token or id
/// in its place, as its own next page, with a count past any page, and in its json values or
/// the xml and html text that names a next page.
fn page_loops(subject: &Subject, link: &str, names: &[String]) -> Vec<(String, Recorded)> {
    let body = subject.recorded.body(link).to_vec();
    let text = String::from_utf8_lossy(&body).into_owned();
    let mut changes: Vec<(String, Recorded)> = names
        .iter()
        .filter(|name| name.len() >= 4 && text.contains(name.as_str()))
        .flat_map(|name| {
            names
                .iter()
                .filter(move |other| *other != name)
                .map(move |other| (name, other))
        })
        .map(|(name, other)| {
            let body = text.replace(name.as_str(), other).into_bytes();
            (
                format!("{link} names {other} for {name}"),
                subject.hostile().echo().answer(link, 200, body),
            )
        })
        .collect();
    changes.extend([
        (
            format!("{link} names itself next"),
            subject.hostile().echo().next(link, link),
        ),
        (
            format!("{link} counts a billion items"),
            subject.hostile().echo().total(link, 1_000_000_000),
        ),
    ]);
    if let Ok(document) = serde_json::from_slice::<Value>(&body) {
        let mut nodes = Vec::new();
        pointers(&document, "", &mut nodes);
        for pointer in &nodes {
            let instead: Vec<Value> = match document.pointer(pointer).unwrap() {
                Value::String(_) => names
                    .iter()
                    .map(|name| Value::from(name.as_str()))
                    .collect(),
                Value::Number(_) => vec![Value::from(1_000_000_000_u64)],
                _ => Vec::new(),
            };
            for value in instead {
                let what = format!("{link} {pointer} as {value}");
                let body = changed(&document, pointer, Some(value));
                changes.push((what, subject.hostile().echo().answer(link, 200, body)));
            }
        }
    } else {
        for (at, tag, value) in texts(&text) {
            let tag = tag.to_ascii_lowercase();
            let names_a_page = ["next", "token", "marker", "truncated"]
                .iter()
                .any(|word| tag.contains(word));
            let mut instead: Vec<String> = Vec::new();
            if names_a_page {
                instead.extend(names.iter().map(|name| written(name, false)));
            }
            if value.bytes().all(|byte| byte.is_ascii_digit()) {
                instead.push("1000000000".to_owned());
            }
            for name in instead {
                let body = format!("{}{name}{}", &text[..at], &text[at + value.len()..]);
                let what = format!("{link} <{tag}> {value:?} as {name}");
                changes.push((
                    what,
                    subject
                        .hostile()
                        .echo()
                        .answer(link, 200, body.into_bytes()),
                ));
            }
        }
        for (at, _) in text.match_indices("href=\"") {
            let start = at + "href=\"".len();
            let Some(end) = text[start..].find('"') else {
                continue;
            };
            for name in names {
                let body = format!(
                    "{}{}{}",
                    &text[..start],
                    written(name, false),
                    &text[start + end..]
                );
                let what = format!("{link} href {:?} as {name}", &text[start..start + end]);
                changes.push((
                    what,
                    subject
                        .hostile()
                        .echo()
                        .answer(link, 200, body.into_bytes()),
                ));
            }
        }
    }
    changes
}

/// (a) a page that names itself or a page before it, by link, by token or by a count past
/// any page the source has, on a server that sends its first page for any page it lacks, ends
/// the listing, never a loop.
#[test]
fn every_adapter_stops_at_a_page_it_was_sent_before() {
    let mut found = Found::of("pages");
    for subject in subjects() {
        let names = page_names(&subject);
        for link in subject.recorded.links() {
            let changes = page_loops(&subject, &link, &names);
            for (what, recorded) in changes {
                if let Some(problem) = misbehaved(&subject.resolve(&recorded)) {
                    found.add(&subject, &what, &problem);
                }
            }
        }
    }
    found.none();
}

/// (e) keys have readers: every source a reference names is a built-in one, each key goes only
/// to the hosts its own source asks or links its files at, never to a host another source's
/// resolver asks, and each account's key is the one its source's descriptor names.
#[test]
fn every_key_is_read_by_its_own_source_alone() {
    let built_in = |access: &Access| ALL.iter().any(|known| std::ptr::eq(*known, access));
    for subject in subjects() {
        assert!(built_in(subject.reference.access()), "{}", subject.text);
        assert!(
            built_in(subject.listed.resolved.access()),
            "{}",
            subject.text
        );
    }
    let at = |host: &str| Url::parse(&format!("https://{host}/")).unwrap();
    for access in ALL {
        let Some(key) = access.key else {
            continue;
        };
        if let Some(host) = key.host {
            assert!(
                access.api.holds(&at(host), None),
                "{} key to {host}",
                access.name
            );
        }
        for host in key.every_hop {
            let url = at(host);
            assert!(
                access.api.holds(&url, None) || access.files.holds(&url, None),
                "{} key to {host}",
                access.name
            );
        }
        for other in ALL.iter().filter(|other| !std::ptr::eq(**other, access)) {
            let Origins::Fixed(_) = other.api else {
                continue;
            };
            for host in key.host.iter().chain(key.every_hop) {
                assert!(
                    !other.api.holds(&at(host), None),
                    "{} asks {host}, where {}'s key goes",
                    other.name,
                    access.name
                );
            }
        }
    }
    for account in Account::SOURCES {
        let access = access::of_account(&account).unwrap();
        assert!(built_in(access), "{}", access.name);
        assert!(
            account
                .key()
                .zip(access.key)
                .is_some_and(|(key, named)| std::ptr::eq(key, named)),
            "{}",
            access.name
        );
    }
}

/// links an answer may give for a file that no file may be fetched from.
const HOSTILE_LINKS: [&str; 6] = [
    "ftp://example.org/hostile.csv",
    "file:///etc/passwd",
    "javascript:alert(1)",
    "http://169.254.169.254/latest/meta-data/",
    "https://user:secret@example.org/hostile.csv",
    "https://example.org:8443/hostile.csv",
];

/// (e) a link an answer gives for a file, made one no file may come from, ends in an error with
/// help, or a listing whose every link is http or https, carries no login and lies where its
/// source's files are kept.
#[test]
fn every_adapter_refuses_a_hostile_link() {
    let mut found = Found::of("hostile links");
    for subject in subjects() {
        for link in subject.recorded.links() {
            let body = String::from_utf8_lossy(subject.recorded.body(&link)).into_owned();
            let given: Vec<&str> = subject
                .listed
                .files
                .iter()
                .map(|file| file.url.as_str())
                .filter(|url| body.contains(url))
                .take(2)
                .collect();
            for url in given {
                for hostile in HOSTILE_LINKS {
                    let changed = body.replace(url, hostile);
                    let recorded = subject.hostile().answer(&link, 200, changed.into_bytes());
                    let what = format!("{link} {url} as {hostile}");
                    let outcome = subject.resolve(&recorded);
                    if let Some(problem) = misbehaved(&outcome) {
                        found.add(&subject, &what, &problem);
                    }
                    if let Outcome::Listed(listing) = &outcome {
                        let from = listing.resolved.access();
                        let written = listing.resolved.written_origin();
                        if let Some(file) = listing.files.iter().find(|file| {
                            !matches!(file.url.scheme(), "http" | "https")
                                || !file.url.username().is_empty()
                                || file.url.password().is_some()
                                || !from.files.holds(&file.url, written.as_ref())
                        }) {
                            found.add(
                                &subject,
                                &what,
                                &format!("listed {} at {}", file.path, file.url),
                            );
                        }
                    }
                }
            }
        }
    }
    found.none();
}
