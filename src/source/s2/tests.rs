//! tests on semantic scholar's answers, recorded on 2026-11-13: papers and a search from its
//! graph API, and a release and the links to a dataset's files from its datasets API, these with
//! a placeholder in place of each signature and a far expiry in place of a week's.

use std::time::UNIX_EPOCH;

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Access, Listing};

const FIELDS: &str =
    "fields=paperId%2CcorpusId%2CexternalIds%2Curl%2Ctitle%2Cyear%2Cauthors%2CopenAccessPdf";

/// where semantic scholar's API answers about the paper `id`.
fn asked(id: &str) -> String {
    format!("https://api.semanticscholar.org/graph/v1/paper/{id}?{FIELDS}")
}

fn reference(text: &str) -> String {
    Reference::parse(text).unwrap().reference.to_string()
}

fn resolve(recorded: &Recorded, text: &str) -> Result<Listing, Error> {
    block_on(Reference::parse(text).unwrap().reference.resolve(recorded))
}

#[test]
fn a_paper_is_named_by_any_id_semantic_scholars_api_takes() {
    for (text, canonical) in [
        (
            "s2:204E3073870FAE3D05BCBC2F6A8E263D9B72E776",
            "s2:204e3073870fae3d05bcbc2f6a8e263d9b72e776",
        ),
        (
            "s2:doi:10.1371/Journal.PCBI.1003285",
            "s2:DOI:10.1371/journal.pcbi.1003285",
        ),
        ("s2:arxiv:1706.03762", "s2:ARXIV:1706.03762"),
        ("s2:ARXIV:hep-th/9901001", "s2:ARXIV:hep-th/9901001"),
        ("s2:corpusid:13756489", "s2:CorpusId:13756489"),
        ("s2:PMCID:PMC3812051", "s2:PMCID:3812051"),
        ("s2:pmid:24204232", "s2:PMID:24204232"),
        ("s2:MAG:2963403868", "s2:MAG:2963403868"),
        ("s2:acl:W12-3903", "s2:ACL:W12-3903"),
        (
            "https://www.semanticscholar.org/paper/Attention-is-All-you-Need-Vaswani-Shazeer/204e3073870fae3d05bcbc2f6a8e263d9b72e776",
            "s2:204e3073870fae3d05bcbc2f6a8e263d9b72e776",
        ),
        (
            "https://www.semanticscholar.org/paper/204e3073870fae3d05bcbc2f6a8e263d9b72e776",
            "s2:204e3073870fae3d05bcbc2f6a8e263d9b72e776",
        ),
        (
            "https://api.semanticscholar.org/graph/v1/paper/DOI:10.1371/journal.pcbi.1003285",
            "s2:DOI:10.1371/journal.pcbi.1003285",
        ),
    ] {
        assert_eq!(reference(text), canonical, "{text}");
        assert_eq!(reference(canonical), canonical, "{canonical} reads back");
    }
}

#[test]
fn malformed_ids_and_unknown_links_are_refused() {
    for bad in [
        "s2:",
        "s2:204e3073870fae3d05bcbc2f6a8e263d9b72e77",
        "s2:DOI:abc",
        "s2:DOI:10.1371",
        "s2:ARXIV:1706.03762v7",
        "s2:CorpusId:12a",
        "s2:PMID:",
        "s2:ISBN:9780262035613",
        "https://www.semanticscholar.org/search?q=attention",
    ] {
        let error = Reference::parse(bad).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{bad}");
    }
    let error = Reference::parse("s2:DOI:abc").unwrap_err();
    assert!(error.help.unwrap().contains("s2:DOI:<doi>"));
    // a citation's arXiv id holds its version, which semantic scholar does not take
    let error = Reference::parse("s2:arxiv:1706.03762v7").unwrap_err();
    assert_eq!(
        error.help.as_deref(),
        Some(
            "semantic scholar knows an arXiv paper by its id without a version, as s2:ARXIV:1706.03762; add that version from arXiv as arxiv:1706.03762@v7"
        )
    );
    let error = Reference::parse("https://www.semanticscholar.org/search?q=attention").unwrap_err();
    assert_eq!(
        error.help.as_deref(),
        Some(
            "paste a paper's page, as https://www.semanticscholar.org/paper/12b056fd90c1300c44c4a8cff9f01f0c7a75cab4, or write its id, as s2:DOI:10.1371/journal.pcbi.1003285"
        )
    );
}

