use super::*;
use crate::project::manifest::Manifest;
use crate::source::Reference;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const ID: &str = "12zxlvJtuHFV6awc3AINaNHnfvRttPv0i";
const URL: &str =
    "https://drive.google.com/embeddedfolderview?id=12zxlvJtuHFV6awc3AINaNHnfvRttPv0i";

fn listing(body: Vec<u8>) -> Listing {
    let recorded = Recorded::default().on(URL, 200, body);
    let reference = Reference::parse(&format!("drive:folders/{ID}"))
        .unwrap()
        .reference;
    block_on(reference.resolve(&recorded)).unwrap()
}

fn mixed() -> Listing {
    listing(fixture_bytes("drive/folder-select-mixed.html"))
}

fn entry(select: &[&str], exclude: &[&str], cli: bool) -> Entry {
    let manifest =
        Manifest::parse(&format!("[datasets.docs]\nref = \"drive:folders/{ID}\"\n")).unwrap();
    let mut entry = manifest.datasets["docs"].clone();
    entry.select = select
        .iter()
        .map(|pattern| Glob::new(pattern).unwrap())
        .collect();
    entry.exclude = exclude
        .iter()
        .map(|pattern| Glob::new(pattern).unwrap())
        .collect();
    if cli {
        entry.lines.clear();
    }
    entry
}

#[test]
fn excluding_a_doc_keeps_the_stored_files() {
    let kept = filter("docs", &entry(&[], &["gdown"], true), &mixed()).unwrap();
    let paths: Vec<_> = kept.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["gdown.pptx", "spam.txt"]);
}

#[test]
fn selecting_a_stored_file_leaves_native_docs_out() {
    let kept = filter("docs", &entry(&["*.pptx"], &[], true), &mixed()).unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].path, "gdown.pptx");
}

#[test]
fn a_selected_doc_is_refused_with_its_exclusion_flag() {
    for select in [&[][..], &["gdown"][..]] {
        let error = filter("docs", &entry(select, &[], true), &mixed()).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(
            error
                .details
                .iter()
                .any(|line| line.contains("gdown") && line.contains("exports"))
        );
        let help = error.help.unwrap();
        assert!(help.contains("--exclude \"gdown\""), "{help}");
        assert!(help.contains("pdf or csv"), "{help}");
    }
}

#[test]
fn an_unmatched_select_still_reports_an_example() {
    let error = filter("docs", &entry(&["*.csv"], &[], true), &mixed()).unwrap_err();
    assert_eq!(error.kind, Kind::Usage);
    assert!(error.message.contains("matched no file"));
    assert!(
        error.details.iter().any(
            |line| line.starts_with("3 entries were considered") && line.contains("gdown.pptx")
        )
    );
}

#[test]
fn a_doc_only_listing_can_explain_an_unmatched_select() {
    let listing = listing(fixture_bytes("drive/folder-select-doc.html"));
    let error = filter("docs", &entry(&["missing"], &[], true), &listing).unwrap_err();
    assert_eq!(error.kind, Kind::Usage);
    assert!(error.message.contains("matched no file"));
    assert!(error.details.iter().any(|line| line.contains("gdown")));
}

#[test]
fn excluding_everything_keeps_the_empty_selection_error() {
    for cli in [false, true] {
        for (select, exclude) in [(&[][..], &["*"][..]), (&["gdown"][..], &["gdown"][..])] {
            let error = filter("docs", &entry(select, exclude, cli), &mixed()).unwrap_err();
            assert_eq!(
                error.kind,
                if cli {
                    Kind::Usage
                } else {
                    Kind::ProjectInvalid
                }
            );
            assert!(error.message.contains("select and exclude leave no file"));
        }
    }
}

#[test]
fn excluding_the_only_doc_leaves_no_files() {
    let listing = listing(fixture_bytes("drive/folder-select-doc.html"));
    let error = filter("docs", &entry(&[], &["gdown"], true), &listing).unwrap_err();
    assert_eq!(error.kind, Kind::Usage);
    assert!(error.message.contains("select and exclude leave no file"));
}

