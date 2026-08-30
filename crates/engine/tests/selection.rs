//! Contract tests over selection, exclusion, and layout rewriting.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use fetchloom_engine::error::ErrorKind;
use fetchloom_engine::selection::{Glob, Layout, Selection};

fn paths(applied: &fetchloom_engine::selection::Applied) -> Vec<&str> {
    applied
        .members
        .iter()
        .map(|member| member.path.as_str())
        .collect()
}

fn directories(applied: &fetchloom_engine::selection::Applied) -> Vec<&str> {
    applied
        .directories
        .iter()
        .map(fetchloom_engine::tree::EntryPath::as_str)
        .collect()
}

fn selecting(patterns: &[&str]) -> Selection {
    Selection {
        include: patterns.iter().map(|text| Glob::new(*text)).collect(),
        exclude: Vec::new(),
        layout: Layout::Keep,
    }
}

#[test]
fn a_star_matches_within_one_component_and_never_crosses_a_separator() {
    assert!(Glob::new("*.txt").matches("a.txt"));
    assert!(Glob::new("data/*.txt").matches("data/a.txt"));
    assert!(!Glob::new("data/*.txt").matches("data/inner/a.txt"));
    assert!(Glob::new("data/*").matches("data/a.txt"));
    assert!(!Glob::new("*").matches("a/b"));
}

#[test]
fn a_star_matches_no_bytes_at_all() {
    assert!(Glob::new("a*b").matches("ab"));
    assert!(Glob::new("*a").matches("a"));
}

#[test]
fn a_double_star_is_a_whole_component_and_crosses_directories() {
    assert!(Glob::new("**/*.txt").matches("a.txt"));
    assert!(Glob::new("**/*.txt").matches("data/inner/a.txt"));
    assert!(Glob::new("data/**").matches("data/inner/a.txt"));
    assert!(Glob::new("data/**").matches("data"));
    assert!(!Glob::new("data/**").matches("other/a.txt"));
}

#[test]
fn a_double_star_inside_a_component_is_two_literal_stars() {
    assert!(!Glob::new("a**b").matches("a/x/b"));
    assert!(Glob::new("a**b").matches("axb"));
}

#[test]
fn a_question_matches_exactly_one_byte_within_one_component() {
    assert!(Glob::new("a?c").matches("abc"));
    assert!(!Glob::new("a?c").matches("ac"));
    assert!(!Glob::new("a?c").matches("abbc"));
    assert!(!Glob::new("a?c").matches("a/c"));
}

#[test]
fn every_other_byte_is_literal() {
    assert!(Glob::new("a[bc]d").matches("a[bc]d"));
    assert!(!Glob::new("a[bc]d").matches("abd"));
    assert!(Glob::new("x{a,b}").matches("x{a,b}"));
    assert!(!Glob::new("x{a,b}").matches("xa"));
    assert!(Glob::new(r"a\b").matches(r"a\b"));
}

#[test]
fn matching_is_case_sensitive_and_normalizes_nothing() {
    assert!(!Glob::new("a.txt").matches("A.TXT"));
    assert!(Glob::new("A.txt").matches("A.txt"));
}

#[test]
fn an_empty_include_list_selects_every_member() {
    let applied = Selection::default().apply(&["a.txt", "b/c.txt"]).unwrap();
    assert_eq!(paths(&applied), vec!["a.txt", "b/c.txt"]);
}

#[test]
fn a_pattern_selects_member_paths_and_not_what_is_under_them() {
    let members = ["data", "data/a.txt", "other.txt"];
    let named = selecting(&["data"]).apply(&members).unwrap();
    assert_eq!(paths(&named), vec!["data"]);

    let under = selecting(&["data/**"]).apply(&members).unwrap();
    assert_eq!(under.members.len(), 2);
}

#[test]
fn exclusion_is_applied_after_every_include() {
    let selection = Selection {
        include: vec![Glob::new("**/*.txt")],
        exclude: vec![Glob::new("data/**")],
        layout: Layout::Keep,
    };
    let applied = selection
        .apply(&["a.txt", "data/b.txt", "other/c.txt"])
        .unwrap();
    assert_eq!(paths(&applied), vec!["a.txt", "other/c.txt"]);
}

#[test]
fn a_selection_that_matches_nothing_is_an_error() {
    let error = selecting(&["nothing/**"])
        .apply(&["a.txt", "b.txt"])
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::ReferenceUnresolved);
    assert!(error.next_action().contains("nothing/**"));
}

#[test]
fn every_ancestor_of_a_selected_member_is_a_directory_entry() {
    let applied = selecting(&["**/*.txt"])
        .apply(&["a/b/c.txt", "a/d.txt"])
        .unwrap();
    assert_eq!(directories(&applied), vec!["a", "a/b"]);
}

#[test]
fn an_ancestor_that_is_itself_selected_is_not_listed_twice() {
    let applied = selecting(&["a", "a/b.txt"])
        .apply(&["a", "a/b.txt"])
        .unwrap();
    assert_eq!(paths(&applied), vec!["a", "a/b.txt"]);
    assert!(directories(&applied).is_empty());
}

#[test]
fn flattening_drops_the_first_components_of_every_path() {
    let selection = Selection {
        include: Vec::new(),
        exclude: Vec::new(),
        layout: Layout::Flatten(1),
    };
    let applied = selection
        .apply(&["release/a.txt", "release/inner/b.txt"])
        .unwrap();
    assert_eq!(paths(&applied), vec!["a.txt", "inner/b.txt"]);
    assert_eq!(directories(&applied), vec!["inner"]);
}

#[test]
fn flattening_a_member_to_nothing_names_the_member_and_the_count() {
    let selection = Selection {
        include: Vec::new(),
        exclude: Vec::new(),
        layout: Layout::Flatten(2),
    };
    let error = selection.apply(&["release/a.txt"]).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::DestinationUnrepresentable);
    assert!(error.next_action().contains("release/a.txt"));
    assert!(error.next_action().contains('2'));
}

#[test]
fn a_member_the_layout_leaves_unnamed_is_never_dropped_silently() {
    let selection = Selection {
        include: Vec::new(),
        exclude: Vec::new(),
        layout: Layout::Flatten(1),
    };
    assert!(selection.apply(&["a.txt"]).is_err());
}

#[test]
fn selection_reports_which_member_each_selected_path_came_from() {
    let applied = selecting(&["b/**"]).apply(&["a.txt", "b/c.txt"]).unwrap();
    assert_eq!(applied.members.len(), 1);
    assert_eq!(applied.members[0].index, 1);
}