/// semantic scholar links arXiv's pdf of an arXiv paper, which comes through arXiv's own source:
/// at its newest version, from the mirror or else from export.arxiv.org, at arXiv's pace.
#[test]
fn an_arxiv_paper_comes_through_arxiv_at_its_newest_version() {
    let paper = "https://export.arxiv.org/api/query?id_list=1706.03762";
    let license = "https://oaipmh.arxiv.org/oai?verb=GetRecord&identifier=oai%3AarXiv.org%3A1706.03762&metadataPrefix=arXivRaw";
    let mirror = "https://storage.googleapis.com/storage/v1/b/arxiv-dataset/o?prefix=arxiv%2Farxiv%2Fpdf%2F1706%2F1706.03762v7.pdf&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken";
    let recorded = Recorded::default()
        .fixture(&asked("ARXIV:1706.03762"), "s2/paper-arxiv.json")
        .fixture(paper, "arxiv/paper.xml")
        .fixture(license, "arxiv/license.xml")
        .on(mirror, 200, b"{}".to_vec());
    let listing = resolve(&recorded, "s2:ARXIV:1706.03762").unwrap();
    assert_eq!(listing.resolved.to_string(), "arxiv:1706.03762@v7");
    assert_eq!(listing.title.as_deref(), Some("Attention Is All You Need"));
    assert_eq!(
        listing.files[0].url.as_str(),
        "https://export.arxiv.org/pdf/1706.03762v7"
    );
    assert_eq!(recorded.asked()[1], paper);
}

/// any other paper is pinned at the pdf semantic scholar links, with its license in the words the
/// license check reads, and named after its DOI.
#[test]
fn an_open_access_paper_is_pinned_at_the_pdf_semantic_scholar_links() {
    let recorded = Recorded::default().fixture(
        &asked("DOI:10.1371/journal.pcbi.1003285"),
        "s2/paper-open.json",
    );
    let listing = resolve(&recorded, "s2:DOI:10.1371/journal.pcbi.1003285").unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "s2:12b056fd90c1300c44c4a8cff9f01f0c7a75cab4"
    );
    assert_eq!(
        listing.title.as_deref(),
        Some("Ten Simple Rules for Reproducible Computational Research")
    );
    assert_eq!(listing.license.as_deref(), Some("CC-BY"));
    assert_eq!(listing.doi.as_deref(), Some("10.1371/journal.pcbi.1003285"));
    assert_eq!(listing.access, Access::Public);
    let [file] = listing.files.as_slice() else {
        panic!("{:?}", listing.files);
    };
    assert_eq!(file.path, "journal.pcbi.1003285.pdf");
    assert_eq!(
        file.url.as_str(),
        "https://journals.plos.org/ploscompbiol/article/file?id=10.1371/journal.pcbi.1003285&type=printable"
    );
    assert_eq!((file.size, file.sha256, file.md5), (None, None, None));
    assert!(!listing.names_its_file());
}

#[test]
fn semantic_scholars_licenses_read_as_the_license_check_knows_them() {
    for (said, named) in [
        ("CCBY", "CC-BY"),
        ("CCBYNC", "CC-BY-NC"),
        ("CCBYNCND", "CC-BY-NC-ND"),
        ("CCBYNCSA", "CC-BY-NC-SA"),
        ("ccbysa", "CC-BY-SA"),
        ("CCBYND", "CC-BY-ND"),
        ("CC0", "CC0-1.0"),
        ("CCBYXY", "CCBYXY"),
        ("publisher-specific-oa", "publisher-specific-oa"),
    ] {
        assert_eq!(paper::license(said), named, "{said}");
    }
}

