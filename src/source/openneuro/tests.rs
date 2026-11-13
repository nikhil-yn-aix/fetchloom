use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const SUB: &str = "db72baeb3665a309db35d6ef649e117eb45a8a87";
const ANAT: &str = "26d2ff4f763308d462f151c53e3e06f6b7d2aaaa";
const FUNC: &str = "4113b1609e2b0ea2e0a04e0800204d2ee6aab578";

/// the link of `query` to openneuro's API.
fn link(query: &str) -> String {
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("query", query)
        .finish();
    format!("{API}?{encoded}")
}

fn top(id: &str, tag: &str) -> String {
    link(&format!(
        r#"query{{snapshot(datasetId:"{id}",tag:"{tag}"){{tag deprecated{{reason}} description{{Name License DatasetDOI}} files{{id filename size directory annexed urls}}}}}}"#
    ))
}

fn folder(id: &str, tag: &str, tree: &str) -> String {
    link(&format!(
        r#"query{{snapshot(datasetId:"{id}",tag:"{tag}"){{files(tree:"{tree}"){{id filename size directory annexed urls}}}}}}"#
    ))
}

fn latest(id: &str) -> String {
    link(&format!(
        r#"query{{dataset(id:"{id}"){{latestSnapshot{{tag}}}}}}"#
    ))
}

/// snapshot 1.0.0 of ds000001, cut to two files at the top, `DataLad`'s folder and sub-01.
fn ds000001() -> Recorded {
    Recorded::default()
        .fixture(&top("ds000001", "1.0.0"), "openneuro/top.json")
        .fixture(&folder("ds000001", "1.0.0", SUB), "openneuro/sub-01.json")
        .fixture(&folder("ds000001", "1.0.0", ANAT), "openneuro/anat.json")
        .fixture(&folder("ds000001", "1.0.0", FUNC), "openneuro/func.json")
}

fn listed(reference: &str, recorded: &Recorded) -> Result<Listing, Error> {
    block_on(Reference::parse(reference)?.reference.resolve(recorded))
}

/// folder by folder, each query with the header openneuro asks for, with their S3 link at the
/// snapshot's version; files git keeps go by their object, and no `DataLad` folder is listed.
#[test]
fn a_snapshot_lists_its_files_with_the_digests_their_annex_keys_state() {
    let recorded = ds000001();
    let listing = listed("openneuro:ds000001@1.0.0", &recorded).unwrap();
    assert_eq!(listing.resolved.to_string(), "openneuro:ds000001@1.0.0");
    assert_eq!(
        listing.title.as_deref(),
        Some("Balloon Analog Risk-taking Task")
    );
    assert_eq!(listing.license.as_deref(), Some("CC0"));
    assert_eq!(
        listing.doi.as_deref(),
        Some("10.18112/openneuro.ds000001.v1.0.0")
    );
    assert_eq!(listing.files.len(), 10);
    let scan = listing
        .files
        .iter()
        .find(|file| file.path == "sub-01/anat/sub-01_T1w.nii.gz")
        .unwrap();
    assert_eq!(scan.size, Some(5_663_237));
    assert_eq!(scan.md5, Md5::from_hex("4608ffbd6b78ce3a325eb338fa556589"));
    assert_eq!(scan.sha256, None);
    assert_eq!(
        scan.url.as_str(),
        "https://s3.amazonaws.com/openneuro.org/ds000001/sub-01/anat/sub-01_T1w.nii.gz?versionId=SHOhSfza0LCFpgctRH3sMZ8iwAGSUwIy"
    );
    let readme = listing
        .files
        .iter()
        .find(|file| file.path == "README")
        .unwrap();
    assert_eq!(
        (readme.size, readme.md5, readme.sha256),
        (Some(1175), None, None)
    );
    assert_eq!(
        readme.url.as_str(),
        "https://openneuro.org/crn/datasets/ds000001/objects/b6aff27bdd11bd66b06f01339be7cd794c56f586?filename=README"
    );
    assert!(
        listing
            .files
            .iter()
            .all(|file| !file.path.starts_with(".datalad"))
    );
    let preflight = " apollo-require-preflight: true";
    assert_eq!(
        recorded.asked(),
        [
            top("ds000001", "1.0.0") + preflight,
            folder("ds000001", "1.0.0", SUB) + preflight,
            folder("ds000001", "1.0.0", ANAT) + preflight,
            folder("ds000001", "1.0.0", FUNC) + preflight,
        ]
    );
}

