use std::time::Instant;

use super::*;
use crate::source::recorded::{Recorded, asked, block_on, fixture_bytes};

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

/// a snapshot of more files than one dataset may list is refused, naming its page at that
/// snapshot to download folders from.
#[test]
fn a_snapshot_of_more_files_than_allowed_is_refused() {
    let error = listed("openneuro:ds000001@1.0.0", &ds000001().most(5)).unwrap_err();
    assert_eq!(error.kind, crate::error::Kind::SourceInvalid);
    assert_eq!(
        error.message,
        "openneuro:ds000001@1.0.0 lists more than 5 files, the most this run lists for one dataset"
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "download the folders you need from https://openneuro.org/datasets/ds000001/versions/1.0.0, or raise the limit with `fetchloom settings set most_files <count>` or FETCHLOOM_MOST_FILES; each listed file takes about 2 KiB of memory"
        )
    );
}

/// each folder is one query with the header openneuro asks for. annexed files get their S3 link at
/// the snapshot's version, git's go by their object, and `DataLad`'s folder is left out.
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

/// a reference without a tag asks for the newest snapshot first.
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

/// a key states its digest with or without the extension an `E` backend adds, and its size. a
/// chunk's key and other backends state no digest, and anything else is no key.
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

/// a missing dataset, a private one, a missing snapshot and any other error are told in openneuro's
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

/// the cases are another snapshot, a file under no key, a key whose size differs from the
/// listing's, a folder without a usable id, one listed inside itself, and a name with a `/`.
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

/// git names a folder by what it holds, so two folders with the same files share an id, as two
/// subjects' `tmp` folders do in openneuro:ds004194@3.0.0. that is no loop: each is listed.
#[test]
fn folders_that_hold_the_same_files_are_each_listed() {
    let folder_entry = |name: &str| {
        format!(
            r#"{{"id":"{ANAT}","filename":"{name}","size":0,"directory":true,"annexed":false,"urls":[]}}"#
        )
    };
    let recorded = Recorded::default()
        .on(
            &top("ds000001", "1.0.0"),
            200,
            format!(
                r#"{{"data":{{"snapshot":{{"tag":"1.0.0","deprecated":null,"description":null,"files":[{},{}]}}}}}}"#,
                folder_entry("sub-01"),
                folder_entry("sub-02")
            ),
        )
        .on(
            &folder("ds000001", "1.0.0", ANAT),
            200,
            r#"{"data":{"snapshot":{"files":[{"id":"b6aff27bdd11bd66b06f01339be7cd794c56f586","filename":"notes.txt","size":5,"directory":false,"annexed":false,"urls":[]}]}}}"#,
        );
    let listing = listed("openneuro:ds000001@1.0.0", &recorded).unwrap();
    let mut paths: Vec<&str> = listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    paths.sort_unstable();
    assert_eq!(paths, ["sub-01/notes.txt", "sub-02/notes.txt"]);
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
    let (net, work) = crate::source::recorded::live(Vec::new());
    let start = Instant::now();
    let reference = Reference::parse("openneuro:ds000001@1.0.0")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "openneuro: {} files, {} requests, {:?}",
        listing.files.len(),
        asked(&work),
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

/// the folders on the way are listed too, when they hold every file wanted. a missing folder has
/// the whole snapshot listed, so a pattern matching nothing is told against it all.
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

/// a snapshot shaped as a study is: `subjects` folders at the top, each holding `anat` and
/// `func`, each of those one file, with every folder's id made from its path.
fn study(subjects: usize) -> Recorded {
    // git names each folder by what it holds; here each id is its own number
    let tree = |path: &str| {
        let (subject, kind) = path.split_once('/').unwrap_or((path, ""));
        let number: usize = subject.trim_start_matches("sub-").parse().unwrap();
        let kind = match kind {
            "" => 0,
            "anat" => 1,
            _ => 2,
        };
        format!("{:040x}", 1_000 + number * 10 + kind)
    };
    let entry = |name: &str, id: &str, directory: bool| {
        serde_json::json!({"id": id, "filename": name, "size": if directory { 0 } else { 9 },
            "directory": directory, "annexed": false,
            "urls": if directory { vec![] } else { vec![format!("https://openneuro.org/crn/datasets/ds000002/objects/{id}?filename={name}")] }})
    };
    let answer = |files: Vec<serde_json::Value>| {
        serde_json::to_vec(&serde_json::json!({"data": {"snapshot": {"files": files}}})).unwrap()
    };
    let mut recorded = Recorded::default();
    let mut top = vec![entry("README", &format!("{:040x}", 1), false)];
    for subject in 1..=subjects {
        let name = format!("sub-{subject:02}");
        top.push(entry(&name, &tree(&name), true));
        let kinds = ["anat", "func"].map(|kind| (kind, format!("{name}/{kind}")));
        let inside = kinds
            .iter()
            .map(|(kind, path)| entry(kind, &tree(path), true))
            .collect();
        recorded = recorded.on(
            &folder("ds000002", "1.0.0", &tree(&name)),
            200,
            answer(inside),
        );
        for (kind, path) in &kinds {
            let file = entry(
                &format!("{name}_{kind}.tsv"),
                &format!("{:040x}", 500_000 + subject * 10 + kind.len()),
                false,
            );
            recorded = recorded.on(
                &folder("ds000002", "1.0.0", &tree(path)),
                200,
                answer(vec![file]),
            );
        }
    }
    let described = serde_json::json!({"data": {"snapshot": {"tag": "1.0.0", "deprecated": null,
        "description": {"Name": "a study", "License": "CC0", "DatasetDOI": null}, "files": top}}});
    recorded.on(
        &top_of("ds000002"),
        200,
        serde_json::to_vec(&described).unwrap(),
    )
}

fn top_of(id: &str) -> String {
    top(id, "1.0.0")
}

/// the rounds of requests that wait on each other to list `recorded`'s study, each answer
/// taking a second of a paused clock, and the requests.
fn study_rounds(recorded: &Recorded) -> (Result<Listing, Error>, u64, usize) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    let reference = Reference::parse("openneuro:ds000002@1.0.0")
        .unwrap()
        .reference;
    runtime.block_on(async {
        let start = tokio::time::Instant::now();
        let listed = reference.resolve(recorded).await;
        (listed, start.elapsed().as_secs(), recorded.asked().len())
    })
}

