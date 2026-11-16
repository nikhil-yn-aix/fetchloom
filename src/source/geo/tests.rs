use std::time::Instant;

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, asked, block_on, fixture_bytes};
use crate::source::{Access, Reference};

const SERIES: &str = "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/";

/// the size `suppl/filelist.txt` states for GSE1000's tar of every sample's files.
const RAW_TAR: u64 = 35_307_520;

/// GSE1000's folder, the four folders in it and its filelist, as NCBI's file server answers them.
fn series() -> Recorded {
    Recorded::default()
        .fixture(SERIES, "geo/series.html")
        .fixture(&format!("{SERIES}matrix/"), "geo/matrix.html")
        .fixture(&format!("{SERIES}miniml/"), "geo/miniml.html")
        .fixture(&format!("{SERIES}soft/"), "geo/soft.html")
        .fixture(&format!("{SERIES}suppl/"), "geo/suppl.html")
        .fixture(&format!("{SERIES}suppl/filelist.txt"), "geo/filelist.txt")
}

/// NCBI's refusal of a listing, as it answered empathy's probe and a live run on windows.
fn forbidden() -> Vec<u8> {
    b"<html><head><title>403 Forbidden</title></head><body><h1>Access forbidden</h1></body></html>"
        .to_vec()
}

/// GSE1000's own folder refused as NCBI refuses it at times, and its files served: each name
/// GEO gives every series answers its first byte, and the filelist is read whole.
fn refused_series() -> Recorded {
    Recorded::default()
        .on(SERIES, 403, forbidden())
        .on(
            &format!("{SERIES}matrix/GSE1000_series_matrix.txt.gz"),
            206,
            b"\x1f".to_vec(),
        )
        .on(
            &format!("{SERIES}miniml/GSE1000_family.xml.tgz"),
            206,
            b"\x1f".to_vec(),
        )
        .on(
            &format!("{SERIES}soft/GSE1000_family.soft.gz"),
            206,
            b"\x1f".to_vec(),
        )
        .fixture(&format!("{SERIES}suppl/filelist.txt"), "geo/filelist.txt")
}

/// a request for `path`'s first byte alone, as the recorded network writes it down.
fn first_byte(path: &str) -> String {
    format!("{SERIES}{path} range: bytes=0-0")
}

fn sizes(listing: &Listing) -> Vec<(&str, Option<u64>)> {
    listing
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.size))
        .collect()
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
fn index(folder: &str, entries: &[&str]) -> Vec<u8> {
    let folder = Url::parse(folder).unwrap();
    let parent = folder.join("..").unwrap();
    let links = entries
        .iter()
        .map(|entry| format!("<a href=\"{entry}\">{entry}</a>  2026-10-05 16:24  1.3M\n"))
        .collect::<Vec<_>>()
        .concat();
    format!(
        "<html><head><title>Index of {}</title></head><body><pre><a href=\"{}\">Parent Directory</a>\n{links}</pre>",
        folder.path().trim_end_matches('/'),
        parent.path()
    )
    .into_bytes()
}

/// each file goes by its path below the series' folder, in index order. NCBI's indexes round sizes
/// from 973 bytes up, as `1.3M`, so a file's size comes from `suppl/filelist.txt`, which states the
/// tar of every sample's files exactly, or from an index that states it in bytes, as the
/// filelist's own `621`; any other is left for its download to tell.
#[test]
fn lists_a_series_files_with_the_sizes_its_filelist_states() {
    let recorded = series();
    let listing = block_on(accession("GSE1000").resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "geo:GSE1000");
    assert_eq!(listing.access, Access::Public);
    let filelist = fixture_bytes("geo/filelist.txt").len() as u64;
    assert_eq!(
        sizes(&listing),
        [
            ("matrix/GSE1000_series_matrix.txt.gz", None),
            ("miniml/GSE1000_family.xml.tgz", None),
            ("soft/GSE1000_family.soft.gz", None),
            ("suppl/GSE1000_RAW.tar", Some(RAW_TAR)),
            ("suppl/filelist.txt", Some(filelist)),
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
            blob: None,
        }
    );
    assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
    assert_eq!(
        recorded.asked(),
        [
            SERIES.to_owned(),
            format!("{SERIES}matrix/"),
            format!("{SERIES}miniml/"),
            format!("{SERIES}soft/"),
            format!("{SERIES}suppl/"),
            format!("{SERIES}suppl/filelist.txt"),
        ]
    );
}