/// asked for first when a reference names no tag.
#[test]
fn the_newest_snapshot_is_found_and_pinned() {
    let recorded = ds000001().fixture(&latest("ds000001"), "openneuro/latest.json");
    let listing = listed("openneuro:ds000001", &recorded).unwrap();
    assert_eq!(listing.resolved.to_string(), "openneuro:ds000001@1.0.0");
    assert_eq!(
        recorded.asked()[0],
        latest("ds000001") + " apollo-require-preflight: true"
    );
}

/// with or without the extension an `E` backend adds, and the size. a chunk's key and other
/// backends state no digest, and anything else is no key.
#[test]
fn annex_keys_state_the_digest_of_their_backend() {
    let sha = "7cfba08d54744d61204226d47fb1e2004f89bf441af3d48c6a3c05738bcf7369";
    let md5 = "4608ffbd6b78ce3a325eb338fa556589";
    for (key, size, sha256, md5) in [
        (
            format!("SHA256E-s1757261330--{sha}.nii.gz"),
            Some(1_757_261_330),
            Some(sha),
            None,
        ),
        (format!("SHA256-s12--{sha}"), Some(12), Some(sha), None),
        (
            format!("MD5E-s5663237--{md5}.nii.gz"),
            Some(5_663_237),
            None,
            Some(md5),
        ),
        (
            format!("MD5-s1-m1700000000--{md5}"),
            Some(1),
            None,
            Some(md5),
        ),
        (format!("SHA256E-s9-S4-C1--{sha}.nii.gz"), None, None, None),
        ("WORM-s3-m1700000000--a.csv".to_owned(), Some(3), None, None),
    ] {
        assert_eq!(
            Key::parse(&key),
            Some(Key {
                size,
                sha256: sha256.and_then(Sha256::from_hex),
                md5: md5.and_then(Md5::from_hex),
            }),
            "{key}"
        );
    }
    for bad in [
        String::new(),
        format!("SHA256E-s12{sha}"),
        format!("SHA256E-sx--{sha}"),
        format!("SHA256E-s12--{}", &sha[1..]),
        format!("SHA256-s12--{sha}.nii.gz"),
        format!("MD5E-s1--{md5}x"),
    ] {
        assert_eq!(Key::parse(&bad), None, "{bad}");
    }
    let entries: Folder =
        serde_json::from_slice::<Answer<Folder>>(&fixture_bytes("openneuro/sha256.json"))
            .unwrap()
            .data
            .unwrap();
    let dataset = Dataset::parse("ds006001@1.1.0").unwrap();
    let entry = entries.snapshot.unwrap().files.remove(0);
    let file = file(&dataset, "sub-C1/anat/scan.nii.gz".to_owned(), &entry).unwrap();
    assert_eq!(file.sha256, Sha256::from_hex(sha));
    assert_eq!(file.size, Some(1_757_261_330));
    assert!(
        file.url
            .as_str()
            .starts_with("https://s3.amazonaws.com/openneuro.org/ds006001/")
            && file
                .url
                .query()
                .is_some_and(|query| query.starts_with("versionId=")),
        "{}",
        file.url
    );
}

