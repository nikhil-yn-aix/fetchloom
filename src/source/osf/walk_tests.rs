use super::*;
use crate::error::Kind;
use crate::source::Reference;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const NODE: &str =
    "https://api.osf.io/v2/nodes/ytpuq/?embed=license&embed=identifiers&related_counts=children";
const FIRST: &str = "https://api.osf.io/v2/nodes/ytpuq/files/osfstorage/?page%5Bsize%5D=100";
const SECOND: &str = "https://api.osf.io/v2/nodes/ytpuq/files/osfstorage/?page=2&page%5Bsize%5D=2";
const LAST: &str = "https://api.osf.io/v2/nodes/ytpuq/files/osfstorage/?page=3&page%5Bsize%5D=2";
const SURVEYS: &str = "https://api.osf.io/v2/nodes/ytpuq/files/osfstorage/55a7dfbb8c5e4a66f26dcac4/?page%5Bsize%5D=100";
const AUDIT: &str = "https://api.osf.io/v2/nodes/ytpuq/files/osfstorage/55d386c68c5e4a2d0b438d39/?page%5Bsize%5D=100";

fn changed_page(change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut value = serde_json::from_slice(&fixture_bytes("osf/page1.json")).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn paged(first: Vec<u8>) -> Recorded {
    Recorded::default()
        .fixture(NODE, "osf/analysis-node.json")
        .on(FIRST, 200, first)
        .fixture(SECOND, "osf/page2.json")
        .fixture(LAST, "osf/page3.json")
        .fixture(SURVEYS, "osf/folder-55a7dfbb8c5e4a66f26dcac4.json")
        .fixture(AUDIT, "osf/folder-55d386c68c5e4a2d0b438d39.json")
}

#[test]
fn server_pages_control_the_walk_and_folders_keep_their_names() {
    let recorded = paged(fixture_bytes("osf/page1.json"));
    let reference = Reference::parse("osf:ytpuq").unwrap().reference;
    let listing = block_on(reference.resolve(&recorded)).unwrap();
    let paths: Vec<_> = listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "Researcher Results Survey.pdf",
            "rpp_data.csv",
            "rpp_data_codebook.csv",
            "Supplementary Surveys/Surprisingness : Importance Data and Averages.csv",
            "Supplementary Surveys/Replicator_Institution_prestige_Survey.csv",
            "Supplementary Surveys/Replicator_Institution_prestige_Survey.qsf",
            "Supplementary Surveys/MTurk_Institution_Prestige_Survey.csv",
            "Supplementary Surveys/MTurk_Institution_Prestige_Survey.qsf",
            "Supplementary Surveys/Prestige_Means_Per_University.xlsx",
            "Analysis Audit/R_check_results_all.csv",
            "Analysis Audit/R_check_results_individual_replications.zip",
            "Analysis Audit/RPP_R_Check_Controller.R",
        ]
    );
    assert_eq!(
        recorded.asked(),
        [NODE, FIRST, SECOND, LAST, SURVEYS, AUDIT]
    );
    assert!(
        listing
            .files
            .iter()
            .all(|file| file.url.query_pairs().any(|(key, _)| key == "version"))
    );
}

/// a page the files API sends that would make the walk repeat itself, leave the OSF API or a
/// project's folders, or go on with nothing to read, is refused for its reason before the walk
/// asks for anything it points to.
#[test]
fn a_page_that_would_mislead_the_walk_is_refused_before_another_request() {
    type Change = fn(&mut serde_json::Value);
    let users = "https://api.osf.io/v2/users/";
    let rows: [(&str, Change, &str); 6] = [
        (
            "a next page that repeats the first",
            |value| value["links"]["next"] = FIRST.into(),
            "sent an invalid listing: it repeated an earlier page",
        ),
        (
            "a next page on another site",
            |value| value["links"]["next"] = "https://other.example/files".into(),
            "leaves the OSF API",
        ),
        (
            "a next page elsewhere in the API",
            |value| value["links"]["next"] = "https://api.osf.io/v2/users/".into(),
            "leaves the OSF API",
        ),
        (
            "an empty page with a next one",
            |value| value["data"] = serde_json::json!([]),
            "an empty file page points to another page",
        ),
        (
            "a folder without its files",
            |value| {
                value["data"][0]["relationships"]
                    .as_object_mut()
                    .unwrap()
                    .remove("files");
            },
            "a folder has no files relationship",
        ),
        (
            "a folder named to climb out",
            |value| value["data"][0]["attributes"]["name"] = "../outside".into(),
            "lists \"../outside\", which fetchloom refuses",
        ),
    ];
    for (row, change, reason) in rows {
        let recorded = paged(changed_page(change)).on(users, 200, b"{}".to_vec());
        let Err(error) = block_on(Node::parse("ytpuq").unwrap().resolve(&recorded)) else {
            panic!("{row}: the walk went on");
        };
        assert_eq!(error.kind, Kind::SourceInvalid, "{row}");
        assert!(error.message.contains(reason), "{row}: {}", error.message);
        assert_eq!(recorded.asked(), [NODE, FIRST], "{row}");
    }
}

#[test]
fn nested_folders_join_each_parent_instead_of_trusting_materialized_paths() {
    let root = changed_page(|value| value["links"]["next"] = serde_json::Value::Null);
    let second: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("osf/page2.json")).unwrap();
    let mut surveys: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("osf/folder-55a7dfbb8c5e4a66f26dcac4.json")).unwrap();
    surveys["data"] = serde_json::json!([second["data"][0]]);
    let recorded = Recorded::default()
        .fixture(NODE, "osf/analysis-node.json")
        .on(FIRST, 200, root)
        .on(SURVEYS, 200, serde_json::to_vec(&surveys).unwrap())
        .fixture(AUDIT, "osf/folder-55d386c68c5e4a2d0b438d39.json");
    let listing = block_on(Node::parse("ytpuq").unwrap().resolve(&recorded)).unwrap();
    let paths: Vec<_> = listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(
        paths,
        [
            "Researcher Results Survey.pdf",
            "Supplementary Surveys/Analysis Audit/R_check_results_all.csv",
            "Supplementary Surveys/Analysis Audit/R_check_results_individual_replications.zip",
            "Supplementary Surveys/Analysis Audit/RPP_R_Check_Controller.R",
        ]
    );
    assert_eq!(recorded.asked(), [NODE, FIRST, SURVEYS, AUDIT]);
}

#[test]
fn unsafe_osf_paths_are_escaped_before_the_walk_stops() {
    for entry in [0, 1] {
        for control in ['\u{202e}', '\u{2066}', '\n', '\u{1b}'] {
            let path = format!("report{control}txt.exe");
            let body = changed_page(|value| {
                value["data"][entry]["attributes"]["name"] = path.clone().into();
            });
            let recorded = paged(body);
            let reference = Reference::parse("osf:ytpuq").unwrap().reference;
            let error = block_on(reference.resolve(&recorded)).unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid);
            assert!(
                !error.message.contains(control),
                "raw control in rejected path: {:?}",
                error.message
            );
            assert!(error.message.contains(&path.escape_debug().to_string()));
            assert_eq!(
                error.help.as_deref(),
                Some("ask the dataset's owner to rename it, then run the same command again")
            );
            assert_eq!(recorded.asked(), [NODE, FIRST]);
        }
    }
}