/// a paper without a pdf link is refused with its page, and a DOI's link with the page it leads
/// to, since neither is the paper's file.
#[test]
fn a_paper_without_an_open_pdf_is_refused_naming_where_to_read_it() {
    let closed = "s2:240bf8044739451452d2c30780d7d9f031d2d89c";
    let recorded = Recorded::default().fixture(
        &asked("240bf8044739451452d2c30780d7d9f031d2d89c"),
        "s2/paper-closed.json",
    );
    let error = resolve(&recorded, closed).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        format!("semantic scholar links no open-access pdf of \"Deep learning\" ({closed})")
    );
    assert!(error.help.as_deref().unwrap().contains(
        "https://www.semanticscholar.org/paper/240bf8044739451452d2c30780d7d9f031d2d89c"
    ));
    let mut answer: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("s2/paper-closed.json")).unwrap();
    answer["openAccessPdf"]["url"] = "https://doi.org/10.1609/aaai.v34i07.6693".into();
    let recorded = Recorded::default().on(
        &asked("240bf8044739451452d2c30780d7d9f031d2d89c"),
        200,
        serde_json::to_vec(&answer).unwrap(),
    );
    let error = resolve(&recorded, closed).unwrap_err();
    assert!(
        error
            .help
            .as_deref()
            .unwrap()
            .contains("https://doi.org/10.1609/aaai.v34i07.6693, which is no pdf"),
        "{error:?}"
    );
}

#[test]
fn a_missing_paper_a_refused_key_and_a_broken_answer_say_what_went_wrong() {
    let id = "DOI:10.1038/nature14539";
    let missing = Recorded::default().on(&asked(id), 404, fixture_bytes("s2/missing.json"));
    let error = resolve(&missing, &format!("s2:{id}")).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "semantic scholar knows no paper s2:DOI:10.1038/nature14539"
    );
    assert!(error.help.unwrap().contains("--source s2"));
    let refused = Recorded::default().on(&asked(id), 403, br#"{"message":"Forbidden"}"#.to_vec());
    let error = resolve(&refused, &format!("s2:{id}")).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.need.as_deref().map(|need| &need.account),
        Some(&Account::S2)
    );
    let broken = Recorded::default().on(&asked(id), 200, b"{\"paperId\":".to_vec());
    let error = resolve(&broken, &format!("s2:{id}")).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

/// a refusal for load, after the network's own waits, suggests a key, whose rate is its own.
#[test]
fn a_busy_api_suggests_a_key_and_other_errors_stay_as_they_are() {
    let limited = busy(Error::new(Kind::NetworkRateLimited, "gave up"));
    let help = limited.help.unwrap();
    assert!(help.contains(S2_KEYS), "{help}");
    assert!(help.contains("`fetchloom auth login s2`"), "{help}");
    let other = busy(Error::new(Kind::NetworkStatus, "answered 500").help("later"));
    assert_eq!(other.help.as_deref(), Some("later"));
}

#[test]
fn requests_go_one_at_a_time_a_second_apart_with_a_key_and_three_without() {
    assert_eq!(spaced(true), Duration::from_secs(1));
    assert_eq!(spaced(false), Duration::from_secs(3));
    assert_eq!(spacing(S2_HOST), Some(Duration::from_secs(3)));
    assert_eq!(most_at_once(S2_HOST), Some(1));
    assert_eq!(spacing("www.semanticscholar.org"), None);
}

/// the search finds the paper first, offers it by its corpus id and tells papers add would refuse.
#[test]
fn a_search_offers_papers_by_corpus_id_and_tells_those_without_a_pdf() {
    let url = format!(
        "https://api.semanticscholar.org/graph/v1/paper/search?query=attention+is+all+you+need&limit=5&{FIELDS}"
    );
    let recorded = Recorded::default().fixture(&url, "s2/search.json");
    let words = ["attention", "is", "all", "you", "need"].map(str::to_owned);
    let found = block_on(search(&recorded, &words, 5)).unwrap();
    assert_eq!(found.len(), 5);
    assert_eq!(found[0].reference.to_string(), "s2:CorpusId:13756489");
    assert_eq!(found[0].title, "Attention is All you Need");
    assert_eq!(found[0].byline.as_deref(), Some("Vaswani et al. 2017"));
    assert_eq!(found[0].note, None);
    let refused: Vec<&str> = found
        .iter()
        .filter(|found| found.note.as_deref() == Some("no open-access pdf"))
        .map(|found| found.title.as_str())
        .collect();
    assert_eq!(
        refused,
        ["Channel Attention Is All You Need for Video Frame Interpolation"]
    );
    assert!(block_on(search(&recorded, &[], 5)).unwrap().is_empty());
}

