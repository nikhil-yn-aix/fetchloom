use std::fmt::Write as _;

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Access, Listing};

const PAPER: &str = "https://export.arxiv.org/api/query?id_list=1706.03762";
const LICENSE: &str = "https://oaipmh.arxiv.org/oai?verb=GetRecord&identifier=oai%3AarXiv.org%3A1706.03762&metadataPrefix=arXivRaw";
const DEFAULT: &str = "http://arxiv.org/licenses/nonexclusive-distrib/1.0/";

/// the mirror's listing of what it holds under `key`.
fn mirror(key: &str) -> String {
    format!(
        "https://storage.googleapis.com/storage/v1/b/arxiv-dataset/o?prefix={}&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken",
        key.replace('/', "%2F")
    )
}

/// a page of the mirror's listing, each `(key, size)` at generation 7.
fn held(objects: &[(&str, u64)]) -> Vec<u8> {
    let items: Vec<serde_json::Value> = objects
        .iter()
        .map(|(key, size)| {
            serde_json::json!({
                "name": key,
                "size": size.to_string(),
                "md5Hash": "F+Ni5+W6b/tiSMSi6SPmPg==",
                "generation": "7",
            })
        })
        .collect();
    serde_json::to_vec(&serde_json::json!({ "items": items })).unwrap()
}

/// a page of arXiv's feed counting `total` papers, holding each `(id with version, published)`.
fn feed(total: usize, entries: &[(String, String)]) -> Vec<u8> {
    let mut text = format!(
        "<?xml version='1.0' encoding='UTF-8'?>\n<feed xmlns:opensearch=\"http://a9.com/-/spec/opensearch/1.1/\" xmlns=\"http://www.w3.org/2005/Atom\"><opensearch:totalResults>{total}</opensearch:totalResults>"
    );
    for (id, published) in entries {
        let _ = write!(
            text,
            "<entry><id>http://arxiv.org/abs/{id}</id><title>Paper {id}</title><published>{published}</published><author><name>Ada Lovelace</name></author></entry>"
        );
    }
    text.push_str("</feed>");
    text.into_bytes()
}

/// arXiv's OAI-PMH record of `id`, each version in turn a withdrawal or not.
fn record(id: &str, withdrawn: &[bool]) -> Vec<u8> {
    let mut versions = String::new();
    for (at, withdrawn) in withdrawn.iter().enumerate() {
        let (size, source) = if *withdrawn {
            ("0kb", "I")
        } else {
            ("56kb", "D")
        };
        let _ = write!(
            versions,
            "<version version=\"v{}\"><date>Wed, 03 Jan 2024 08:54:56 GMT</date><size>{size}</size><source_type>{source}</source_type></version>",
            at + 1
        );
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><OAI-PMH xmlns=\"http://www.openarchives.org/OAI/2.0/\"><GetRecord><record><metadata><arXivRaw xmlns=\"http://arxiv.org/OAI/arXivRaw/\"><id>{id}</id>{versions}<license>{DEFAULT}</license></arXivRaw></metadata></record></GetRecord></OAI-PMH>"
    )
    .into_bytes()
}

/// where arXiv's OAI-PMH interface keeps the record of `id`.
fn record_of(id: &str) -> String {
    format!(
        "https://oaipmh.arxiv.org/oai?verb=GetRecord&identifier=oai%3AarXiv.org%3A{}&metadataPrefix=arXivRaw",
        id.replace('/', "%2F")
    )
}

fn reference(text: &str) -> String {
    Reference::parse(text).unwrap().reference.to_string()
}

fn resolve(recorded: &Recorded, text: &str) -> Result<Listing, Error> {
    block_on(Reference::parse(text).unwrap().reference.resolve(recorded))
}