/// a missing dataset, a private one, a missing snapshot and any other error, in openneuro's own
/// words, without the stack trace it sends with them.
#[test]
fn what_openneuro_says_went_wrong_is_told_as_such() {
    let missing = listed(
        "openneuro:ds999999",
        &Recorded::default().fixture(&latest("ds999999"), "openneuro/missing.json"),
    )
    .unwrap_err();
    assert_eq!(missing.kind, Kind::ReferenceUnresolved);
    assert_eq!(missing.message, "openneuro:ds999999 does not exist");
    let private = listed(
        "openneuro:ds004999",
        &Recorded::default().fixture(&latest("ds004999"), "openneuro/private.json"),
    )
    .unwrap_err();
    assert_eq!(private.kind, Kind::AuthRequired);
    assert!(
        private.message.starts_with("openneuro:ds004999 is private"),
        "{}",
        private.message
    );
    let unknown = listed(
        "openneuro:ds000001@9.9.9",
        &Recorded::default().fixture(&top("ds000001", "9.9.9"), "openneuro/no-snapshot.json"),
    )
    .unwrap_err();
    assert_eq!(unknown.kind, Kind::ReferenceUnresolved);
    assert_eq!(unknown.message, "openneuro:ds000001@9.9.9 does not exist");
    assert!(
        unknown
            .help
            .as_deref()
            .is_some_and(|help| help.contains("https://openneuro.org/datasets/ds000001")),
        "{:?}",
        unknown.help
    );
    let other = listed(
        "openneuro:ds000001@1.0.0",
        &Recorded::default().on(
            &top("ds000001", "1.0.0"),
            200,
            r#"{"errors":[{"message":"the worker is busy\n    at stack (frame.js:1:1)"}],"data":{"snapshot":null}}"#,
        ),
    )
    .unwrap_err();
    assert_eq!(other.kind, Kind::SourceInvalid);
    assert_eq!(
        other.message,
        "openneuro could not list openneuro:ds000001@1.0.0: it said \"the worker is busy\""
    );
}

/// another snapshot, a file under no key, a key whose size differs from the listing's, a folder
/// without a usable id, one listed inside itself, and a name with a `/`.
#[test]
fn a_listing_that_cannot_be_trusted_says_why() {
    let answer = |tag: &str, files: &str| {
        format!(
            r#"{{"data":{{"snapshot":{{"tag":"{tag}","deprecated":null,"description":null,"files":[{files}]}}}}}}"#
        )
    };
    let entry = |id: &str, name: &str, size: u64, directory: bool, annexed: bool| {
        format!(
            r#"{{"id":"{id}","filename":"{name}","size":{size},"directory":{directory},"annexed":{annexed},"urls":[]}}"#
        )
    };
    let md5 = "4608ffbd6b78ce3a325eb338fa556589";
    for (body, said) in [
        (answer("1.0.1", ""), "the answer names another snapshot"),
        (
            answer("1.0.0", &entry("not-a-key", "a.nii.gz", 1, false, true)),
            "a.nii.gz is kept under \"not-a-key\", which is no git-annex key",
        ),
        (
            answer(
                "1.0.0",
                &entry(
                    &format!("MD5E-s2--{md5}.nii.gz"),
                    "a.nii.gz",
                    1,
                    false,
                    true,
                ),
            ),
            "a.nii.gz is listed at 1 bytes, and its key says otherwise",
        ),
        (
            answer("1.0.0", &entry("x", "sub-01", 0, true, false)),
            "the folder sub-01 has no usable id",
        ),
        (
            answer("1.0.0", &entry(SUB, "a/b", 1, false, false)),
            "an entry in the top folder has the name \"a/b\"",
        ),
    ] {
        let recorded = Recorded::default().on(&top("ds000001", "1.0.0"), 200, body.clone());
        let error = listed("openneuro:ds000001@1.0.0", &recorded).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "{body}");
        assert_eq!(
            error.message,
            format!("openneuro:ds000001@1.0.0 sent an invalid listing: {said}")
        );
    }
    let looping = Recorded::default()
        .on(
            &top("ds000001", "1.0.0"),
            200,
            answer("1.0.0", &entry(SUB, "sub-01", 0, true, false)),
        )
        .on(
            &folder("ds000001", "1.0.0", SUB),
            200,
            format!(
                r#"{{"data":{{"snapshot":{{"files":[{}]}}}}}}"#,
                entry(SUB, "again", 0, true, false)
            ),
        );
    let error = listed("openneuro:ds000001@1.0.0", &looping).unwrap_err();
    assert_eq!(
        error.message,
        "openneuro:ds000001@1.0.0 sent an invalid listing: a folder lies in itself"
    );
}

