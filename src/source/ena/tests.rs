use std::time::Instant;

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Access, Reference};

const TITLE: &str = "CGCI: Non-Hodgkin Lymphoma (NHL)";

fn count(id: &str) -> String {
    format!(
        "https://www.ebi.ac.uk/ena/portal/api/filereportcount?accession={id}&result=read_run&format=json"
    )
}

fn report(id: &str, runs: usize) -> String {
    format!(
        "https://www.ebi.ac.uk/ena/portal/api/filereport?accession={id}&result=read_run&fields={FIELDS}&format=json&limit={runs}"
    )
}

fn counted(runs: usize) -> Vec<u8> {
    format!(r#"{{"count":"{runs}"}}"#).into_bytes()
}

fn changed(fixture: &str, change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes(fixture)).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

/// ENA's answers for `id`: the count, then the report from `fixture`, changed by `change`.
fn answered(id: &str, runs: usize, body: Vec<u8>) -> Recorded {
    Recorded::default()
        .on(&count(id), 200, counted(runs))
        .on(&report(id, runs), 200, body)
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

#[test]
fn lists_a_runs_fastq_with_the_md5_and_size_ena_states() {
    let recorded = Recorded::default()
        .fixture(&count("SRR390728"), "ena/count.json")
        .fixture(&report("SRR390728", 1), "ena/run.json");
    let listing = block_on(accession("SRR390728").resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "ena:SRR390728");
    assert_eq!(listing.title.as_deref(), Some(TITLE));
    assert_eq!(listing.access, Access::Public);
    assert_eq!(listing.warnings, Vec::<String>::new());
    let file = |name: &str, size, md5| Remote {
        path: name.to_owned(),
        size: Some(size),
        sha256: None,
        md5: Md5::from_hex(md5),
        url: Url::parse(&format!(
            "https://ftp.sra.ebi.ac.uk/vol1/fastq/SRR390/SRR390728/{name}"
        ))
        .unwrap(),
        named_by_server: false,
        etag: None,
    };
    assert_eq!(
        listing.files,
        vec![
            file(
                "SRR390728_1.fastq.gz",
                101_304_405,
                "a04ed1f37028cda9486eec9726237796"
            ),
            file(
                "SRR390728_2.fastq.gz",
                101_858_469,
                "3d440434c968624acaa4f787d48efe2f"
            ),
        ]
    );
    assert_eq!(
        recorded.asked(),
        [count("SRR390728"), report("SRR390728", 1)]
    );
}

/// an experiment, a sample or a study may hold several runs, so each run's files sit in a
/// folder named by its run, in the order of their accessions, whatever order ENA sends.
#[test]
fn several_runs_each_get_a_folder_named_by_the_run() {
    let recorded = answered("SRX079566", 2, fixture_bytes("ena/experiment.json"));
    let listing = block_on(accession("SRX079566").resolve(&recorded)).unwrap();
    assert_eq!(
        paths(&listing),
        [
            "SRR292241/SRR292241_1.fastq.gz",
            "SRR292241/SRR292241_2.fastq.gz",
            "SRR390728/SRR390728_1.fastq.gz",
            "SRR390728/SRR390728_2.fastq.gz",
        ]
    );
}

#[test]
fn submitted_files_are_the_ones_the_submitter_sent() {
    let recorded = answered("ERR1160846", 1, fixture_bytes("ena/submitted.json"));
    let listing = block_on(accession("submitted/ERR1160846").resolve(&recorded)).unwrap();
    assert_eq!(listing.resolved.to_string(), "ena:submitted/ERR1160846");
    assert_eq!(
        listing.files,
        vec![Remote {
            path: "10317.BLANK.93.3E.r22.fastq.gz".to_owned(),
            size: Some(17324),
            sha256: None,
            md5: Md5::from_hex("5216fec6f0525034c3967855d575b9df"),
            url: Url::parse(
                "https://ftp.sra.ebi.ac.uk/vol1/run/ERR116/ERR1160846/10317.BLANK.93.3E.r22.fastq.gz"
            )
            .unwrap(),
            named_by_server: false,
            etag: None,
        }]
    );
}

/// a submitter's file name may hold `#`, as a CRAM from a sequencing lane's tag does; it stays
/// one name, and its link encodes it.
#[test]
fn a_file_name_with_a_hash_stays_one_name() {
    let body = changed("ena/submitted.json", |value| {
        value[0]["submitted_ftp"] =
            "ftp.sra.ebi.ac.uk/vol1/run/ERR132/ERR13251870/47404_4#31.cram".into();
    });
    let recorded = answered("ERR1160846", 1, body);
    let listing = block_on(accession("submitted/ERR1160846").resolve(&recorded)).unwrap();
    assert_eq!(listing.files[0].path, "47404_4#31.cram");
    assert_eq!(
        listing.files[0].url.as_str(),
        "https://ftp.sra.ebi.ac.uk/vol1/run/ERR132/ERR13251870/47404_4%2331.cram"
    );
}

/// five of the real rows of PRJNA172563, whose other runs are like them: three under dbGaP's
/// controlled access, two public.
#[test]
fn runs_under_controlled_access_are_left_out_and_told() {
    let recorded = answered("PRJNA172563", 5, fixture_bytes("ena/study.json"));
    let listing = block_on(accession("PRJNA172563").resolve(&recorded)).unwrap();
    assert_eq!(
        paths(&listing),
        [
            "SRR1236466/SRR1236466_1.fastq.gz",
            "SRR1236466/SRR1236466_2.fastq.gz",
            "SRR1236467/SRR1236467_1.fastq.gz",
            "SRR1236467/SRR1236467_2.fastq.gz",
        ]
    );
    assert_eq!(
        listing.warnings,
        ["3 of the 5 runs are under controlled access, so their files are left out"]
    );
}

#[test]
fn runs_all_under_controlled_access_say_where_ena_sends_people() {
    let body = changed("ena/study.json", |value| {
        value.as_array_mut().unwrap().truncate(3);
    });
    let recorded = answered("PRJNA172563", 3, body);
    let error = block_on(accession("PRJNA172563").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "the runs of PRJNA172563 are under controlled access"
    );
    let help = error.help.unwrap();
    assert!(
        help.contains("ENA sends people to dbGap for them"),
        "{help}"
    );
    assert!(error.need.is_none(), "no sign-in here lifts it");
}

#[test]
fn a_run_with_no_files_says_so() {
    let recorded = answered("DRR1002481", 1, fixture_bytes("ena/empty.json"));
    let error = block_on(accession("DRR1002481").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(error.message, "ENA lists no FASTQ files for ena:DRR1002481");
    assert_eq!(
        error.help.as_deref(),
        Some(
            "ENA has no files for its runs yet; check https://www.ebi.ac.uk/ena/browser/view/DRR1002481"
        )
    );
}

/// a run without the kind of files asked for names the kind it has, so the reference to change
/// to is in the message.
#[test]
fn runs_without_the_files_asked_for_name_what_they_have() {
    let recorded = answered("ERR1160846", 1, fixture_bytes("ena/submitted.json"));
    let error = block_on(accession("sra/ERR1160846").resolve(&recorded)).unwrap_err();
    assert_eq!(
        error.message,
        "ENA lists no SRA files for ena:sra/ERR1160846"
    );
    assert_eq!(
        error.help.as_deref(),
        Some("its runs have FASTQ files: use ena:ERR1160846")
    );

    let body = changed("ena/experiment.json", |value| {
        value[1]["fastq_ftp"] = "".into();
        value[1]["submitted_ftp"] = "ftp.sra.ebi.ac.uk/vol1/run/SRR292/SRR292241/reads.bam".into();
    });
    let recorded = answered("SRX079566", 2, body);
    let listing = block_on(accession("SRX079566").resolve(&recorded)).unwrap();
    assert_eq!(listing.files.len(), 2);
    assert_eq!(
        listing.warnings,
        [
            "1 of the 2 runs has no FASTQ files, so it is left out; ena:submitted/SRX079566 lists its submitted files"
        ]
    );
}

#[test]
fn an_accession_with_no_public_runs_is_unresolved_after_one_request() {
    let url = count("SRR0000000");
    let recorded = Recorded::default().on(&url, 200, counted(0));
    let error = block_on(accession("SRR0000000").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(error.message, "ENA has no public runs for SRR0000000");
    assert_eq!(recorded.asked(), [url]);
}

#[test]
fn an_accession_ena_refuses_is_invalid() {
    let url = count("SRR390728");
    let recorded = Recorded::default().on(&url, 400, fixture_bytes("ena/invalid.json"));
    let error = block_on(accession("SRR390728").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceInvalid);
    assert!(error.help.is_some());
}

/// ENA's report has no pages: it comes whole or not at all. so a study with more runs than a
/// listing may hold is refused on its count alone, before the report is asked for.
#[test]
fn a_study_larger_than_a_listing_may_hold_is_refused_before_its_report() {
    let url = count("PRJNA172563");
    let recorded = Recorded::default().on(&url, 200, counted(100_001));
    let error = block_on(accession("PRJNA172563").resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(
        error.message.contains("more than 100,000 files"),
        "{}",
        error.message
    );
    assert_eq!(recorded.asked(), [url]);
}

/// the report must hold the runs ENA counted: no fewer, which would drop runs unseen, and no
/// more than it was asked for.
#[test]
fn a_report_that_does_not_match_its_count_is_refused() {
    for (runs, body, reason) in [
        (
            3,
            fixture_bytes("ena/run.json"),
            "ends after 1 of the 3 runs",
        ),
        (
            1,
            fixture_bytes("ena/experiment.json"),
            "more than the 1 run it was asked for",
        ),
        (1, b"[]".to_vec(), "ends after 0 of the 1 run ENA counts"),
    ] {
        let recorded = answered("SRR390728", runs, body);
        let error = block_on(accession("SRR390728").resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "{reason}");
        assert!(error.message.contains(reason), "{}", error.message);
    }
}

#[test]
fn a_report_that_cannot_be_trusted_says_why() {
    type Change = fn(&mut serde_json::Value);
    let rows: [(&str, Change, &str); 6] = [
        (
            "a file on another host",
            |value| {
                value[0]["fastq_ftp"] = "example.org/vol1/SRR390728_1.fastq.gz;ftp.sra.ebi.ac.uk/vol1/fastq/SRR390/SRR390728/SRR390728_2.fastq.gz".into();
            },
            "gives a file away from ENA",
        ),
        (
            "a path that climbs out",
            |value| {
                value[0]["fastq_ftp"] = "ftp.sra.ebi.ac.uk/vol1/../x.fastq.gz;ftp.sra.ebi.ac.uk/vol1/fastq/SRR390/SRR390728/SRR390728_2.fastq.gz".into();
            },
            "gives a file away from ENA",
        ),
        (
            "an MD5 that is not hex",
            |value| value[0]["fastq_md5"] = "broken;3d440434c968624acaa4f787d48efe2f".into(),
            "is not hex",
        ),
        (
            "a size that is no number",
            |value| value[0]["fastq_bytes"] = "large;101858469".into(),
            "is no number",
        ),
        (
            "fewer hashes than files",
            |value| value[0]["fastq_md5"] = "a04ed1f37028cda9486eec9726237796".into(),
            "gives 2 files but not as many sizes and hashes",
        ),
        (
            "a run without a run accession",
            |value| value[0]["run_accession"] = "".into(),
            "has no run accession",
        ),
    ];
    for (row, change, reason) in rows {
        let recorded = answered("SRR390728", 1, changed("ena/run.json", change));
        let Err(error) = block_on(accession("SRR390728").resolve(&recorded)) else {
            panic!("{row}: the report was used");
        };
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
        assert!(error.help.is_some(), "{row}");
    }
    let recorded =
        Recorded::default().on(&count("SRR390728"), 200, br#"{"count":"many"}"#.to_vec());
    let error = block_on(accession("SRR390728").resolve(&recorded)).unwrap_err();
    assert!(error.message.contains("the count of runs is no number"));
}

/// every accession ENA's report takes for runs, written as ENA writes it, and the pages of ENA's
/// browser that show one. other accessions, and other ebi.ac.uk links, are not read as runs.
#[test]
fn accessions_and_browser_pages_name_runs_and_other_forms_are_refused() {
    for (text, expected) in [
        ("ena:ERR1160846", "ena:ERR1160846"),
        ("ena:DRR1002481", "ena:DRR1002481"),
        ("ena:SAMN00630374", "ena:SAMN00630374"),
        ("ena:samea3484362", "ena:SAMEA3484362"),
        ("ena:SAMD00012345", "ena:SAMD00012345"),
        ("ena:SRS212581", "ena:SRS212581"),
        ("ena:PRJEB102639", "ena:PRJEB102639"),
        ("ena:PRJDB41547", "ena:PRJDB41547"),
        ("ena:SRP020237", "ena:SRP020237"),
        ("ena:sra/SRR390728", "ena:sra/SRR390728"),
        ("ena:SUBMITTED/ERX079566", "ena:submitted/ERX079566"),
        (
            "https://www.ebi.ac.uk/ena/data/view/SRR390728",
            "ena:SRR390728",
        ),
        (
            "https://ebi.ac.uk/ena/browser/view/srr390728",
            "ena:SRR390728",
        ),
    ] {
        let parsed = Reference::parse(text).unwrap().reference;
        assert_eq!(parsed.to_string(), expected, "{text}");
        assert_eq!(Reference::parse(expected).unwrap().reference, parsed);
    }
    for text in [
        "ena:SRA123456",
        "ena:9606",
        "ena:SRR123",
        "ena:SAMN123",
        "ena:PRJ123",
        "ena:bam/SRR390728",
        "ena:",
        "https://www.ebi.ac.uk/ena/browser/view/MN908947.3",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
    }
    for link in [
        "https://www.ebi.ac.uk/ena/browser/api/fasta/MN908947.3",
        "https://ftp.sra.ebi.ac.uk/vol1/fastq/SRR390/SRR390728/SRR390728_1.fastq.gz",
    ] {
        assert_eq!(
            Reference::parse(link).unwrap().reference,
            Reference::Url(Url::parse(link).unwrap()),
            "{link}"
        );
    }
}

#[test]
#[ignore = "live: talks to www.ebi.ac.uk"]
fn live_ena() {
    let (net, work) = crate::source::recorded::live(Vec::new());
    let start = Instant::now();
    let reference = Reference::parse("ena:ERR1160846").unwrap().reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "ena: {} files, {} requests, {:?}",
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    assert_eq!(listing.files.len(), 1);
    assert_eq!(listing.files[0].path, "ERR1160846.fastq.gz");
    assert_eq!(
        listing.files[0].md5,
        Md5::from_hex("5924f20ef547ebdfed7cad795bbab6e6")
    );
    assert_eq!(work.counts().requests, 2);
}