#[test]
fn ids_versions_and_links_find_a_paper() {
    for (text, canonical) in [
        ("arxiv:1706.03762", "arxiv:1706.03762"),
        ("arXiv:1706.03762v7", "arxiv:1706.03762@v7"),
        ("arxiv:1706.03762V7", "arxiv:1706.03762@v7"),
        ("arxiv:1706.03762@v7", "arxiv:1706.03762@v7"),
        ("arxiv:1706.03762@7", "arxiv:1706.03762@v7"),
        ("arxiv:0704.0001", "arxiv:0704.0001"),
        ("arxiv:hep-th/9901001", "arxiv:hep-th/9901001"),
        ("arxiv:HEP-TH/9901001v2", "arxiv:hep-th/9901001@v2"),
        ("arxiv:math.GT/0309136", "arxiv:math/0309136"),
        ("arxiv:solv-int/9901001", "arxiv:solv-int/9901001"),
        ("https://arxiv.org/abs/1706.03762", "arxiv:1706.03762"),
        ("https://arxiv.org/abs/1706.03762v7", "arxiv:1706.03762@v7"),
        ("https://arxiv.org/pdf/1706.03762v7", "arxiv:1706.03762@v7"),
        (
            "https://arxiv.org/pdf/1706.03762v7.pdf",
            "arxiv:1706.03762@v7",
        ),
        ("http://www.arxiv.org/pdf/1706.03762", "arxiv:1706.03762"),
        (
            "https://export.arxiv.org/abs/hep-th/9901001v1",
            "arxiv:hep-th/9901001@v1",
        ),
        (
            "https://arxiv.org/html/2401.00001v2/",
            "arxiv:2401.00001@v2",
        ),
        ("arxiv:2401", "arxiv:2401"),
        ("arxiv:cs.LG/2401", "arxiv:cs.LG/2401"),
        ("arxiv:cs.lg/2401", "arxiv:cs.LG/2401"),
        (
            "arxiv:Cond-Mat.Mes-Hall/2401",
            "arxiv:cond-mat.mes-hall/2401",
        ),
        ("arxiv:hep-th/9901", "arxiv:hep-th/9901"),
    ] {
        assert_eq!(reference(text), canonical, "{text}");
        assert_eq!(reference(canonical), canonical, "{canonical} reads back");
    }
}

#[test]
fn malformed_references_are_refused() {
    for bad in [
        "arxiv:",
        "arxiv:1706.0376",
        "arxiv:1501.0001",
        "arxiv:1412.00001",
        "arxiv:0703.0001",
        "arxiv:1713.03762",
        "arxiv:1706.03762@v0",
        "arxiv:1706.03762@",
        "arxiv:1706.03762v7@v7",
        "arxiv:1706.03762x",
        "arxiv:hep-th/990100",
        "arxiv:hep_th/9901001",
        "arxiv:math.GTX/0309136",
        "arxiv:hep-th/0704001",
        "arxiv:2413",
        "arxiv:2401@v2",
        "arxiv:cs..LG/2401",
        "arxiv:cs.LG/24011",
        "arxiv:cs.L1/2401",
    ] {
        let error = Reference::parse(bad).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{bad}");
    }
    let early = Reference::parse("arxiv:9901").unwrap_err();
    assert_eq!(
        early.message,
        "arxiv:9901 is before april 2007, when arXiv numbered papers within each archive"
    );
    assert_eq!(
        early.help.as_deref(),
        Some(
            "add one archive's month from the mirror, as gs://arxiv-dataset/arxiv/hep-th/pdf/9901/, or one category's, as arxiv:hep-th/9901"
        )
    );
    for other in [
        "https://arxiv.org/list/cs.LG/2401",
        "https://arxiv.org/e-print/1706.03762",
        "https://arxiv.org/abs/",
    ] {
        let error = Reference::parse(other).unwrap_err();
        assert!(error.message.contains("arXiv link"), "{other}: {error:?}");
        assert_eq!(
            error.help.as_deref(),
            Some(
                "paste a paper's page or its pdf link, as https://arxiv.org/abs/1706.03762, or write arxiv:2401 for a month's papers or arxiv:cs.LG/2401 for one category's"
            ),
            "{other}"
        );
    }
}

