use std::time::Instant;

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Access, Reference};

const SERIES: &str = "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/";

/// GSE1000's folder and the four folders in it, as NCBI's file server answers them.
fn series() -> Recorded {
    Recorded::default()
        .fixture(SERIES, "geo/series.html")
        .fixture(&format!("{SERIES}matrix/"), "geo/matrix.html")
        .fixture(&format!("{SERIES}miniml/"), "geo/miniml.html")
        .fixture(&format!("{SERIES}soft/"), "geo/soft.html")
        .fixture(&format!("{SERIES}suppl/"), "geo/suppl.html")
}

fn accession(text: &str) -> Accession {
    Accession::parse(text).unwrap()
}

fn paths(listing: &Listing) -> Vec<&str> {
    listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

/// an index in the form NCBI's file server writes one, linking to `entries`.
fn index(entries: &[&str]) -> Vec<u8> {
    let links = entries
        .iter()
        .map(|entry| format!("<a href=\"{entry}\">{entry}</a>  2026-10-05 16:24  1.3M\n"))
        .collect::<Vec<_>>()
        .concat();
    format!("<pre><a href=\"/geo/series/GSE1nnn/\">Parent Directory</a>\n{links}</pre>")
        .into_bytes()
}

/// each file goes by its path below the series' folder, in index order. NCBI's indexes round sizes,
/// as `1.3M`, so a file's size is left for its download to tell.
#[test]
fn lists_a_series_files_from_its_folders_without_rounded_sizes() {
    let recorded = series();
    let listing = block_on(accession("GSE1000").resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "geo:GSE1000");
    assert_eq!(listing.access, Access::Public);
    assert_eq!(
        paths(&listing),
        [
            "matrix/GSE1000_series_matrix.txt.gz",
            "miniml/GSE1000_family.xml.tgz",
            "soft/GSE1000_family.soft.gz",
            "suppl/GSE1000_RAW.tar",
            "suppl/filelist.txt",
        ]
    );
    assert_eq!(
        listing.files[0],
        Remote {
            path: "matrix/GSE1000_series_matrix.txt.gz".to_owned(),
            size: None,
            sha256: None,
            md5: None,
            url: Url::parse(&format!("{SERIES}matrix/GSE1000_series_matrix.txt.gz")).unwrap(),
            named_by_server: false,
            etag: None,
        }
    );
    assert!(listing.files.iter().all(|file| file.size.is_none()));
    assert_eq!(
        recorded.asked(),
        [
            SERIES.to_owned(),
            format!("{SERIES}matrix/"),
            format!("{SERIES}miniml/"),
            format!("{SERIES}soft/"),
            format!("{SERIES}suppl/"),
        ]
    );
}

#[test]
fn a_sample_lists_its_supplementary_files() {
    let folder = "https://ftp.ncbi.nlm.nih.gov/geo/samples/GSM2666nnn/GSM2666939/";
    let recorded = Recorded::default()
        .fixture(folder, "geo/sample.html")
        .fixture(&format!("{folder}suppl/"), "geo/sample-suppl.html");
    let listing = block_on(accession("GSM2666939").resolve(&recorded)).unwrap();
    assert_eq!(
        paths(&listing),
        ["suppl/GSM2666939_chip_fosl1b11-bira.macs14_peaks.bed.gz"]
    );
    assert_eq!(
        listing.files[0].url.as_str(),
        format!("{folder}suppl/GSM2666939_chip_fosl1b11-bira.macs14_peaks.bed.gz")
    );
}

#[test]
fn a_platform_lists_its_annotation_and_family_files() {
    let folder = "https://ftp.ncbi.nlm.nih.gov/geo/platforms/GPLnnn/GPL96/";
    let recorded = Recorded::default()
        .fixture(folder, "geo/platform.html")
        .fixture(&format!("{folder}annot/"), "geo/annot.html")
        .fixture(&format!("{folder}miniml/"), "geo/platform-miniml.html")
        .fixture(&format!("{folder}soft/"), "geo/platform-soft.html");
    let listing = block_on(accession("GPL96").resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 15);
    assert_eq!(listing.files[0].path, "annot/GPL96.annot.gz");
    assert_eq!(listing.files[14].path, "soft/GPL96_family.soft.gz");
}

/// a thousand accessions share a folder named by all but their last three digits.
#[test]
fn an_accession_sits_in_the_folder_of_its_thousand() {
    for (id, folder) in [
        ("GSE10", "series/GSEnnn/GSE10/"),
        ("GSE1000", "series/GSE1nnn/GSE1000/"),
        ("GSE123456", "series/GSE123nnn/GSE123456/"),
        ("GSM2666939", "samples/GSM2666nnn/GSM2666939/"),
        ("GPL96", "platforms/GPLnnn/GPL96/"),
    ] {
        assert_eq!(
            accession(id).folder().unwrap().as_str(),
            format!("https://ftp.ncbi.nlm.nih.gov/geo/{folder}"),
            "{id}"
        );
    }
}

/// GEO keeps no folder for an accession that does not exist or is not public, nor for a sample
/// with no supplementary files; a folder with no files is the same to a listing.
#[test]
fn an_accession_geo_keeps_no_files_for_is_unresolved() {
    let missing = "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE999nnn/GSE999999/";
    let recorded = Recorded::default().on(missing, 404, fixture_bytes("geo/missing.html"));
    let error = block_on(accession("GSE999999").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(error.message, "GEO keeps no files for GSE999999");
    assert!(
        error
            .help
            .unwrap()
            .contains("https://www.ncbi.nlm.nih.gov/geo/query/acc.cgi?acc=GSE999999")
    );
    assert_eq!(recorded.asked(), [missing]);

    let recorded = Recorded::default().on(SERIES, 200, index(&["suppl/"])).on(
        &format!("{SERIES}suppl/"),
        200,
        index(&[]),
    );
    let error = block_on(accession("GSE1000").resolve(&recorded)).unwrap_err();
    assert_eq!(error.message, "GEO keeps no files for GSE1000");
}

#[test]
fn a_refusal_or_a_failing_server_is_told_as_such() {
    let recorded = Recorded::default().on(SERIES, 403, b"forbidden".to_vec());
    let error = block_on(accession("GSE1000").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(error.need.is_none(), "no sign-in here lifts it");
    let recorded = Recorded::default().on(SERIES, 500, b"busy".to_vec());
    let error = block_on(accession("GSE1000").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::NetworkStatus);
}

#[test]
fn an_index_that_cannot_be_trusted_says_why() {
    for (row, entries, reason) in [
        (
            "a name with a separator",
            &["a%2Fb.txt"][..],
            "names an entry that is no file name",
        ),
        (
            "a name with a control character",
            &["a%0Ab.txt"][..],
            "names an entry that is no file name",
        ),
    ] {
        let recorded = Recorded::default().on(SERIES, 200, index(entries));
        let Err(error) = block_on(accession("GSE1000").resolve(&recorded)) else {
            panic!("{row}: the index was used");
        };
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
        assert!(error.help.is_some(), "{row}");
    }
    let recorded = Recorded::default().on(SERIES, 200, index(&["suppl/"])).on(
        &format!("{SERIES}suppl/"),
        404,
        fixture_bytes("geo/missing.html"),
    );
    let error = block_on(accession("GSE1000").resolve(&recorded)).unwrap_err();
    assert!(
        error
            .message
            .contains("a folder its index lists is not there"),
        "{}",
        error.message
    );
}

/// a link that is no entry is never followed.
#[test]
fn links_away_from_the_folder_are_not_entries() {
    let html = r#"<a href="/geo/series/GSE1nnn/">Parent Directory</a>
<a href="../GSE1001/">x</a> <a href="https://example.org/x.gz">x</a> <a href="?C=M;O=A">Last modified</a>
<a href="data.txt">data.txt</a>"#;
    assert_eq!(entries(html), ["data.txt"]);
}

#[test]
fn more_folders_than_a_listing_reads_are_refused() {
    let names: Vec<String> = (0..MOST_FOLDERS).map(|n| format!("f{n}/")).collect();
    let links: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut recorded = Recorded::default().on(SERIES, 200, index(&links));
    for name in &names {
        recorded = recorded.on(&format!("{SERIES}{name}"), 200, index(&[]));
    }
    let error = block_on(accession("GSE1000").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(
        error.message.contains("more than 64 folders"),
        "{}",
        error.message
    );
    assert_eq!(recorded.asked().len(), MOST_FOLDERS);
}

/// GEO's page of an accession and its folder on NCBI's file server name it, a link into a folder
/// selecting that folder. a file's link and NCBI's other pages stay ordinary links.
#[test]
fn accessions_and_pages_name_one_accession_and_other_forms_are_refused() {
    for (text, expected, select) in [
        ("geo:GPL96", "geo:GPL96", None),
        (
            "https://www.ncbi.nlm.nih.gov/geo/query/acc.cgi?acc=gse1000&targ=self&form=text",
            "geo:GSE1000",
            None,
        ),
        (
            "http://www.ncbi.nlm.nih.gov/geo/query/acc.cgi?acc=GSM2666939",
            "geo:GSM2666939",
            None,
        ),
        (
            "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000",
            "geo:GSE1000",
            None,
        ),
        (
            "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/suppl/",
            "geo:GSE1000",
            Some("suppl/**"),
        ),
    ] {
        let parsed = Reference::parse(text).unwrap();
        assert_eq!(parsed.reference.to_string(), expected, "{text}");
        assert_eq!(
            parsed.select(),
            select.map(str::to_owned).into_iter().collect::<Vec<_>>(),
            "{text}"
        );
    }
    for text in [
        "geo:GDS10",
        "geo:GSE",
        "geo:GSE0012",
        "geo:GSEabc",
        "geo:",
        "https://www.ncbi.nlm.nih.gov/geo/browse/",
        "https://www.ncbi.nlm.nih.gov/geo/query/acc.cgi?acc=GDS10",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
    }
    for link in [
        "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/suppl/GSE1000_RAW.tar",
        "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/filelist.txt",
        "https://www.ncbi.nlm.nih.gov/sra/SRR390728",
    ] {
        assert_eq!(
            Reference::parse(link).unwrap().reference,
            Reference::Url(Url::parse(link).unwrap()),
            "{link}"
        );
    }
}

#[test]
fn ncbi_hosts_ask_their_requests_to_start_a_third_of_a_second_apart() {
    for host in [
        "ftp.ncbi.nlm.nih.gov",
        "www.ncbi.nlm.nih.gov",
        "ncbi.nlm.nih.gov",
    ] {
        assert_eq!(spacing(host), Some(Duration::from_millis(334)), "{host}");
    }
    for host in [
        "ncbi.nlm.nih.gov.example.org",
        "ftp.sra.ebi.ac.uk",
        "nih.gov",
    ] {
        assert_eq!(spacing(host), None, "{host}");
    }
}

#[test]
#[ignore = "live: talks to ftp.ncbi.nlm.nih.gov"]
fn live_geo() {
    let (net, work) = crate::source::recorded::live(Vec::new());
    let start = Instant::now();
    let reference = Reference::parse("geo:GSE1000").unwrap().reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "geo: {} files, {} requests, {:?}",
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    assert!(
        listing
            .files
            .iter()
            .any(|file| file.path == "matrix/GSE1000_series_matrix.txt.gz")
    );
    assert_eq!(work.counts().requests, 5);
}