#[test]
fn nested_docs_match_their_full_paths_and_get_deeper_hints() {
    let root = "https://drive.google.com/embeddedfolderview?id=1TMZoSAu4ecs_Q3GEEWfiyrQIPtj3DJAC";
    let child = "https://drive.google.com/embeddedfolderview?id=1-Z2wzjaaqqB1lBiPXNGfgkICA2Pc9q_U";
    let recorded = Recorded::default()
        .fixture(root, "drive/folder-deep-1.html")
        .fixture(child, "drive/folder-select-doc.html");
    let reference = Reference::parse("drive:folders/1TMZoSAu4ecs_Q3GEEWfiyrQIPtj3DJAC")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&recorded)).unwrap();
    let kept = filter("docs", &entry(&[], &["folder_1_1/gdown"], true), &listing).unwrap();
    assert_eq!(kept[0].path, "file.txt");
    let unmatched = filter("docs", &entry(&["gdown"], &[], true), &listing).unwrap_err();
    assert_eq!(unmatched.kind, Kind::Usage);
    assert!(unmatched.help.unwrap().contains("**/gdown"));
    let refused = filter("docs", &entry(&["**/gdown"], &[], true), &listing).unwrap_err();
    assert_eq!(refused.kind, Kind::SourceInvalid);
    assert!(
        refused
            .help
            .unwrap()
            .contains("--exclude \"folder_1_1/gdown\"")
    );
}

#[test]
fn exclusion_hints_match_literal_glob_characters() {
    let body = String::from_utf8(fixture_bytes("drive/folder-select-mixed.html"))
        .unwrap()
        .replace(">gdown<", ">gdown[1]*?<");
    let listing = listing(body.into_bytes());
    let error = filter("docs", &entry(&[], &[], true), &listing).unwrap_err();
    assert!(
        error
            .help
            .unwrap()
            .contains("--exclude \"gdown[[]1][*][?]\"")
    );
    let kept = filter("docs", &entry(&[], &["gdown[[]1][*][?]"], true), &listing).unwrap();
    assert_eq!(kept.len(), 2);
}

#[test]
fn unavailable_paths_cannot_escape_or_repeat_a_stored_path() {
    for (name, reason) in [("../gdown", "climbs"), ("gdown.pptx", "twice")] {
        let body = String::from_utf8(fixture_bytes("drive/folder-select-mixed.html"))
            .unwrap()
            .replace(">gdown<", &format!(">{name}<"));
        let recorded = Recorded::default().on(URL, 200, body);
        let reference = Reference::parse(&format!("drive:folders/{ID}"))
            .unwrap()
            .reference;
        let error = block_on(reference.resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(error.message.contains(reason), "{}", error.message);
    }
}
#[test]
fn exclusion_hints_quote_shell_characters() {
    let body = String::from_utf8(fixture_bytes("drive/folder-select-mixed.html"))
        .unwrap()
        .replace(">gdown<", ">gdown&#39;s $notes&#96;extra<");
    let listing = listing(body.into_bytes());
    let error = filter("docs", &entry(&[], &[], true), &listing).unwrap_err();
    let expected = if cfg!(windows) {
        "--exclude 'gdown''s $notes`extra'"
    } else {
        "--exclude 'gdown'\"'\"'s $notes`extra'"
    };
    assert!(error.help.unwrap().contains(expected));
}

#[test]
fn only_unavailable_entries_left_selected_are_named() {
    let body = String::from_utf8(fixture_bytes("drive/folder-select-mixed.html")).unwrap()
        .replace("<div class=\"flip-entries\">", "<div class=\"flip-entries\"><div class=\"flip-entry\"><a href=\"https://docs.google.com/document/d/1TFYNzuZJTgNGzGmjraZ58ZVOh9_YoKeBnU-opWgXQL4/edit\"><div class=\"flip-entry-title\">notes</div></a></div>");
    let listing = listing(body.into_bytes());
    let all = filter("docs", &entry(&[], &[], true), &listing).unwrap_err();
    let help = all.help.unwrap();
    assert!(help.contains("--exclude \"gdown\""));
    assert!(help.contains("--exclude \"notes\""));
    let one = filter("docs", &entry(&[], &["gdown"], true), &listing).unwrap_err();
    assert_eq!(one.details.len(), 1);
    assert!(one.details[0].contains("notes"));
    assert!(!one.help.unwrap().contains("gdown"));
    assert_eq!(
        filter("docs", &entry(&[], &["gdown", "notes"], true), &listing)
            .unwrap()
            .len(),
        2
    );
}
