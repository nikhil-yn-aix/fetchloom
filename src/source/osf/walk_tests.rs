use super::*;
use crate::error::Kind;
use crate::source::Reference;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const NODE: &str = "https://api.osf.io/v2/nodes/ytpuq/?embed=license&embed=identifiers";
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

#[test]
fn repeated_pages_stop_before_another_request() {
    let body = changed_page(|value| value["links"]["next"] = FIRST.into());
    let recorded = paged(body);
    let error = block_on(Node::parse("ytpuq").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(recorded.asked(), [NODE, FIRST]);
}

#[test]
fn pages_cannot_leave_the_osf_api() {
    let body = changed_page(|value| value["links"]["next"] = "https://other.example/files".into());
    let recorded = paged(body);
    let error = block_on(Node::parse("ytpuq").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(recorded.asked(), [NODE, FIRST]);
}

#[test]
fn empty_pages_cannot_continue_forever() {
    let body = changed_page(|value| value["data"] = serde_json::json!([]));
    let recorded = paged(body);
    let error = block_on(Node::parse("ytpuq").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(recorded.asked(), [NODE, FIRST]);
}

#[test]
fn folders_must_provide_a_files_relationship() {
    let body = changed_page(|value| {
        value["data"][0]["relationships"]
            .as_object_mut()
            .unwrap()
            .remove("files");
    });
    let recorded = paged(body);
    let error = block_on(Node::parse("ytpuq").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(recorded.asked(), [NODE, FIRST]);
}

#[test]
fn escaping_folder_names_are_refused_before_descending() {
    let body = changed_page(|value| value["data"][0]["attributes"]["name"] = "../outside".into());
    let recorded = paged(body);
    let error = block_on(Node::parse("ytpuq").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(recorded.asked(), [NODE, FIRST]);
}
#[test]
fn paging_links_cannot_request_other_api_resources() {
    let target = "https://api.osf.io/v2/users/";
    let body = changed_page(|value| value["links"]["next"] = target.into());
    let recorded = paged(body).on(target, 200, b"{}".to_vec());
    let error = block_on(Node::parse("ytpuq").unwrap().resolve(&recorded)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(recorded.asked(), [NODE, FIRST]);
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
