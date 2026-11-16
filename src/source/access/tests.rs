use super::*;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Listing, Reference, Remote};

/// a source's network, as one of its tests recorded it, with the reference it resolves.
struct Case {
    reference: &'static str,
    recorded: Recorded,
}

fn case(reference: &'static str, answers: &[(&str, &str)]) -> Case {
    let recorded = answers
        .iter()
        .fold(Recorded::default(), |recorded, (url, fixture)| {
            recorded.fixture(url, fixture)
        });
    Case {
        reference,
        recorded,
    }
}

/// a case for every source that asks anything, each answered as its own tests record it.
#[expect(clippy::too_many_lines, reason = "a case for each source")]
fn every_source() -> Vec<Case> {
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
    let mut cases = vec![
        case(
            "zenodo:3608990",
            &[("https://zenodo.org/api/records/3608990", "zenodo/iris.json")],
        ),
        case(
            "doi:10.5281/zenodo.3608990",
            &[
                (
                    "https://doi.org/api/handles/10.5281%2Fzenodo.3608990",
                    "doi/zenodo.json",
                ),
                ("https://zenodo.org/api/records/3608990", "zenodo/iris.json"),
            ],
        ),
        case(
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
        case(
            "github:jqlang/jq@jq-1.8.2",
            &[(
                "https://api.github.com/repos/jqlang/jq/releases/tags/jq-1.8.2",
                "github/release.json",
            )],
        ),
        case(
            "gitlab:gitlab-org/gitlab-runner@v19.3.6",
            &[(
                "https://gitlab.com/api/v4/projects/gitlab-org%2Fgitlab-runner/releases/v19.3.6",
                "gitlab/release.json",
            )],
        ),
        case(
            "gitlab://code.videolan.org/videolan/dav1d@1.5.3",
            &[(
                "https://code.videolan.org/api/v4/projects/videolan%2Fdav1d/releases/1.5.3",
                "gitlab/dav1d-release.json",
            )],
        ),
        case(
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
        case(
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
        case(
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
        case(
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
        case(
            "figshare:34744689",
            &[(
                "https://api.figshare.com/v2/articles/34744689",
                "figshare/article.json",
            )],
        ),
        case(
            "ia:iris_20240626",
            &[("https://archive.org/metadata/iris_20240626", "ia/iris.json")],
        ),
        case(
            "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP",
            &[(
                "https://dataverse.harvard.edu/api/datasets/:persistentId/versions/:latest-published?persistentId=doi%3A10.7910%2FDVN%2FTJCLKP",
                "dataverse/version.json",
            )],
        ),
        case(
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
        case(
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
        case(
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
        case(
            "arxiv:1706.03762",
            &[(paper, "arxiv/paper.xml"), (license, "arxiv/license.xml")],
        ),
        case(
            "s2:ARXIV:1706.03762",
            &[
                (&s2_paper, "s2/paper-arxiv.json"),
                (paper, "arxiv/paper.xml"),
                (license, "arxiv/license.xml"),
            ],
        ),
        case(
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
        case("https://www.dropbox.com/s/abc123/iris.csv?dl=0", &[]),
        case("https://example.org/iris.csv", &[]),
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
    cases.push(Case {
        reference: "kaggle:competitions/titanic",
        recorded: competition,
    });
    let open = "https://drive.google.com/open?id=1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb";
    cases.push(Case {
        reference: open,
        recorded: Recorded::default()
            .fixture(open, "drive/open-file.html")
            .redirected(
                open,
                "https://drive.google.com/file/d/1cKSdgtWrPgvEsBGmjWALOH33taGyVXKb/view",
            ),
    });
    for case in &mut cases {
        if case.reference.contains("1706.03762") {
            case.recorded =
                std::mem::take(&mut case.recorded).on(mirror, 200, held.as_bytes().to_vec());
        }
    }
    let mendeley = "https://data.mendeley.com/public-api/datasets/rscbjbr9sj";
    let root = format!("{mendeley}/files?folder_id=root&version=2&$start=0");
    cases.push(Case {
        reference: "mendeley:rscbjbr9sj@2",
        recorded: Recorded::default()
            .fixture(&format!("{mendeley}?version=2"), "mendeley/dataset.json")
            .fixture(&format!("{mendeley}/folders/2"), "mendeley/folders.json")
            .fixture(&root, "mendeley/files.json")
            .total(&root, 3),
    });
    cases
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
    let cases = every_source();
    let mut sources: Vec<&str> = cases
        .iter()
        .map(|case| {
            Reference::parse(case.reference)
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
        .filter(|name| !["cloud storage", "imported files"].contains(name))
        .collect();
    for name in &every {
        assert!(sources.contains(name), "no case asks {name}");
    }
    for case in cases {
        let reference = Reference::parse(case.reference).unwrap().reference;
        let listing = block_on(reference.resolve(&case.recorded))
            .unwrap_or_else(|error| panic!("{}: {}", case.reference, error.message));
        assert!(!listing.files.is_empty(), "{}", case.reference);
        for asked in case.recorded.asked() {
            let url = Url::parse(asked.split(' ').next().unwrap()).unwrap();
            assert!(
                asked_inside(&reference, &listing, &url),
                "{} asked {url}",
                case.reference
            );
        }
        let from = listing.resolved.access();
        let written = listing.resolved.written_origin();
        for Remote { url, path, .. } in &listing.files {
            assert!(
                from.files.holds(url, written.as_ref()),
                "{} links {path} at {url}",
                case.reference
            );
        }
    }
}

/// a resolver's request to an origin its source does not name is refused before it goes out.
#[test]
fn a_request_outside_the_descriptor_never_goes_out() {
    let recorded = Recorded::default().on("https://zenodo.org/api/records/1", 200, b"{}".to_vec());
    let scoped = Scoped::new(&recorded, &ZENODO, None, "zenodo:1".to_owned());
    for elsewhere in [
        "https://example.org/api/records/1",
        "http://zenodo.org/api/records/1",
        "https://zenodo.org:8443/api/records/1",
        "https://api.github.com/repos/org/private/contents/",
    ] {
        let url = Url::parse(elsewhere).unwrap();
        let error = block_on(scoped.get(&url)).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkRefused, "{elsewhere}");
        assert!(error.message.contains("zenodo:1"), "{}", error.message);
        assert!(block_on(scoped.get_once(&url)).is_err(), "{elsewhere}");
        assert!(block_on(scoped.get_with(&url, &[])).is_err(), "{elsewhere}");
    }
    assert!(recorded.asked().is_empty(), "{:?}", recorded.asked());
    let own = Url::parse("https://zenodo.org/api/records/1").unwrap();
    assert_eq!(block_on(scoped.get(&own)).unwrap().status, 200);
}

/// a written origin is the reference's own, at its scheme, host and port alone; a fixed one with
/// `*` takes the hosts below its name and no other.
#[test]
fn origins_hold_their_own_scheme_host_and_port() {
    let url = |text: &str| Url::parse(text).unwrap();
    let written = url("https://dv.example.org:8443/").origin();
    for (asked, held) in [
        ("https://dv.example.org:8443/api/users/:me", true),
        ("https://dv.example.org/api/users/:me", false),
        ("http://dv.example.org:8443/api/users/:me", false),
        ("https://other.example.org:8443/api", false),
    ] {
        assert_eq!(
            Origins::Written.holds(&url(asked), Some(&written)),
            held,
            "{asked}"
        );
    }
    assert!(!Origins::Written.holds(&url("https://dv.example.org:8443/"), None));
    for (asked, held) in [
        ("https://ftp.sra.ebi.ac.uk/vol1/a.fastq.gz", true),
        ("https://ebi.ac.uk/a", true),
        ("https://deep.ftp.ebi.ac.uk/a", true),
        ("https://evilebi.ac.uk/a", false),
        ("https://ebi.ac.uk.example.org/a", false),
        ("http://ftp.sra.ebi.ac.uk/a", false),
        ("https://ftp.sra.ebi.ac.uk:8443/a", false),
    ] {
        assert_eq!(ENA.files.holds(&url(asked), None), held, "{asked}");
    }
}

/// a listing whose file links where its source keeps no files is refused, whichever way the link
/// got there.
#[test]
fn a_file_linked_outside_its_sources_files_is_refused() {
    let reference = Reference::parse("zenodo:3608990").unwrap().reference;
    let listing = |link: &str| Listing {
        resolved: reference.clone(),
        title: None,
        license: None,
        doi: None,
        access: crate::source::Access::Public,
        files: vec![Remote {
            path: "iris.csv".to_owned(),
            size: None,
            sha256: None,
            md5: None,
            url: Url::parse(link).unwrap(),
            named_by_server: false,
            etag: None,
        }],
        unavailable: Vec::new(),
        warnings: Vec::new(),
        within: Vec::new(),
    };
    let kept = "https://zenodo.org/api/records/3608990/files/iris.csv/content";
    assert!(crate::source::check(&reference, listing(kept), 10).is_ok());
    let error =
        crate::source::check(&reference, listing("https://example.org/iris.csv"), 10).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(
        error.message.contains(
            "links \"iris.csv\" at https://example.org, where zenodo's files are not kept"
        ),
        "{}",
        error.message
    );
}

/// a refusal from a source's own host names its account, and only from where its key goes or
/// its own site, so a sign-in it offers can lift it.
#[test]
fn a_refusal_names_the_account_whose_key_reaches_its_host() {
    for (host, account) in [
        ("huggingface.co", Some(Account::HuggingFace)),
        ("www.huggingface.co", Some(Account::HuggingFace)),
        ("hf.co", Some(Account::HuggingFace)),
        ("api.kaggle.com", Some(Account::Kaggle)),
        ("www.kaggle.com", Some(Account::Kaggle)),
        ("ndownloader.figshare.com", Some(Account::Figshare)),
        ("files.osf.io", Some(Account::Osf)),
        ("api.semanticscholar.org", Some(Account::S2)),
        ("github.com", None),
        ("storage.googleapis.com", None),
        ("export.arxiv.org", None),
        ("example.org", None),
    ] {
        assert_eq!(account_at(host), account, "{host}");
    }
}

/// a source's key goes to its own api, the first origin its resolver asks, or for an
/// installation named by its reference, to that installation alone.
#[test]
fn every_key_goes_to_its_sources_own_api() {
    for access in ALL {
        let Some(key) = access.key else {
            continue;
        };
        match key.host {
            Some(host) => assert_eq!(access.api.hosts().next(), Some(host), "{}", access.name),
            None => assert!(matches!(access.api, Origins::Written), "{}", access.name),
        }
    }
}