/// requests to every arXiv host share one pace, one at a time and three seconds apart, as
/// arXiv's terms ask of a program; the mirror on google's storage is not paced.
#[test]
fn arxivs_hosts_are_paced_together_and_others_alone() {
    for host in ["arxiv.org", "export.arxiv.org", "oaipmh.arxiv.org"] {
        let name = crate::source::paced_as(host);
        assert_eq!(name, "arxiv.org", "{host}");
        assert_eq!(
            crate::source::spacing(&name),
            Some(Duration::from_secs(3)),
            "{host}"
        );
        assert_eq!(crate::source::most_at_once(&name), Some(1), "{host}");
    }
    for host in [
        "storage.googleapis.com",
        "arxiv.org.example.org",
        "notarxiv.org",
    ] {
        let name = crate::source::paced_as(host);
        assert_eq!(name, host);
        assert_eq!(crate::source::spacing(&name), None, "{host}");
        assert_eq!(crate::source::most_at_once(&name), None, "{host}");
    }
}

/// the newest version is pinned, with the license arXiv states and the mirror's copy, pinned to
/// its generation; the API, then OAI-PMH, then the mirror are asked once each.
#[test]
fn a_paper_is_pinned_at_its_newest_version_with_the_mirrors_pdf() {
    let key = "arxiv/arxiv/pdf/1706/1706.03762v7.pdf";
    let recorded = Recorded::default()
        .fixture(PAPER, "arxiv/paper.xml")
        .fixture(LICENSE, "arxiv/license.xml")
        .on(&mirror(key), 200, held(&[(key, 2_215_244)]));
    let listing = resolve(&recorded, "arxiv:1706.03762").unwrap();
    assert_eq!(listing.resolved.to_string(), "arxiv:1706.03762@v7");
    assert_eq!(listing.title.as_deref(), Some("Attention Is All You Need"));
    assert_eq!(listing.license.as_deref(), Some(DEFAULT));
    assert_eq!(listing.doi.as_deref(), Some("10.48550/arXiv.1706.03762"));
    assert_eq!(listing.access, Access::Unknown);
    let [file] = listing.files.as_slice() else {
        panic!("{:?}", listing.files);
    };
    assert_eq!(file.path, "1706.03762v7.pdf");
    assert_eq!(
        file.url.as_str(),
        "https://storage.googleapis.com/arxiv-dataset/arxiv/arxiv/pdf/1706/1706.03762v7.pdf?generation=7"
    );
    assert_eq!(file.size, Some(2_215_244));
    assert!(file.md5.is_some());
    assert_eq!(
        recorded.asked(),
        [PAPER.to_owned(), LICENSE.to_owned(), mirror(key)]
    );
    assert!(!listing.names_its_file());
    assert_eq!(listing.naming_title(), Some("Attention Is All You Need"));
}

/// a paper newer than the mirror comes from the host arXiv keeps for programs, its size unknown
/// until it downloads.
#[test]
fn a_paper_the_mirror_lacks_comes_from_export_arxiv_org() {
    let key = "arxiv/arxiv/pdf/1706/1706.03762v7.pdf";
    let recorded = Recorded::default()
        .fixture(PAPER, "arxiv/paper.xml")
        .fixture(LICENSE, "arxiv/license.xml")
        .on(&mirror(key), 200, b"{}".to_vec());
    let listing = resolve(&recorded, "https://arxiv.org/abs/1706.03762").unwrap();
    let [file] = listing.files.as_slice() else {
        panic!("{:?}", listing.files);
    };
    assert_eq!(file.path, "1706.03762v7.pdf");
    assert_eq!(
        file.url.as_str(),
        "https://export.arxiv.org/pdf/1706.03762v7"
    );
    assert_eq!((file.size, file.md5), (None, None));
}