/// a study of 16 subjects takes a request a folder either way, 49, but asking four folders at a
/// time waits through 13 rounds where one at a time waits through all 49, and lists the same.
#[test]
fn a_study_asking_four_folders_at_a_time_waits_through_a_quarter_of_the_rounds() {
    let second = std::time::Duration::from_secs(1);
    let (alone, alone_rounds, alone_asked) = study_rounds(&study(16).holding(second));
    let (together, together_rounds, together_asked) =
        study_rounds(&study(16).holding(second).window(4));
    let (alone, together) = (alone.unwrap(), together.unwrap());
    assert_eq!(alone.files.len(), 33);
    assert_eq!(format!("{alone:?}"), format!("{together:?}"));
    assert_eq!((alone_asked, together_asked), (49, 49));
    assert_eq!((alone_rounds, together_rounds), (49, 13));
}

/// a study past the most files a run lists ends in the same refusal either way, and the walk
/// asking folders together asks at most the folder the walk one at a time stops before and the
/// three asked beside it.
#[test]
fn a_study_past_the_most_files_ends_alike_asking_folders_together() {
    let (alone, _, alone_asked) = study_rounds(&study(16).most(12));
    let (together, _, together_asked) = study_rounds(&study(16).most(12).window(4));
    let (alone, together) = (alone.unwrap_err(), together.unwrap_err());
    assert_eq!(
        (alone.kind, alone.message.as_str()),
        (together.kind, together.message.as_str())
    );
    assert!(
        together_asked <= alone_asked + 4,
        "one at a time asked {alone_asked}, together {together_asked}"
    );
}

/// the link of one query of every file of `id`'s snapshot `tag`, as a listing of a snapshot
/// data.lock pins whole asks it.
fn whole(id: &str, tag: &str) -> String {
    link(&format!(
        r#"query{{snapshot(datasetId:"{id}",tag:"{tag}"){{tag deprecated{{reason}} description{{Name License DatasetDOI}} files(recursive:true){{id filename size directory annexed urls}}}}}}"#
    ))
}