/// a snapshot its owners deprecated is still read, and the listing says why they did.
#[test]
fn a_deprecated_snapshot_is_read_and_says_so() {
    let recorded = Recorded::default().on(
        &top("ds000001", "1.0.0"),
        200,
        r#"{"data":{"snapshot":{"tag":"1.0.0","deprecated":{"reason":"a scan was mislabeled"},"description":null,"files":[]}}}"#,
    );
    let listing = listed("openneuro:ds000001@1.0.0", &recorded).unwrap();
    assert_eq!(
        listing.warnings,
        ["the owners of openneuro:ds000001@1.0.0 deprecated it: a scan was mislabeled"]
    );
}

#[test]
#[ignore = "live: talks to openneuro.org"]
fn live_openneuro() {
    let work = Arc::new(crate::work::Work::default());
    let net = crate::net::Net::new(
        crate::net::Options {
            offline: crate::net::Offline::No,
            jobs: 4,
            credentials: Vec::new(),
            split: crate::net::Splitting::WhenSlow,
        },
        work.clone(),
        Arc::default(),
        Arc::new(|_| {}),
    );
    let start = Instant::now();
    let reference = Reference::parse("openneuro:ds000001@1.0.0")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "openneuro: {} files, {} requests, {:?}",
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    let scan = listing
        .files
        .iter()
        .find(|file| file.path == "sub-01/anat/sub-01_T1w.nii.gz")
        .unwrap();
    assert_eq!(scan.md5, Md5::from_hex("4608ffbd6b78ce3a325eb338fa556589"));
    assert!(scan.url.as_str().contains("versionId="), "{}", scan.url);
}

/// with the folders on the way to them, when they hold every file wanted. a folder the snapshot
/// lacks has the whole snapshot listed, so a pattern that matches nothing is told against it all.
#[test]
fn selected_folders_are_listed_without_the_rest() {
    let anat = [crate::glob::Glob::new("sub-01/anat/**").unwrap()];
    let recorded = ds000001();
    let reference = Reference::parse("openneuro:ds000001@1.0.0")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve_in(&recorded, &anat)).unwrap();
    assert_eq!(listing.within, ["sub-01/anat"]);
    let paths: Vec<&str> = listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "sub-01/anat/sub-01_T1w.nii.gz",
            "sub-01/anat/sub-01_inplaneT2.nii.gz"
        ]
    );
    assert_eq!(recorded.asked().len(), 3, "{:?}", recorded.asked());
    let lacking = [crate::glob::Glob::new("sub-99/**").unwrap()];
    let recorded = ds000001();
    let listing = block_on(reference.resolve_in(&recorded, &lacking)).unwrap();
    assert_eq!(listing.within, Vec::<String>::new());
    assert_eq!(listing.files.len(), 10);
}

/// the listing of snapshot 1.0.0 of ds000001 for the files `select` can match, and the queries
/// it took.
fn selecting(select: &[&str]) -> (Listing, Vec<String>) {
    let select: Vec<crate::glob::Glob> = select
        .iter()
        .map(|text| crate::glob::Glob::new(text).unwrap())
        .collect();
    let recorded = ds000001();
    let reference = Reference::parse("openneuro:ds000001@1.0.0")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve_in(&recorded, &select)).unwrap();
    (listing, recorded.asked())
}

fn paths(listing: &Listing) -> Vec<&str> {
    listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

/// `*.json` needs the top folder alone and `sub-*/anat/*` only the folders it matches; the listing
/// names what it covers.
#[test]
fn a_selection_lists_only_the_folders_its_patterns_can_reach() {
    let preflight = " apollo-require-preflight: true";
    let (at_top, asked) = selecting(&["*.json"]);
    assert_eq!(asked, [top("ds000001", "1.0.0") + preflight]);
    assert_eq!(at_top.within, [""]);
    assert_eq!(paths(&at_top), ["README", "dataset_description.json"]);
    let (anat, asked) = selecting(&["sub-*/anat/*"]);
    assert_eq!(
        asked,
        [
            top("ds000001", "1.0.0") + preflight,
            folder("ds000001", "1.0.0", SUB) + preflight,
            folder("ds000001", "1.0.0", ANAT) + preflight,
        ]
    );
    assert_eq!(anat.within, ["sub-*/anat"]);
    assert_eq!(
        paths(&anat),
        [
            "sub-01/anat/sub-01_T1w.nii.gz",
            "sub-01/anat/sub-01_inplaneT2.nii.gz"
        ]
    );
}