const RELEASE: &str = "https://api.semanticscholar.org/datasets/v1/release/latest";
const LINKS: &str = "https://api.semanticscholar.org/datasets/v1/release/2026-11-10/dataset/tldrs";
/// the first file of the recorded dataset, by the name its signed links share.
const FIRST: &str = "20261113_085251_00187_5yhyg_01e7a63d-0ddd-4f4d-956b-98f2199bbd92.gz";

#[test]
fn a_dataset_is_named_with_or_without_its_release() {
    for (text, canonical) in [
        ("s2:papers", "s2:papers"),
        ("s2:Papers@2026-11-10", "s2:papers@2026-11-10"),
        ("s2:embeddings-specter_v2", "s2:embeddings-specter_v2"),
        (
            "https://api.semanticscholar.org/datasets/v1/release/2026-11-10/dataset/tldrs",
            "s2:tldrs@2026-11-10",
        ),
        (
            "https://api.semanticscholar.org/datasets/v1/release/latest/dataset/tldrs",
            "s2:tldrs",
        ),
    ] {
        assert_eq!(reference(text), canonical, "{text}");
        assert_eq!(reference(canonical), canonical, "{canonical} reads back");
    }
    for bad in [
        "s2:papers@latest",
        "s2:papers@2026-1-10",
        "s2:-papers",
        "s2:pa/pers",
        "s2:papers@",
    ] {
        let error = Reference::parse(bad).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{bad}");
    }
}

/// the release names the dataset and its license to anyone; the files come from the links the
/// API gives a key, each kept in data.lock as the listing with the file's name, never a link
/// that holds a signature.
#[test]
fn a_dataset_is_pinned_at_the_newest_release_by_its_files_names() {
    let recorded = Recorded::default()
        .fixture(RELEASE, "s2/release.json")
        .fixture(LINKS, "s2/dataset.json");
    let listing = resolve(&recorded, "s2:tldrs").unwrap();
    assert_eq!(listing.resolved.to_string(), "s2:tldrs@2026-11-10");
    assert_eq!(
        listing.title.as_deref(),
        Some("A short natural-language summary of the contents of a paper.")
    );
    assert_eq!(listing.license.as_deref(), Some("ODC-By-1.0"));
    assert_eq!(listing.access, Access::Login);
    assert_eq!(listing.naming_title(), None);
    assert!(!listing.names_its_file());
    assert_eq!(listing.files.len(), 30);
    let first = &listing.files[0];
    assert_eq!(first.path, FIRST);
    assert_eq!(first.url.as_str(), format!("{LINKS}#{FIRST}"));
    assert_eq!((first.size, first.sha256, first.md5), (None, None, None));
    assert!(listing.files.iter().all(|file| file.url.query().is_none()));
    assert_eq!(recorded.asked(), [RELEASE, LINKS]);
}

#[test]
fn an_embeddings_dataset_takes_the_license_its_readme_names() {
    let recorded = Recorded::default()
        .fixture(
            "https://api.semanticscholar.org/datasets/v1/release/2026-11-10",
            "s2/release.json",
        )
        .fixture(
            "https://api.semanticscholar.org/datasets/v1/release/2026-11-10/dataset/embeddings-specter_v2",
            "s2/dataset.json",
        );
    let listing = resolve(&recorded, "s2:embeddings-specter_v2@2026-11-10").unwrap();
    assert_eq!(listing.license.as_deref(), Some("Apache-2.0"));
    assert_eq!(
        listing.resolved.to_string(),
        "s2:embeddings-specter_v2@2026-11-10"
    );
}