/// what data.lock pins of `listing` once a sync listed it: every file but those a listing keeps
/// only when selected.
fn pin_of(listing: &Listing) -> Pin {
    Pin::of(
        listing
            .files
            .iter()
            .map(|file| file.path.as_str())
            .filter(|path| !listing.kept_only_if_selected(path)),
    )
}

/// what data.lock pins of ds000001's cut once a sync listed it.
fn ds000001_pin() -> Pin {
    pin_of(&listed("openneuro:ds000001@1.0.0", &ds000001()).unwrap())
}

/// ds000001's cut with its one-query answer, openneuro's own recursive answer for snapshot 1.0.0
/// cut to the files the cut's folders hold, in the order openneuro sent them, and data.lock
/// pinning `pin` of that snapshot.
fn ds000001_whole(pin: Pin) -> Recorded {
    ds000001()
        .fixture(&whole("ds000001", "1.0.0"), "openneuro/whole.json")
        .pinned_whole("openneuro:ds000001@1.0.0", pin)
}

/// ds000001's one-query answer, its snapshot as `change` leaves it.
fn whole_answer(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut answer: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("openneuro/whole.json")).unwrap();
    change(&mut answer["data"]["snapshot"]);
    serde_json::to_vec(&answer).unwrap()
}

/// the files of a one-query answer's snapshot.
fn answered(snapshot: &mut serde_json::Value) -> &mut Vec<serde_json::Value> {
    snapshot["files"].as_array_mut().unwrap()
}

/// the header each query carries, as a recorded network notes it after the link.
const PREFLIGHT_SAID: &str = " apollo-require-preflight: true";

/// a snapshot data.lock pins whole lists, in one query, the files, in the walk's order, and the
/// title, license, DOI and warnings its walk lists, a query a folder, from the same snapshot:
/// openneuro answers the folders from a stack, sub-01's func before its anat, which the walk lists
/// the other way round.
#[test]
fn a_pinned_snapshot_lists_in_one_query_what_its_walk_lists() {
    let walked = ds000001();
    let walk = listed("openneuro:ds000001@1.0.0", &walked).unwrap();
    let recorded = ds000001_whole(pin_of(&walk));
    let whole_listed = listed("openneuro:ds000001@1.0.0", &recorded).unwrap();
    assert_eq!(format!("{whole_listed:?}"), format!("{walk:?}"));
    assert_eq!(walked.asked().len(), 4);
    assert_eq!(
        recorded.asked(),
        [whole("ds000001", "1.0.0") + PREFLIGHT_SAID]
    );
}

/// the walk lists the snapshot where data.lock pins nothing whole, pins another snapshot, pins
/// more files than one query takes or than half the most a dataset may list, or select patterns
/// narrow what is listed.
#[test]
fn a_snapshot_the_one_query_gate_leaves_out_is_walked() {
    let anat = [crate::glob::Glob::new("sub-01/anat/**").unwrap()];
    let unpinned = ds000001().fixture(&whole("ds000001", "1.0.0"), "openneuro/whole.json");
    let elsewhere = unpinned
        .clone()
        .pinned_whole("openneuro:ds000001@0.9.0", ds000001_pin());
    let many: Vec<String> = (0..20_001).map(|file| format!("file-{file}")).collect();
    let past_one_query = Pin::of(many.iter().map(String::as_str));
    let cases: [(&str, Recorded, &[crate::glob::Glob]); 5] = [
        ("nothing pinned", unpinned, &[]),
        ("another snapshot pinned", elsewhere, &[]),
        ("past one query's most", ds000001_whole(past_one_query), &[]),
        (
            "past half the most files",
            ds000001_whole(ds000001_pin()).most(19),
            &[],
        ),
        (
            "narrowed by select patterns",
            ds000001_whole(ds000001_pin()),
            &anat,
        ),
    ];
    for (case, recorded, select) in cases {
        let reference = Reference::parse("openneuro:ds000001@1.0.0")
            .unwrap()
            .reference;
        block_on(reference.resolve_in(&recorded, select)).unwrap();
        let asked = recorded.asked();
        assert_eq!(
            asked[0],
            top("ds000001", "1.0.0") + PREFLIGHT_SAID,
            "{case}"
        );
        assert!(
            !asked.contains(&(whole("ds000001", "1.0.0") + PREFLIGHT_SAID)),
            "{case}: {asked:#?}"
        );
    }
}