/// NCBI refuses a program the listing of its folders at times while it serves their files, so a
/// refused series is listed by the names GEO gives every series and the tar its filelist names,
/// which for GSE1000 are every file it keeps, and says what such a listing may leave out.
#[test]
fn a_refused_listing_lists_the_files_geo_names_for_every_series() {
    let recorded = refused_series();
    let listing = block_on(accession("GSE1000").resolve(&recorded)).unwrap();
    let mut listed = block_on(accession("GSE1000").resolve(&series())).unwrap();
    // only the index NCBI refused states the filelist's own size
    for file in &mut listed.files {
        if file.path == FILELIST {
            file.size = None;
        }
    }
    assert_eq!(listing.files, listed.files);
    assert_eq!(listing.access, Access::Public);
    assert_eq!(
        recorded.asked(),
        [
            SERIES.to_owned(),
            first_byte("matrix/GSE1000_series_matrix.txt.gz"),
            first_byte("miniml/GSE1000_family.xml.tgz"),
            first_byte("soft/GSE1000_family.soft.gz"),
            format!("{SERIES}suppl/filelist.txt"),
        ]
    );
    assert_eq!(
        listing.warnings,
        [
            "NCBI refused to list the folders of geo:GSE1000 for now, so it holds the files GEO names alike for every series: one named otherwise, such as a matrix for each of several platforms, is left out until NCBI lists them again"
        ]
    );
}

/// a series of several platforms has a matrix for each, under names no rule gives, so the one
/// name a series of one platform has is left out when its first byte is not there.
#[test]
fn a_name_the_file_server_does_not_serve_is_left_out() {
    let matrix = format!("{SERIES}matrix/GSE1000_series_matrix.txt.gz");
    let recorded = refused_series().on(&matrix, 404, fixture_bytes("geo/missing.html"));
    let listing = block_on(accession("GSE1000").resolve(&recorded)).unwrap();
    assert_eq!(
        paths(&listing),
        [
            "miniml/GSE1000_family.xml.tgz",
            "soft/GSE1000_family.soft.gz",
            "suppl/GSE1000_RAW.tar",
            "suppl/filelist.txt",
        ]
    );
}

/// a folder refused while the others list takes the names GEO gives its files alone.
#[test]
fn a_refused_folder_alone_is_listed_by_name() {
    let soft = format!("{SERIES}soft/GSE1000_family.soft.gz");
    let recorded =
        series()
            .on(&format!("{SERIES}soft/"), 403, forbidden())
            .on(&soft, 206, b"\x1f".to_vec());
    let listing = block_on(accession("GSE1000").resolve(&recorded)).unwrap();
    let listed = block_on(accession("GSE1000").resolve(&series())).unwrap();
    assert_eq!(listing.files, listed.files);
    let probed: Vec<String> = recorded
        .asked()
        .into_iter()
        .filter(|asked| asked.contains("range:"))
        .collect();
    assert_eq!(probed, [first_byte("soft/GSE1000_family.soft.gz")]);
    assert_eq!(listing.warnings.len(), 1);
}

/// with a refused listing and no file GEO names by rule, nothing tells what the accession holds:
/// the refusal is NCBI's own, said as such, never as a private dataset's whose owner could help.
#[test]
fn a_refused_listing_with_nothing_named_says_to_try_again_later() {
    let sample = "https://ftp.ncbi.nlm.nih.gov/geo/samples/GSM2666nnn/GSM2666939/";
    let refused = Recorded::default().on(sample, 403, forbidden());
    let nothing = refused_series()
        .on(
            &format!("{SERIES}matrix/GSE1000_series_matrix.txt.gz"),
            403,
            forbidden(),
        )
        .on(
            &format!("{SERIES}miniml/GSE1000_family.xml.tgz"),
            404,
            fixture_bytes("geo/missing.html"),
        )
        .on(
            &format!("{SERIES}soft/GSE1000_family.soft.gz"),
            403,
            forbidden(),
        )
        .on(&format!("{SERIES}suppl/filelist.txt"), 403, forbidden());
    for (id, recorded) in [("GSM2666939", refused), ("GSE1000", nothing)] {
        let error = block_on(accession(id).resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkStatus, "{id}");
        assert_eq!(
            error.message,
            format!("ftp.ncbi.nlm.nih.gov refused to list the files of geo:{id}")
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "NCBI's file server refuses programs a listing of its folders at times, though GEO's files are public: run the same command again later"
            ),
            "{id}"
        );
        assert!(error.need.is_none(), "{id}: no sign-in lifts it");
    }
}