/// an old id keeps its archive in the file's name, and the mirror files it by archive.
#[test]
fn an_old_paper_is_found_in_its_archives_folder() {
    let asked = "https://export.arxiv.org/api/query?id_list=hep-th%2F9901001";
    let license = "https://oaipmh.arxiv.org/oai?verb=GetRecord&identifier=oai%3AarXiv.org%3Ahep-th%2F9901001&metadataPrefix=arXivRaw";
    let key = "arxiv/hep-th/pdf/9901/9901001v3.pdf";
    let recorded = Recorded::default()
        .fixture(asked, "arxiv/old-paper.xml")
        .fixture(license, "arxiv/no-record.xml")
        .on(&mirror(key), 200, held(&[(key, 140_744)]));
    let listing = resolve(&recorded, "arxiv:hep-th/9901001").unwrap();
    assert_eq!(listing.resolved.to_string(), "arxiv:hep-th/9901001@v3");
    assert_eq!(listing.files[0].path, "hep-th9901001v3.pdf");
    assert_eq!(listing.license, None, "arXiv states no license for it");
    assert_eq!(
        listing.doi.as_deref(),
        Some("10.48550/arXiv.hep-th/9901001")
    );
}

/// a version asked for is the one listed, and one arXiv does not have is unresolved.
#[test]
fn a_named_version_is_kept_and_a_missing_one_is_unresolved() {
    let asked = "https://export.arxiv.org/api/query?id_list=1706.03762v5";
    let key = "arxiv/arxiv/pdf/1706/1706.03762v5.pdf";
    let five = String::from_utf8(fixture_bytes("arxiv/paper.xml"))
        .unwrap()
        .replace("1706.03762v7", "1706.03762v5");
    let recorded = Recorded::default()
        .on(asked, 200, five)
        .fixture(LICENSE, "arxiv/license.xml")
        .on(&mirror(key), 200, held(&[(key, 2_201_700)]));
    let listing = resolve(&recorded, "arxiv:1706.03762@v5").unwrap();
    assert_eq!(listing.resolved.to_string(), "arxiv:1706.03762@v5");
    assert_eq!(listing.files[0].path, "1706.03762v5.pdf");

    let missing = "https://export.arxiv.org/api/query?id_list=1706.03762v9";
    let recorded = Recorded::default().fixture(missing, "arxiv/none.xml");
    let error = resolve(&recorded, "arxiv:1706.03762v9").unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(error.message, "arXiv has no version v9 of arxiv:1706.03762");
    let recorded = Recorded::default().fixture(PAPER, "arxiv/none.xml");
    let error = resolve(&recorded, "arxiv:1706.03762").unwrap_err();
    assert_eq!(error.message, "arXiv has no paper arxiv:1706.03762");
}

/// an answer about another paper, an error entry or no feed at all is refused, never listed.
#[test]
fn answers_fetchloom_cannot_use_are_refused() {
    let paper = String::from_utf8(fixture_bytes("arxiv/paper.xml")).unwrap();
    for (body, reason) in [
        (
            paper.replace("1706.03762v7</id>", "1706.03763v1</id>"),
            "it described arxiv:1706.03763v1",
        ),
        (
            paper.replace(
                "<title>Attention Is All You Need</title>",
                "<title> </title>",
            ),
            "1706.03762 has no title",
        ),
        (
            "<html><body>busy</body></html>".to_owned(),
            "it is not an atom feed",
        ),
        (
            paper.replace(
                "http://arxiv.org/abs/1706.03762v7",
                "http://arxiv.org/api/errors#incorrect_id_format",
            ),
            "it said",
        ),
    ] {
        let recorded = Recorded::default()
            .on(PAPER, 200, body)
            .fixture(LICENSE, "arxiv/license.xml");
        let error = resolve(&recorded, "arxiv:1706.03762").unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "{reason}");
        assert!(
            error.message.contains(reason),
            "{reason}: {}",
            error.message
        );
        assert_eq!(
            error.help.as_deref(),
            Some(crate::error::report_answer().as_str())
        );
    }
    let recorded = Recorded::default().on(PAPER, 503, "busy");
    assert_eq!(
        resolve(&recorded, "arxiv:1706.03762").unwrap_err().kind,
        Kind::NetworkStatus
    );
}

