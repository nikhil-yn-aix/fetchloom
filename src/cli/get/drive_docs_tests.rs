use super::*;
use crate::cli::args::Filter;
use crate::source::recorded::{Recorded, block_on};

fn listed(fixture: &str) -> (Parsed, Listing) {
    let parsed = Reference::parse("drive:folders/12zxlvJtuHFV6awc3AINaNHnfvRttPv0i").unwrap();
    let recorded = Recorded::default().fixture(
        "https://drive.google.com/embeddedfolderview?id=12zxlvJtuHFV6awc3AINaNHnfvRttPv0i",
        fixture,
    );
    let listing = block_on(parsed.reference.resolve(&recorded)).unwrap();
    (parsed, listing)
}

fn args(exclude: &[&str]) -> Get {
    Get {
        link: "drive:folders/12zxlvJtuHFV6awc3AINaNHnfvRttPv0i".to_owned(),
        output: None,
        filter: Filter {
            select: Vec::new(),
            exclude: exclude.iter().map(|value| (*value).to_owned()).collect(),
        },
        no_extract: false,
    }
}

#[test]
fn get_without_filters_refuses_selected_docs() {
    let (parsed, listing) = listed("drive/folder-select-mixed.html");
    assert_eq!(
        selected(&args(&[]), &parsed, &listing).unwrap_err().kind,
        Kind::SourceInvalid
    );
}

#[test]
fn get_can_exclude_docs_without_dropping_stored_files() {
    let (parsed, listing) = listed("drive/folder-select-mixed.html");
    let files = selected(&args(&["gdown"]), &parsed, &listing).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0].path, "gdown.pptx");
}

#[test]
fn get_excluding_a_doc_only_folder_reports_empty_selection() {
    let (parsed, listing) = listed("drive/folder-select-doc.html");
    let error = selected(&args(&["gdown"]), &parsed, &listing).unwrap_err();
    assert_eq!(error.kind, Kind::Usage);
    assert!(error.message.contains("select and exclude leave no file"));
}