/// a filelist names the tars GEO keeps beside it by its rows of archives, in the form GEO writes;
/// a row it cannot read, or a list in another form, names none.
#[test]
fn a_filelist_names_only_the_archives_its_rows_state() {
    let text = String::from_utf8(fixture_bytes("geo/filelist.txt")).unwrap();
    assert_eq!(
        archives(&text),
        Some(vec![("GSE1000_RAW.tar".to_owned(), RAW_TAR)])
    );
    let header = "#Archive/File\tName\tTime\tSize\tType";
    let rows = [
        "Archive\tGSE1_RAW.tar\t01/17/2013 13:28:57\t10\tTAR",
        "Archive\tGSE1_more.tar\t01/17/2013 13:28:57\t1.3M\tTAR",
        "Archive\t../GSE1_RAW.tar\t01/17/2013 13:28:57\t10\tTAR",
        "Archive\tGSE1_RAW.tar\t10\tTAR",
        "File\tGSM1.CEL.gz\t01/28/2004 08:49:28\t3460434\tCEL",
        "Archive\tGSE1_last.tar\t01/17/2013 13:28:57\t20\tTAR\r",
    ];
    assert_eq!(
        archives(&format!("{header}\n{}\n", rows.join("\n"))),
        Some(vec![
            ("GSE1_RAW.tar".to_owned(), 10),
            ("GSE1_last.tar".to_owned(), 20)
        ])
    );
    for unread in [rows.join("\n"), "collected in 2020\n".to_owned()] {
        assert_eq!(archives(&unread), None, "{unread}");
    }
    let cut = format!("{header}\nArchive\tGSE1_RAW.tar\t01/17/2013 13:28:57\t3530");
    assert_eq!(
        archives(&cut),
        Some(Vec::new()),
        "a row cut short states no size"
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

    let recorded = Recorded::default()
        .on(SERIES, 200, index(SERIES, &["suppl/"]))
        .on(
            &format!("{SERIES}suppl/"),
            200,
            index(&format!("{SERIES}suppl/"), &[]),
        );
    let error = block_on(accession("GSE1000").resolve(&recorded)).unwrap_err();
    assert_eq!(error.message, "GEO keeps no files for GSE1000");
}

#[test]
fn a_failing_server_is_told_as_such() {
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
        ("an empty name", &[""][..], "links away"),
        ("a name two steps up", &["../.."][..], "links away"),
        ("a link away", &["../GSE1001/"][..], "links away"),
        (
            "a name holding a drive",
            &["C:hostile.csv"][..],
            "links away",
        ),
    ] {
        let recorded = Recorded::default().on(SERIES, 200, index(SERIES, entries));
        let Err(error) = block_on(accession("GSE1000").resolve(&recorded)) else {
            panic!("{row}: the index was used");
        };
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
        assert!(error.help.is_some(), "{row}");
    }
    let recorded = Recorded::default()
        .on(SERIES, 200, index(SERIES, &["suppl/"]))
        .on(
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
    let sign_in = b"<!DOCTYPE html><html><body><form action=\"/login\"></form></body></html>";
    for (link, reason) in [
        (
            format!("{SERIES}matrix/"),
            "it sent a page that is no index of its folder",
        ),
        (
            format!("{SERIES}suppl/filelist.txt"),
            "its suppl/filelist.txt is in another form than GEO's",
        ),
    ] {
        let recorded = series().on(&link, 200, sign_in.to_vec());
        let error = block_on(accession("GSE1000").resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "{link}");
        assert!(error.message.contains(reason), "{}", error.message);
    }
}

/// an index row ends with the size of its file, in bytes under 973 bytes and rounded from there,
/// as `34M`, so only a size in bytes is the file's own; a folder's row states none.
#[test]
fn an_index_row_states_a_size_in_bytes_alone() {
    let folder = Url::parse("https://ftp.ncbi.nlm.nih.gov/p/x/").unwrap();
    let html = "<title>Index of /p/x</title><pre><a href=\"/p/\">Parent Directory</a>   -\n<a href=\"filelist.txt\">filelist.txt</a>  2013-01-17 13:28  621   \n<a href=\"a.tar\">a.tar</a>  2013-01-17 13:28   34M  \n<a href=\"b.gz\">b.gz</a>  2013-01-17 13:28  1.0K  \n<a href=\"suppl/\">suppl/</a>  2014-06-02 17:34    -   \n</pre>";
    let index = super::super::folder::read(html, &folder, &folder).unwrap();
    let sizes: Vec<(&str, Option<u64>)> = index
        .entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry.size))
        .collect();
    assert_eq!(
        sizes,
        [
            ("filelist.txt", Some(621)),
            ("a.tar", None),
            ("b.gz", None),
            ("suppl", None),
        ]
    );
}