/// a one-query answer that cannot be used is followed by the walk, whose listing stands as if the
/// query had not been asked, and the run's other listings of the snapshot walk at once: an error,
/// a status, another snapshot, an answer of another shape, a folder named or the top folder alone,
/// an entry the walk would judge, and an answer that holds other files than data.lock pins, cut
/// short as openneuro answers without a folder whose tree it could not read, or a file listed
/// twice in another's place.
#[test]
fn an_unusable_one_query_answer_is_followed_by_the_walk() {
    let walk = listed("openneuro:ds000001@1.0.0", &ds000001()).unwrap();
    let link = whole("ds000001", "1.0.0");
    let refused =
        br#"{"errors":[{"message":"Unknown argument \"recursive\" on field \"Snapshot.files\"."}]}"#;
    let folder = serde_json::json!({"id": SUB, "filename": "sub-01", "size": 0, "directory": true,
        "annexed": false, "urls": []});
    let cases: Vec<(&str, Vec<u8>, u16)> = vec![
        ("an error", refused.to_vec(), 200),
        ("a status", refused.to_vec(), 400),
        (
            "another snapshot",
            whole_answer(|snapshot| snapshot["tag"] = "1.0.1".into()),
            200,
        ),
        (
            "a folder named",
            whole_answer(|snapshot| answered(snapshot).insert(2, folder)),
            200,
        ),
        (
            "the top folder alone",
            fixture_bytes("openneuro/top.json"),
            200,
        ),
        (
            "an empty part of a path",
            whole_answer(|snapshot| {
                answered(snapshot)[8]["filename"] = "sub-01//sub-01_T1w.nii.gz".into();
            }),
            200,
        ),
        (
            "a size that is no number",
            whole_answer(|snapshot| answered(snapshot)[8]["size"] = "many".into()),
            200,
        ),
        (
            "an answer cut short",
            whole_answer(|snapshot| {
                answered(snapshot).retain(|file| {
                    !file["filename"]
                        .as_str()
                        .unwrap()
                        .starts_with("sub-01/anat/")
                });
            }),
            200,
        ),
        (
            "a file listed twice in another's place",
            whole_answer(|snapshot| {
                let files = answered(snapshot);
                files[9]["filename"] = files[8]["filename"].clone();
            }),
            200,
        ),
    ];
    for (case, body, status) in cases {
        let recorded = ds000001()
            .on(&link, status, body)
            .pinned_whole("openneuro:ds000001@1.0.0", pin_of(&walk));
        let listing = listed("openneuro:ds000001@1.0.0", &recorded).unwrap();
        assert_eq!(format!("{listing:?}"), format!("{walk:?}"), "{case}");
        let asked = recorded.asked();
        assert_eq!(
            asked.len(),
            5,
            "{case}: the one query, then the walk's four"
        );
        assert_eq!(asked[0], link.clone() + PREFLIGHT_SAID, "{case}");
        assert_eq!(recorded.pinned("openneuro:ds000001@1.0.0"), None, "{case}");
    }
}