/// a paper withdrawn at its newest version has no pdf there, and the error names an earlier one.
#[test]
fn a_paper_withdrawn_at_its_newest_version_is_refused_with_its_earlier_one() {
    let asked = "https://export.arxiv.org/api/query?id_list=2401.01626";
    let two = String::from_utf8(fixture_bytes("arxiv/paper.xml"))
        .unwrap()
        .replace("1706.03762v7", "2401.01626v2");
    let recorded = Recorded::default().on(asked, 200, two).on(
        &record_of("2401.01626"),
        200,
        record("2401.01626", &[false, true]),
    );
    let error = resolve(&recorded, "arxiv:2401.01626").unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "arxiv:2401.01626 was withdrawn at v2, which has no pdf"
    );
    assert_eq!(
        error.help.as_deref(),
        Some("its earlier versions stay on arXiv: add one by its version, as arxiv:2401.01626@v1")
    );
}

/// a license under a creative commons license may be shared, so its listing says public.
#[test]
fn a_creative_commons_paper_is_public() {
    let key = "arxiv/arxiv/pdf/1706/1706.03762v7.pdf";
    let open = String::from_utf8(fixture_bytes("arxiv/license.xml"))
        .unwrap()
        .replace(DEFAULT, "http://creativecommons.org/licenses/by/4.0/");
    let recorded = Recorded::default()
        .fixture(PAPER, "arxiv/paper.xml")
        .on(LICENSE, 200, open)
        .on(&mirror(key), 200, held(&[(key, 2_215_244)]));
    let listing = resolve(&recorded, "arxiv:1706.03762").unwrap();
    assert_eq!(listing.access, Access::Public);
}

/// a month lists each paper once, at the newest version the mirror holds, and leaves out what
/// names no paper of the month.
#[test]
fn a_month_lists_each_papers_newest_version_from_the_mirror() {
    let folder = "arxiv/arxiv/pdf/2401/";
    let recorded = Recorded::default().on(
        &mirror(folder),
        200,
        held(&[
            ("arxiv/arxiv/pdf/2401/2401.00001v1.pdf", 10),
            ("arxiv/arxiv/pdf/2401/2401.00002v1.pdf", 20),
            ("arxiv/arxiv/pdf/2401/2401.00002v3.pdf", 23),
            ("arxiv/arxiv/pdf/2401/2401.00002v2.pdf", 22),
            ("arxiv/arxiv/pdf/2401/README.txt", 1),
        ]),
    );
    let listing = resolve(&recorded, "arxiv:2401").unwrap();
    assert_eq!(listing.resolved.to_string(), "arxiv:2401");
    assert_eq!(listing.title.as_deref(), Some("arXiv, January 2024"));
    assert_eq!(listing.naming_title(), None);
    assert_eq!(listing.access, Access::Unknown);
    let files: Vec<(&str, Option<u64>)> = listing
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.size))
        .collect();
    assert_eq!(
        files,
        [
            ("2401.00001v1.pdf", Some(10)),
            ("2401.00002v3.pdf", Some(23))
        ]
    );
    assert_eq!(
        listing.warnings,
        [
            "left out 1 object in the mirror's folder for arxiv:2401, since it names no paper",
            month::LICENSES,
        ]
    );
    let recorded = Recorded::default().on(&mirror("arxiv/arxiv/pdf/2701/"), 200, b"{}".to_vec());
    let error = resolve(&recorded, "arxiv:2701").unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "the mirror holds no papers of arxiv:2701 yet"
    );
}

const CATEGORY: &str = "https://export.arxiv.org/api/query?search_query=cat%3Acs.LG+AND+submittedDate%3A%5B202401010000+TO+202402010000%5D&sortBy=submittedDate&sortOrder=ascending";

fn category_page(start: usize) -> String {
    format!("{CATEGORY}&start={start}&max_results=1000")
}