/// without a key the release still answers, so the refusal comes once the name is known good,
/// and names the key's page and the sign-in.
#[test]
fn a_datasets_files_without_a_key_are_refused_naming_the_sign_in() {
    let recorded = Recorded::default().fixture(RELEASE, "s2/release.json").on(
        LINKS,
        401,
        br#"{"error":"A valid API key is required"}"#.to_vec(),
    );
    let error = resolve(&recorded, "s2:tldrs").unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "semantic scholar gives the files of s2:tldrs@2026-11-10 only for an API key"
    );
    let help = error.help.as_deref().unwrap();
    assert!(
        help.contains(S2_KEYS) && help.contains("`fetchloom auth login s2`"),
        "{help}"
    );
    assert_eq!(
        error.need.as_deref().map(|need| &need.account),
        Some(&Account::S2)
    );
}

#[test]
fn an_unknown_release_or_dataset_is_refused_with_what_there_is() {
    let recorded = Recorded::default().on(
        "https://api.semanticscholar.org/datasets/v1/release/2000-01-01",
        404,
        br#"{"error":"No release with ID 2000-01-01"}"#.to_vec(),
    );
    let error = resolve(&recorded, "s2:papers@2000-01-01").unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(error.message, "semantic scholar has no release 2000-01-01");
    assert!(
        error
            .help
            .unwrap()
            .contains("https://api.semanticscholar.org/datasets/v1/release/")
    );
    let recorded = Recorded::default().fixture(RELEASE, "s2/release.json");
    let error = resolve(&recorded, "s2:paper").unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "semantic scholar's release 2026-11-10 has no dataset paper"
    );
    assert!(
        error.help.unwrap().contains("paper-ids, papers"),
        "it names the datasets there are"
    );
    assert_eq!(recorded.asked(), [RELEASE], "no links are asked for");
}

/// a link to anywhere but S3 over https is no file of the dataset, whatever the API says.
#[test]
fn a_link_that_is_not_on_s3_is_refused() {
    let recorded = Recorded::default().fixture(RELEASE, "s2/release.json").on(
        LINKS,
        200,
        br#"{"name":"tldrs","files":["http://ai2-s2ag.s3.amazonaws.com/staging/a.gz"]}"#.to_vec(),
    );
    let error = resolve(&recorded, "s2:tldrs").unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
}