/// a one query the walk's small queries need not fail as it failed, past the most an answer may
/// hold or a server's error through every retry, is followed by the walk, once: the run's next
/// listing of the snapshot walks at once.
#[test]
fn a_one_query_the_server_fails_is_followed_by_the_walk_once() {
    let walk = listed("openneuro:ds000001@1.0.0", &ds000001()).unwrap();
    let link = whole("ds000001", "1.0.0");
    let failures = [
        (
            "past the most an answer holds",
            crate::net::too_large_answer(&Url::parse(&link).unwrap()),
        ),
        (
            "a server's error through every retry",
            Error::new(
                Kind::NetworkStatus,
                "gave up on openneuro.org after 5 tries: openneuro.org answered 504",
            ),
        ),
    ];
    for (case, failure) in failures {
        let recorded = ds000001()
            .fail(&link, failure)
            .pinned_whole("openneuro:ds000001@1.0.0", pin_of(&walk));
        for listing in 0..2 {
            let listing_now = listed("openneuro:ds000001@1.0.0", &recorded).unwrap();
            assert_eq!(
                format!("{listing_now:?}"),
                format!("{walk:?}"),
                "{case}, listing {listing}"
            );
        }
        let asked = recorded.asked();
        assert_eq!(
            asked.len(),
            9,
            "{case}: the one query, then the walk's four, twice"
        );
        assert_eq!(asked[0], link.clone() + PREFLIGHT_SAID, "{case}");
        assert!(
            !asked[1..].contains(&(link.clone() + PREFLIGHT_SAID)),
            "{case}: {asked:#?}"
        );
    }
}

/// a one query a stop, a refusal, a rate limit, a lost network or a timeout ends, as any of them
/// would end the walk's first query, fails the listing with it and nothing more is asked; so does
/// a private dataset, in the words the walk's first query gets.
#[test]
fn a_one_query_ended_as_the_walks_first_query_would_be_fails_the_listing() {
    let link = whole("ds000001", "1.0.0");
    for kind in [
        Kind::Interrupted,
        Kind::NetworkRefused,
        Kind::NetworkRateLimited,
        Kind::NetworkUnreachable,
        Kind::OfflineMissing,
    ] {
        let ended = Error::new(kind, "the request ended");
        let walked = ds000001().fail(&top("ds000001", "1.0.0"), ended.clone());
        let walk = listed("openneuro:ds000001@1.0.0", &walked).unwrap_err();
        let recorded = ds000001()
            .fail(&link, ended)
            .pinned_whole("openneuro:ds000001@1.0.0", ds000001_pin());
        let error = listed("openneuro:ds000001@1.0.0", &recorded).unwrap_err();
        assert_eq!(error.kind, kind);
        assert_eq!(
            (error.kind, &error.message, &error.help),
            (walk.kind, &walk.message, &walk.help),
            "{kind:?}"
        );
        assert_eq!(recorded.asked().len(), 1, "{kind:?}");
    }
    let private = br#"{"errors":[{"message":"You do not have access to read this dataset."}]}"#;
    let walked = ds000001().on(&top("ds000001", "1.0.0"), 200, private.to_vec());
    let walk = listed("openneuro:ds000001@1.0.0", &walked).unwrap_err();
    let recorded = ds000001()
        .on(&link, 200, private.to_vec())
        .pinned_whole("openneuro:ds000001@1.0.0", ds000001_pin());
    let error = listed("openneuro:ds000001@1.0.0", &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        (error.kind, &error.message, &error.help),
        (walk.kind, &walk.message, &walk.help)
    );
    assert_eq!(recorded.asked().len(), 1);
}

/// a file the walk refuses, in both answers, is refused in the walk's own words after the one
/// query.
#[test]
fn a_file_the_walk_refuses_is_refused_in_its_words_after_the_one_query() {
    let mut said: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("openneuro/top.json")).unwrap();
    said["data"]["snapshot"]["files"][0]["size"] = "many".into();
    let walked = ds000001().on(
        &top("ds000001", "1.0.0"),
        200,
        serde_json::to_vec(&said).unwrap(),
    );
    let walk = listed("openneuro:ds000001@1.0.0", &walked).unwrap_err();
    let recorded = walked
        .clone()
        .on(
            &whole("ds000001", "1.0.0"),
            200,
            whole_answer(|snapshot| answered(snapshot)[0]["size"] = "many".into()),
        )
        .pinned_whole("openneuro:ds000001@1.0.0", ds000001_pin());
    let error = listed("openneuro:ds000001@1.0.0", &recorded).unwrap_err();
    assert_eq!(
        walk.message,
        "openneuro:ds000001@1.0.0 sent an invalid listing: the size of README is not a number"
    );
    assert_eq!(
        (error.kind, &error.message, &error.help),
        (walk.kind, &walk.message, &walk.help)
    );
    assert_eq!(recorded.asked().len(), 1 + walked.asked().len());
}