/// a category's month comes page by page from arXiv's API, each paper at the version the API
/// names, from the mirror where it holds that version. a paper submitted in the next month is
/// left out, one whose newest version is a withdrawal too, as arXiv's record tells, and one
/// revised since the mirror took it comes from export.arxiv.org.
#[test]
fn a_category_month_pages_through_the_api_and_downloads_from_the_mirror() {
    let first: Vec<(String, String)> = (1..=1000)
        .map(|number| {
            let version = if matches!(number, 2 | 3) { 2 } else { 1 };
            (
                format!("2401.{number:05}v{version}"),
                "2024-01-10T10:00:00Z".to_owned(),
            )
        })
        .collect();
    let second = [
        ("2401.01001v1", "2024-01-31T12:00:00Z"),
        ("2402.00010v1", "2024-01-31T23:50:00Z"),
        ("2402.00020v1", "2024-02-01T00:00:00Z"),
    ]
    .map(|(id, published)| (id.to_owned(), published.to_owned()));
    let mut objects: Vec<(String, u64)> = (1..=1001)
        .map(|number| (format!("arxiv/arxiv/pdf/2401/2401.{number:05}v1.pdf"), 5))
        .collect();
    objects.push(("arxiv/arxiv/pdf/2401/2401.99999v1.pdf".to_owned(), 5));
    let objects: Vec<(&str, u64)> = objects
        .iter()
        .map(|(key, size)| (key.as_str(), *size))
        .collect();
    let late = "arxiv/arxiv/pdf/2402/2402.00010v1.pdf";
    let recorded = Recorded::default()
        .on(&category_page(0), 200, feed(1003, &first))
        .on(&category_page(1000), 200, feed(1003, &second))
        .on(&mirror("arxiv/arxiv/pdf/2401/"), 200, held(&objects))
        .on(&mirror("arxiv/arxiv/pdf/2402/"), 200, held(&[(late, 6)]))
        .on(
            &record_of("2401.00002"),
            200,
            record("2401.00002", &[false, false]),
        )
        .on(
            &record_of("2401.00003"),
            200,
            record("2401.00003", &[false, true]),
        );
    let listing = resolve(&recorded, "arxiv:cs.lg/2401").unwrap();
    assert_eq!(listing.resolved.to_string(), "arxiv:cs.LG/2401");
    assert_eq!(listing.title.as_deref(), Some("arXiv cs.LG, January 2024"));
    assert_eq!(listing.files.len(), 1001);
    let found = |path: &str| {
        listing
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap_or_else(|| panic!("{path}"))
    };
    assert_eq!(
        found("2401.00002v2.pdf").url.as_str(),
        "https://export.arxiv.org/pdf/2401.00002v2",
        "the mirror holds only v1"
    );
    assert_eq!(
        found("2401.00001v1.pdf").url.as_str(),
        "https://storage.googleapis.com/arxiv-dataset/arxiv/arxiv/pdf/2401/2401.00001v1.pdf?generation=7"
    );
    assert_eq!(found("2402.00010v1.pdf").size, Some(6));
    for gone in ["2402.00020v1.pdf", "2401.00003v2.pdf", "2401.00003v1.pdf"] {
        assert!(listing.files.iter().all(|file| file.path != gone), "{gone}");
    }
    assert_eq!(
        listing.warnings,
        [
            "left out 1 paper whose newest version is a withdrawal, which has no pdf, as arxiv:2401.00003@v2; an earlier version can be added by its id",
            "the mirror does not hold 1 paper at its newest version yet, so it comes from export.arxiv.org, one every 3 seconds",
            month::LICENSES,
        ]
    );
    assert_eq!(
        recorded.asked(),
        [
            category_page(0),
            category_page(1000),
            mirror("arxiv/arxiv/pdf/2401/"),
            record_of("2401.00002"),
            record_of("2401.00003"),
            mirror("arxiv/arxiv/pdf/2402/"),
        ]
    );
}