/// an index writes `&`, `<`, `>` and quotes in a link as html does, so `a&amp;b.csv` is the file
/// `a&b.csv`, by its name and its link alike, and `&#63;` is a `?` like any other.
#[test]
fn a_link_is_read_with_its_character_references_undone() {
    let folder = "https://ftp.ncbi.nlm.nih.gov/geo/samples/GSM2666nnn/GSM2666939/";
    let recorded = Recorded::default()
        .on(folder, 200, index(folder, &["suppl/"]))
        .on(
            &format!("{folder}suppl/"),
            200,
            index(
                &format!("{folder}suppl/"),
                &["a&amp;b.csv", "&lt;c&gt;&#39;d&#x27;.csv", "e&#63;f.csv"],
            ),
        );
    let listing = block_on(accession("GSM2666939").resolve(&recorded)).unwrap();
    assert_eq!(paths(&listing), ["suppl/<c>'d'.csv", "suppl/a&b.csv"]);
    assert_eq!(
        listing.files[0].url.as_str(),
        format!("{folder}suppl/%3Cc%3E'd'.csv")
    );
    assert_eq!(
        listing.files[1].url.as_str(),
        format!("{folder}suppl/a&b.csv")
    );
}

#[test]
fn more_folders_than_a_listing_reads_are_refused() {
    let names: Vec<String> = (0..MOST_FOLDERS).map(|n| format!("f{n}/")).collect();
    let links: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut recorded = Recorded::default().on(SERIES, 200, index(SERIES, &links));
    for name in &names {
        let folder = format!("{SERIES}{name}");
        recorded = recorded.on(&folder, 200, index(&folder, &[]));
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
        assert_eq!(
            crate::source::access::spacing(host),
            Some(Duration::from_millis(334)),
            "{host}"
        );
    }
    for host in [
        "ncbi.nlm.nih.gov.example.org",
        "ftp.sra.ebi.ac.uk",
        "nih.gov",
    ] {
        assert_eq!(crate::source::access::spacing(host), None, "{host}");
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
        asked(&work),
        start.elapsed()
    );
    assert!(
        listing
            .files
            .iter()
            .any(|file| file.path == "matrix/GSE1000_series_matrix.txt.gz")
    );
    assert!(listing.files.iter().any(|file| file.size == Some(RAW_TAR)));
    assert_eq!(asked(&work), 6);
}

/// a refused listing takes the names GEO gives every series, each kept once its first byte comes
/// back. NCBI cannot be made to refuse a listing, so this asks for those names as such a listing
/// would: each of GSE1000's is served, and a series of several platforms has no one matrix.
#[test]
#[ignore = "live: talks to ftp.ncbi.nlm.nih.gov"]
fn live_geo_names() {
    let (net, work) = crate::source::recorded::live(Vec::new());
    let series = accession("GSE1000");
    let top = series.folder().unwrap();
    for (folder, name) in series.named_by_rule() {
        let link = series.link(&top, folder, &name).unwrap();
        assert!(block_on(series.serves(&net, &link)).unwrap(), "{link}");
    }
    let several = accession("GSE15745");
    let top = several.folder().unwrap();
    let matrix = several
        .link(&top, "matrix", "GSE15745_series_matrix.txt.gz")
        .unwrap();
    assert!(!block_on(several.serves(&net, &matrix)).unwrap());
    eprintln!("geo names: {} requests", asked(&work));
}