/// a snapshot past the most files one dataset may list, pinned under the gate by an older pin, is
/// walked after the one query, which refuses it as before.
#[test]
fn a_one_query_past_the_most_files_is_refused_as_the_walk_refuses() {
    let walked = ds000001().most(5);
    let walk = listed("openneuro:ds000001@1.0.0", &walked).unwrap_err();
    let recorded = walked
        .clone()
        .fixture(&whole("ds000001", "1.0.0"), "openneuro/whole.json")
        .pinned_whole(
            "openneuro:ds000001@1.0.0",
            Pin::of(["README", "dataset_description.json"]),
        );
    let error = listed("openneuro:ds000001@1.0.0", &recorded).unwrap_err();
    assert_eq!(
        (error.kind, &error.message, &error.help),
        (walk.kind, &walk.message, &walk.help)
    );
    let asked = recorded.asked();
    assert_eq!(asked[0], whole("ds000001", "1.0.0") + PREFLIGHT_SAID);
    assert_eq!(asked[1..], walked.asked()[..]);
}

/// a snapshot pinned whole and asked by its dataset's name lists in one query once the newest tag
/// is the pinned one: two requests, the newest tag and the one query.
#[test]
fn the_newest_snapshot_pinned_whole_lists_in_one_query() {
    let recorded =
        ds000001_whole(ds000001_pin()).fixture(&latest("ds000001"), "openneuro/latest.json");
    let listing = listed("openneuro:ds000001", &recorded).unwrap();
    assert_eq!(listing.resolved.to_string(), "openneuro:ds000001@1.0.0");
    assert_eq!(listing.files.len(), 10);
    assert_eq!(
        recorded.asked(),
        [
            latest("ds000001") + PREFLIGHT_SAID,
            whole("ds000001", "1.0.0") + PREFLIGHT_SAID
        ]
    );
}

/// a snapshot of ds000003 as openneuro keeps it: the top folder's entries, and each folder's by
/// its tree's id, each a name, an id and whether it is a folder, in the folder's own order.
#[derive(Default)]
struct Kept {
    top: Vec<(String, String, bool)>,
    trees: HashMap<String, Vec<(String, String, bool)>>,
}

impl Kept {
    /// an entry of an answer.
    fn entry(name: &str, id: &str, folder: bool) -> serde_json::Value {
        serde_json::json!({"id": id, "filename": name, "size": if folder { 0 } else { 7 },
            "directory": folder, "annexed": false, "urls": []})
    }

    /// the snapshot with `files` as its answer's files, its owners' reason to deprecate it, if
    /// any, as `deprecated`.
    fn snapshot(files: &[serde_json::Value], deprecated: Option<&str>) -> Vec<u8> {
        let deprecated = deprecated.map(|reason| serde_json::json!({"reason": reason}));
        serde_json::to_vec(&serde_json::json!({"data": {"snapshot": {"tag": "1.0.0",
            "deprecated": deprecated, "description": {"Name": "shapes", "License": "CC0",
            "DatasetDOI": null}, "files": files}}}))
        .unwrap()
    }

    /// the walk's answers: the top folder's, and a folder's for each tree.
    fn walked(&self, deprecated: Option<&str>) -> Recorded {
        let entries = |entries: &[(String, String, bool)]| {
            entries
                .iter()
                .map(|(name, id, folder)| Kept::entry(name, id, *folder))
                .collect::<Vec<_>>()
        };
        let mut recorded = Recorded::default().on(
            &top("ds000003", "1.0.0"),
            200,
            Kept::snapshot(&entries(&self.top), deprecated),
        );
        for (tree, inside) in &self.trees {
            let answer = serde_json::json!({"data": {"snapshot": {"files": entries(inside)}}});
            recorded = recorded.on(
                &folder("ds000003", "1.0.0", tree),
                200,
                serde_json::to_vec(&answer).unwrap(),
            );
        }
        recorded
    }