/// a stable link names the listing to ask again and the file to find in it; a file the listing
/// no longer has says how to pin the files it has.
#[test]
fn a_stable_link_finds_its_files_fresh_link_by_name() {
    let stable = Url::parse(&format!("{LINKS}#{FIRST}")).unwrap();
    let first = relink(&stable).unwrap();
    assert!(
        relink(&Url::parse(LINKS).unwrap()).is_none(),
        "no file named"
    );
    assert!(
        relink(
            &Url::parse(&format!(
                "https://api.semanticscholar.org/datasets/v1/release/latest/dataset/tldrs#{FIRST}"
            ))
            .unwrap()
        )
        .is_none(),
        "no release pinned"
    );
    let recorded = Recorded::default().fixture(LINKS, "s2/dataset.json");
    let link = block_on(first.link(&recorded)).unwrap();
    assert_eq!(
        link.as_str(),
        format!(
            "https://ai2-s2ag.s3.amazonaws.com/staging/2026-11-10/tldrs/{FIRST}?AWSAccessKeyId=PLACEHOLDER&Signature=PLACEHOLDER&x-amz-security-token=PLACEHOLDER&Expires=4102444800"
        )
    );
    let gone = relink(&Url::parse(&format!("{LINKS}#gone.gz")).unwrap()).unwrap();
    let error = block_on(gone.link(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "semantic scholar no longer lists gone.gz in s2:tldrs@2026-11-10"
    );
    assert!(
        crate::source::fetch_plan(&stable).expired.is_some(),
        "a download tells an expired link apart"
    );
}

#[test]
fn a_signed_link_expires_a_minute_before_its_time() {
    let link = |expires: &str| {
        Url::parse(&format!(
            "https://ai2-s2ag.s3.amazonaws.com/a.gz?Signature=x&Expires={expires}"
        ))
        .unwrap()
    };
    let now = UNIX_EPOCH + Duration::from_secs(1_763_000_000);
    assert!(link_expired(&link("1762999999"), now));
    assert!(link_expired(&link("1763000060"), now));
    assert!(!link_expired(&link("1763000061"), now));
    assert!(!link_expired(&link("4102444800"), now));
    let unstated = Url::parse("https://ai2-s2ag.s3.amazonaws.com/a.gz").unwrap();
    assert!(!link_expired(&unstated, now), "no expiry is S3's to judge");
}

/// S3's error documents for a link whose time ran out and for one whose signing session ended,
/// as AWS words them, against refusals that are not about expiry.
#[test]
fn s3_says_a_link_expired_in_two_ways() {
    let expired = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>AccessDenied</Code><Message>Request has expired</Message><Expires>1970-01-01T00:00:00Z</Expires><ServerTime>2026-11-13T13:37:05Z</ServerTime><RequestId>VBVWVFN0V1TNHWXR</RequestId></Error>";
    let token = "<Error><Code>ExpiredToken</Code><Message>The provided token has expired.</Message></Error>";
    let signature = "<Error><Code>SignatureDoesNotMatch</Code><Message>The request signature we calculated does not match the signature you provided.</Message></Error>";
    let denied = "<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>";
    let check = expiry_check(&Url::parse(&format!("{LINKS}#{FIRST}")).unwrap()).unwrap();
    assert!(check(expired));
    assert!(check(token));
    assert!(!check(signature));
    assert!(!check(denied));
}

/// semantic scholar answers without a key, so these run in CI's live job: one paper and one small
/// search, a few seconds apart. the pool every program without a key shares is often busy, and a
/// run it turns away says so and checks nothing.
#[test]
#[ignore = "asks semantic scholar's API"]
fn live_semantic_scholar_paper_and_search() {
    let (net, _work) = crate::source::recorded::live(Vec::new());
    let busy = |error: &Error| {
        let turned_away = error.kind == Kind::NetworkRateLimited;
        if turned_away {
            eprintln!("skipped: {}", error.message);
        }
        turned_away
    };
    let paper = Reference::parse("s2:DOI:10.1371/journal.pcbi.1003285")
        .unwrap()
        .reference;
    let listing = match block_on(paper.resolve(&net)) {
        Err(error) if busy(&error) => return,
        listing => listing.unwrap(),
    };
    assert_eq!(
        listing.title.as_deref(),
        Some("Ten Simple Rules for Reproducible Computational Research")
    );
    assert_eq!(listing.license.as_deref(), Some("CC-BY"));
    let words = ["attention", "is", "all", "you", "need"].map(str::to_owned);
    let found = match block_on(search(&net, &words, 3)) {
        Err(error) if busy(&error) => return,
        found => found.unwrap(),
    };
    assert_eq!(found[0].reference.to_string(), "s2:CorpusId:13756489");
}

/// a dataset's links need a key, which the owner sets in `S2_API_KEY`; without one this says so
/// and passes.
#[test]
#[ignore = "asks semantic scholar's API with the key in S2_API_KEY"]
fn live_semantic_scholar_dataset_with_a_key() {
    let Some(key) = std::env::var_os("S2_API_KEY").filter(|key| !key.is_empty()) else {
        eprintln!("skipped: S2_API_KEY is not set");
        return;
    };
    let var = move |name: &str| (name == "S2_API_KEY").then(|| key.clone());
    let (net, _work) = crate::source::recorded::live(crate::auth::credentials(&var));
    let dataset = Reference::parse("s2:tldrs").unwrap().reference;
    let listing = block_on(dataset.resolve(&net)).unwrap();
    assert!(listing.resolved.to_string().starts_with("s2:tldrs@"));
    assert_eq!(listing.license.as_deref(), Some("ODC-By-1.0"));
    assert!(!listing.files.is_empty(), "the release lists its files");
    let first = relink(&listing.files[0].url).unwrap();
    let link = block_on(first.link(&net)).unwrap();
    assert_eq!(link.host_str(), Some("ai2-s2ag.s3.amazonaws.com"));
    assert!(!link_expired(&link, std::time::SystemTime::now()));
}