/// a category with no papers in the month says how arXiv writes categories, and a page that
/// comes back empty before the count is reached is no listing.
#[test]
fn a_category_month_that_lists_nothing_or_stops_early_is_refused() {
    let recorded = Recorded::default().on(&category_page(0), 200, feed(0, &[]));
    let error = resolve(&recorded, "arxiv:cs.LG/2401").unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "arXiv lists no papers in cs.LG first submitted in January 2024"
    );
    let recorded = Recorded::default().on(&category_page(0), 200, feed(5, &[]));
    let error = resolve(&recorded, "arxiv:cs.LG/2401").unwrap_err();
    assert_eq!(error.kind, Kind::NetworkStatus);
    assert!(
        error.message.contains("sent no papers at 0 of the 5"),
        "{}",
        error.message
    );
    let recorded = Recorded::default().on(&category_page(0), 200, feed(30_001, &[]));
    let error = resolve(&recorded, "arxiv:cs.LG/2401").unwrap_err();
    assert_eq!(
        error.message,
        "arXiv's API lists at most 30,000 papers for one query, and arxiv:cs.LG/2401 has 30,001"
    );
}

/// a search finds titles first, then fills up from papers that hold the words anywhere.
#[test]
fn a_search_asks_titles_first_then_everywhere() {
    let titles = "https://export.arxiv.org/api/query?search_query=ti%3A%22attention+is+all+you+need%22&start=0&max_results=3&sortBy=relevance";
    let anywhere = "https://export.arxiv.org/api/query?search_query=all%3A%22attention+is+all+you+need%22&start=0&max_results=3&sortBy=relevance";
    let recorded = Recorded::default().fixture(titles, "arxiv/paper.xml").on(
        anywhere,
        200,
        feed(
            47,
            &[
                ("2604.21816v1".to_owned(), "2026-04-02T00:00:00Z".to_owned()),
                ("1706.03762v7".to_owned(), "2017-06-12T17:57:34Z".to_owned()),
                ("2104.04692v3".to_owned(), "2021-04-10T00:00:00Z".to_owned()),
            ],
        ),
    );
    let words = ["attention", "is", "all", "you", "need"].map(str::to_owned);
    let found = block_on(search(&recorded, &words, 3)).unwrap();
    let shown: Vec<(String, &str, Option<&str>)> = found
        .iter()
        .map(|found| {
            (
                found.reference.to_string(),
                found.title.as_str(),
                found.note.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (
                "arxiv:1706.03762".to_owned(),
                "Attention Is All You Need",
                Some("Vaswani et al. 2017")
            ),
            (
                "arxiv:2604.21816".to_owned(),
                "Paper 2604.21816v1",
                Some("Lovelace 2026")
            ),
            (
                "arxiv:2104.04692".to_owned(),
                "Paper 2104.04692v3",
                Some("Lovelace 2021")
            ),
        ]
    );
    assert!(block_on(search(&recorded, &[], 3)).unwrap().is_empty());
}

#[test]
#[ignore = "live: talks to export.arxiv.org, oaipmh.arxiv.org and storage.googleapis.com, paced"]
fn live_arxiv_paper_category_page_and_search() {
    let (net, _work) = crate::source::recorded::live(Vec::new());
    let paper = Reference::parse("arxiv:1706.03762").unwrap().reference;
    let listing = block_on(paper.resolve(&net)).unwrap();
    assert!(
        listing
            .resolved
            .to_string()
            .starts_with("arxiv:1706.03762@v")
    );
    assert_eq!(listing.title.as_deref(), Some("Attention Is All You Need"));
    assert_eq!(listing.license.as_deref(), Some(DEFAULT));
    assert_eq!(
        listing.files[0].url.host_str(),
        Some("storage.googleapis.com"),
        "the mirror holds it"
    );
    let month = Reference::parse("arxiv:q-bio.SC/2401").unwrap().reference;
    let listing = block_on(month.resolve(&net)).unwrap();
    assert_eq!(
        listing.title.as_deref(),
        Some("arXiv q-bio.SC, January 2024")
    );
    assert!(
        !listing.files.is_empty(),
        "one page of the category's papers"
    );
    assert!(listing.files.iter().all(|file| {
        matches!(
            file.url.host_str(),
            Some("storage.googleapis.com" | "export.arxiv.org")
        )
    }));
    let words = ["attention", "is", "all", "you", "need"].map(str::to_owned);
    let found = block_on(search(&net, &words, 1)).unwrap();
    assert_eq!(found[0].reference.to_string(), "arxiv:1706.03762");
}