    /// openneuro's answer to one query of every file, as its server's `reconstructFromTrees`
    /// makes it from the same trees: from a stack of folders, the top first, each taken off
    /// putting on the folders it holds and answering its files, each by its whole path.
    fn whole(&self, deprecated: Option<&str>) -> Vec<u8> {
        let mut files = Vec::new();
        let mut stack = vec![(&self.top, String::new())];
        while let Some((entries, path)) = stack.pop() {
            for (name, id, folder) in entries {
                let at = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{path}/{name}")
                };
                if !*folder {
                    files.push(Kept::entry(&at, id, false));
                } else if let Some(inside) = self.trees.get(id) {
                    stack.push((inside, at));
                }
            }
        }
        Kept::snapshot(&files, deprecated)
    }
}

/// numbers drawn from a seed, by xorshift.
struct Draws(u64);

impl Draws {
    /// a number below `below`.
    fn below(&mut self, below: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % below as u64).unwrap()
    }
}

/// the shapes a [`grown`] snapshot took: a folder holding another folder's tree, an empty folder,
/// a file four folders down, and a folder whose files and folders interleave.
#[derive(Default)]
struct Shapes {
    shared: usize,
    empty: usize,
    deep: usize,
    interleaved: usize,
}

/// a snapshot of ds000003 grown from `seed`: in each folder up to five entries, files and folders
/// in any order, a `.gitattributes` among them now and then, folders up to five deep, some
/// holding a tree made before, as git names a folder by what it holds, some empty, and
/// `DataLad`'s folder at the top.
fn grown(seed: u64, shapes: &mut Shapes) -> Kept {
    /// the entries of a folder `depth` folders down, its trees kept in `kept`.
    fn entries(
        kept: &mut Kept,
        draws: &mut Draws,
        made: &mut Vec<String>,
        depth: usize,
        shapes: &mut Shapes,
    ) -> Vec<(String, String, bool)> {
        let count = draws.below(6);
        if count == 0 && depth > 0 {
            shapes.empty += 1;
        }
        let mut held = Vec::new();
        for at in 0..count {
            if depth < 5 && draws.below(3) == 0 {
                let id = if !made.is_empty() && draws.below(5) == 0 {
                    shapes.shared += 1;
                    made[draws.below(made.len())].clone()
                } else {
                    let inside = entries(kept, draws, made, depth + 1, shapes);
                    let id = format!("{:040x}", kept.trees.len() + 1);
                    kept.trees.insert(id.clone(), inside);
                    made.push(id.clone());
                    id
                };
                held.push((format!("d{at}"), id, true));
            } else {
                if depth == 4 {
                    shapes.deep += 1;
                }
                let name = if at == 0 && draws.below(4) == 0 {
                    ".gitattributes".to_owned()
                } else {
                    format!("f{at}")
                };
                let id = format!("{:040x}", 1_000_000 + draws.below(1_000_000));
                held.push((name, id, false));
            }
        }
        let folders: Vec<bool> = held.iter().map(|(_, _, folder)| *folder).collect();
        if folders
            .windows(3)
            .any(|three| three[0] != three[1] && three[1] != three[2])
        {
            shapes.interleaved += 1;
        }
        held
    }
    let mut draws = Draws(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let mut kept = Kept::default();
    let mut made = Vec::new();
    let mut top = entries(&mut kept, &mut draws, &mut made, 0, shapes);
    let datalad = format!("{:040x}", 999_999);
    kept.trees.insert(
        datalad.clone(),
        vec![("config".to_owned(), format!("{:040x}", 999_998), false)],
    );
    let at = draws.below(top.len() + 1);
    top.insert(at, (".datalad".to_owned(), datalad, true));
    kept.top = top;
    kept
}

/// for 400 snapshots of every shape [`grown`] makes, the one query lists what the walk lists, in
/// its order, with the same warnings, from openneuro's answer as its server makes it from the
/// same trees, in one request.
#[test]
fn the_one_query_lists_every_shape_of_snapshot_as_the_walk_does() {
    let mut shapes = Shapes::default();
    let mut walked_folders = 0;
    for seed in 0..400 {
        let kept = grown(seed, &mut shapes);
        let deprecated = (seed % 7 == 0).then_some("a newer snapshot fixes its events");
        let walked = kept.walked(deprecated);
        let walk = listed("openneuro:ds000003@1.0.0", &walked).unwrap();
        if walked.asked().len() >= 3 {
            walked_folders += 1;
        }
        let recorded = walked
            .clone()
            .on(&whole("ds000003", "1.0.0"), 200, kept.whole(deprecated))
            .pinned_whole("openneuro:ds000003@1.0.0", pin_of(&walk));
        let listing = listed("openneuro:ds000003@1.0.0", &recorded).unwrap();
        assert_eq!(format!("{listing:?}"), format!("{walk:?}"), "seed {seed}");
        assert_eq!(recorded.asked().len(), 1, "seed {seed}");
    }
    let Shapes {
        shared,
        empty,
        deep,
        interleaved,
    } = shapes;
    assert!(
        shared > 40 && empty > 40 && deep > 40 && interleaved > 40 && walked_folders > 100,
        "shared {shared}, empty {empty}, deep {deep}, interleaved {interleaved}, walked two folders \
         or more {walked_folders}"
    );
}

/// an answer of more folders and files than half the most files is read no further and walked,
/// though data.lock pins it whole under the gate, as one file 15 folders down beside ten at the
/// top makes it at 26 for the most of 51; at 52 it lists in one query.
#[test]
fn a_one_query_past_half_the_most_files_with_its_folders_is_walked() {
    let mut top: Vec<(String, String, bool)> = (0..10)
        .map(|file| (format!("f{file}"), format!("{:040x}", 500 + file), false))
        .collect();
    let mut trees = HashMap::new();
    let mut inside = vec![("x".to_owned(), format!("{:040x}", 499), false)];
    for depth in (0..15).rev() {
        let id = format!("{:040x}", depth + 1);
        trees.insert(id.clone(), inside);
        inside = vec![("d".to_owned(), id, true)];
    }
    top.extend(inside);
    let kept = Kept { top, trees };
    for (most, one_query) in [(51, false), (52, true)] {
        let walked = kept.walked(None).most(most);
        let walk = listed("openneuro:ds000003@1.0.0", &walked).unwrap();
        assert_eq!(walk.files.len(), 11);
        let recorded = walked
            .clone()
            .on(&whole("ds000003", "1.0.0"), 200, kept.whole(None))
            .pinned_whole("openneuro:ds000003@1.0.0", pin_of(&walk));
        let listing = listed("openneuro:ds000003@1.0.0", &recorded).unwrap();
        assert_eq!(format!("{listing:?}"), format!("{walk:?}"), "most {most}");
        let asked = recorded.asked().len();
        let expected = if one_query {
            1
        } else {
            1 + walked.asked().len()
        };
        assert_eq!(asked, expected, "most {most}");
    }
}

/// a folder holding no file at any depth, as git's empty tree, leaves no trace in openneuro's one
/// query answer, so it counts toward the most files only in the walk: the walk refuses a snapshot
/// of one file beside 25 such folders where the most is 20, and the one query lists its file.
#[test]
fn folders_holding_no_file_count_toward_the_most_only_in_the_walk() {
    let empty = "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_owned();
    let mut top: Vec<(String, String, bool)> = (0..25)
        .map(|at| (format!("d{at:02}"), empty.clone(), true))
        .collect();
    top.push(("f".to_owned(), format!("{:040x}", 2), false));
    let kept = Kept {
        top,
        trees: HashMap::from([(empty, Vec::new())]),
    };
    let walk = listed("openneuro:ds000003@1.0.0", &kept.walked(None).most(20)).unwrap_err();
    assert_eq!(
        walk.message,
        "openneuro:ds000003@1.0.0 lists more than 20 files, the most this run lists for one dataset"
    );
    let recorded = kept
        .walked(None)
        .most(20)
        .on(&whole("ds000003", "1.0.0"), 200, kept.whole(None))
        .pinned_whole("openneuro:ds000003@1.0.0", Pin::of(["f"]));
    let listing = listed("openneuro:ds000003@1.0.0", &recorded).unwrap();
    assert_eq!(paths(&listing), ["f"]);
    assert_eq!(recorded.asked().len(), 1);
}
