use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on};

const COMMIT: &str = "71e2436a092d714350de0fc409ca8a8714e7e78f";
const API: &str = "https://api.github.com/repos/lab/survey";

fn folder(link: &str) -> Folder {
    match Reference::parse(link).unwrap().reference {
        Reference::GitFolder(folder) => folder,
        other => panic!("{link} read as {other:?}"),
    }
}

/// github's answer for a tree holding `entries`, cut short when `truncated`.
fn tree(entries: &serde_json::Value, truncated: bool) -> String {
    serde_json::json!({"sha": COMMIT, "tree": entries, "truncated": truncated}).to_string()
}

fn blob(path: &str, size: u64) -> serde_json::Value {
    serde_json::json!({"path": path, "mode": "100644", "type": "blob", "sha": "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391", "size": size})
}

fn subtree(path: &str, id: &str) -> serde_json::Value {
    serde_json::json!({"path": path, "mode": "040000", "type": "tree", "sha": id})
}

fn paths(listing: &Listing) -> Vec<&str> {
    listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

/// a folder's link reads back as written, in one form: https, without `www.`, a closing `/`,
/// gitlab's `ref_type` or an encoded `/`, its parts encoded only where a link must.
#[test]
fn folder_links_read_back_in_one_form() {
    for (typed, written, origin) in [
        (
            "https://github.com/lab/survey/tree/main/data",
            "https://github.com/lab/survey/tree/main/data",
            None,
        ),
        (
            "http://www.github.com/lab/survey/tree/main/field%20notes/",
            "https://github.com/lab/survey/tree/main/field%20notes",
            None,
        ),
        (
            "https://github.com/lab/survey/tree/release%2Fv1/data%5B1%5D",
            "https://github.com/lab/survey/tree/release/v1/data[1]",
            None,
        ),
        (
            "https://github.com/lab/survey/tree/main",
            "https://github.com/lab/survey/tree/main",
            None,
        ),
        (
            "https://gitlab.com/lab/group/survey/-/tree/main/data?ref_type=heads",
            "https://gitlab.com/lab/group/survey/-/tree/main/data",
            None,
        ),
        (
            "https://code.example.org:8443/lab/survey/-/tree/v1.0/data#readme",
            "https://code.example.org:8443/lab/survey/-/tree/v1.0/data",
            Some("https://code.example.org:8443"),
        ),
    ] {
        let parsed = folder(typed);
        assert_eq!(parsed.to_string(), written, "{typed}");
        assert_eq!(folder(written), parsed, "{typed}");
        assert_eq!(
            Reference::GitFolder(parsed).written_origin(),
            origin.map(|origin| Url::parse(origin).unwrap().origin()),
            "{typed}"
        );
    }
    for plain in [
        "https://github.com/lab/survey/tree",
        "https://github.com/lab/survey/tree/main/a%5Cb",
        "https://github.com/lab_survey/x/tree/main",
        "http://code.example.org/lab/survey/-/tree/main",
        "https://code.example.org/survey/-/tree/main",
        "https://example.org/lab/survey/tree/main",
    ] {
        let parsed = Reference::parse(plain).unwrap().reference;
        assert!(
            matches!(parsed, Reference::Url(_)),
            "{plain} read as {parsed:?}"
        );
    }
}

/// a repository's own link, as people share one, is its root at the default branch, written
/// without a closing `/`, `.git`, a query or a fragment, and named after the repository.
#[test]
fn a_repository_link_is_its_top_folder_at_the_default_branch() {
    for (typed, written) in [
        (
            "https://github.com/lab/survey",
            "https://github.com/lab/survey",
        ),
        (
            "https://github.com/lab/survey/",
            "https://github.com/lab/survey",
        ),
        (
            "http://www.github.com/lab/survey.git?tab=readme-ov-file#data",
            "https://github.com/lab/survey",
        ),
        (
            "https://gitlab.com/lab/group/survey",
            "https://gitlab.com/lab/group/survey",
        ),
        (
            "https://gitlab.com/lab/group/survey.git/",
            "https://gitlab.com/lab/group/survey",
        ),
    ] {
        let parsed = folder(typed);
        assert_eq!(parsed.to_string(), written, "{typed}");
        assert_eq!(folder(written), parsed, "{typed}");
        let reference = Reference::GitFolder(parsed);
        assert_eq!(reference.naming(), "survey", "{typed}");
        assert!(!reference.fixes_bytes(), "{typed}");
    }
}

/// a page of a repository that is not its code reads as it did: its issues, pulls, wiki,
/// actions and list of releases stay links, a release's page is the release and a file's page its
/// raw file. github's and gitlab's own pages, a user's or a group's, and another host's link of
/// two parts stay links too, since nothing in them names a repository's code.
#[test]
fn pages_that_are_not_the_code_read_as_they_did() {
    for link in [
        "https://github.com/lab/survey/issues",
        "https://github.com/lab/survey/pulls",
        "https://github.com/lab/survey/wiki",
        "https://github.com/lab/survey/actions",
        "https://github.com/lab/survey/releases",
        "https://github.com/lab",
        "https://github.com/topics/survey",
        "https://github.com/orgs/lab",
        "https://gitlab.com/lab",
        "https://gitlab.com/lab/survey/-/issues",
        "https://gitlab.com/explore/projects",
        "https://gitlab.com/lab/survey/blob/main/rows.csv",
        "https://code.example.org/lab/survey",
        "https://example.org/data/rows.csv",
    ] {
        let parsed = Reference::parse(link).unwrap().reference;
        assert_eq!(parsed, Reference::Url(Url::parse(link).unwrap()), "{link}");
    }
    for (link, read) in [
        (
            "https://github.com/lab/survey/releases/tag/v1",
            "github:lab/survey@v1",
        ),
        (
            "https://github.com/lab/survey/blob/main/rows.csv",
            "https://raw.githubusercontent.com/lab/survey/main/rows.csv",
        ),
    ] {
        assert_eq!(
            Reference::parse(link).unwrap().reference.to_string(),
            read,
            "{link}"
        );
    }
}

/// a repository's own link lists what its default branch's link lists: the commit `HEAD` names,
/// in one request as for a branch, then the whole tree.
#[test]
fn a_repository_link_lists_its_default_branch() {
    let trees = format!("{API}/git/trees/{COMMIT}?recursive=1");
    let recorded = Recorded::default()
        .on(&format!("{API}/commits/HEAD"), 200, COMMIT)
        .on(&format!("{API}/commits/main"), 200, COMMIT)
        .on(
            &trees,
            200,
            tree(
                &serde_json::json!([
                    blob("README.md", 9),
                    subtree("data", "d"),
                    blob("data/rows.csv", 2048)
                ]),
                false,
            ),
        );
    let bare = block_on(folder("https://github.com/lab/survey").listing(&recorded, false)).unwrap();
    assert_eq!(
        bare.resolved.to_string(),
        format!("https://github.com/lab/survey/tree/{COMMIT}")
    );
    assert_eq!(paths(&bare), ["README.md", "data/rows.csv"]);
    let main =
        block_on(folder("https://github.com/lab/survey/tree/main").listing(&recorded, false))
            .unwrap();
    assert_eq!(bare.resolved, main.resolved);
    assert_eq!(bare.files, main.files);
    let asked: Vec<String> = recorded
        .asked()
        .iter()
        .map(|asked| asked.split(' ').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        asked,
        [
            format!("{API}/commits/HEAD"),
            trees.clone(),
            format!("{API}/commits/main"),
            trees
        ]
    );
}

/// an empty repository's default branch holds no commit, which github answers with 409 and
/// gitlab with 404, and its own link says it is empty.
#[test]
fn an_empty_repository_says_so() {
    for (link, asked, status, body) in [
        (
            "https://github.com/lab/survey",
            format!("{API}/commits/HEAD"),
            409,
            r#"{"message":"Git Repository is empty.","status":"409"}"#,
        ),
        (
            "https://gitlab.com/lab/group/survey",
            format!("{PROJECT}/repository/commits/HEAD"),
            404,
            r#"{"message":"404 Commit Not Found"}"#,
        ),
    ] {
        let recorded = Recorded::default().on(&asked, status, body);
        let error = block_on(folder(link).listing(&recorded, false)).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceUnresolved, "{link}");
        assert_eq!(
            error.message,
            format!("{link} is empty: its default branch holds no commit")
        );
        assert_eq!(
            error.help.as_deref(),
            Some("check the link in a browser; an empty repository has no file to pin")
        );
    }
}

/// a dataset is named after its folder, or its repository for the whole of it; a commit fixes
/// its bytes, a branch does not.
#[test]
fn a_folder_names_its_dataset_and_a_commit_fixes_its_bytes() {
    for (link, named, fixed) in [
        (
            "https://github.com/lab/survey/tree/main/data/2020",
            "2020",
            false,
        ),
        ("https://github.com/lab/survey/tree/main", "survey", false),
        (
            "https://github.com/lab/survey/tree/71e2436a092d714350de0fc409ca8a8714e7e78f/data",
            "data",
            true,
        ),
        (
            "https://gitlab.com/lab/group/tools/-/tree/v1",
            "tools",
            false,
        ),
    ] {
        let reference = Reference::GitFolder(folder(link));
        assert_eq!(reference.naming(), named, "{link}");
        assert_eq!(reference.fixes_bytes(), fixed, "{link}");
    }
}

/// a ref may hold `/`, so the link's parts are asked for as a ref one more at a time, the fewest
/// first; the rest of the link is the folder.
#[test]
fn a_ref_with_a_slash_is_found_part_by_part() {
    let no_commit = r#"{"message":"No commit found for SHA: release","status":"422"}"#;
    let recorded = Recorded::default()
        .on(&format!("{API}/commits/release"), 422, no_commit)
        .on(&format!("{API}/commits/release%2Fv1"), 200, COMMIT)
        .on(
            &format!("{API}/git/trees/{COMMIT}?recursive=1"),
            200,
            tree(
                &serde_json::json!([subtree("data", "1"), blob("data/rows.csv", 19)]),
                false,
            ),
        );
    let link = "https://github.com/lab/survey/tree/release/v1/data";
    let listing = block_on(folder(link).listing(&recorded, false)).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        format!("https://github.com/lab/survey/tree/{COMMIT}/data")
    );
    assert_eq!(paths(&listing), ["rows.csv"]);
    assert_eq!(listing.access, crate::source::Access::Public);
    let asked: Vec<String> = recorded
        .asked()
        .iter()
        .map(|asked| asked.split(' ').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        asked,
        [
            format!("{API}/commits/release"),
            format!("{API}/commits/release%2Fv1"),
            format!("{API}/git/trees/{COMMIT}?recursive=1"),
        ]
    );
    assert!(
        recorded.asked()[0].ends_with(" accept: application/vnd.github.sha"),
        "{:?}",
        recorded.asked()
    );

    let recorded = Recorded::default().otherwise(422);
    let error = block_on(folder(link).listing(&recorded, false)).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert_eq!(
        error.message,
        "https://github.com/lab/survey has no branch, tag or commit named release, release/v1 or release/v1/data"
    );
    assert_eq!(recorded.asked().len(), 3);

    let recorded = Recorded::default().otherwise(422);
    let pinned = format!("https://github.com/lab/survey/tree/{COMMIT}/a/b/c");
    let error = block_on(folder(&pinned).listing(&recorded, false)).unwrap_err();
    assert!(
        error.message.ends_with(&format!("named {COMMIT}")),
        "{}",
        error.message
    );
    assert_eq!(recorded.asked().len(), 1, "a commit's id holds no `/`");
}

/// a folder of 2,000 files takes two requests of github's api, the most a run without a key can
/// spare: the commit its branch names, then the whole tree in one answer.
#[test]
fn a_folder_of_two_thousand_files_takes_two_requests() {
    let mut entries = vec![subtree("data", "d"), blob("README.md", 9)];
    entries.extend((0..2000).map(|n| blob(&format!("data/{n:04}.csv"), 2048)));
    let recorded = Recorded::default()
        .on(&format!("{API}/commits/main"), 200, COMMIT)
        .on(
            &format!("{API}/git/trees/{COMMIT}?recursive=1"),
            200,
            tree(&serde_json::Value::from(entries), false),
        );
    let link = "https://github.com/lab/survey/tree/main/data";
    let listing = block_on(folder(link).listing(&recorded, false)).unwrap();
    assert_eq!(listing.files.len(), 2000);
    assert_eq!(listing.files[1999].path, "1999.csv");
    assert_eq!(recorded.asked().len(), 2);
}

/// a file of at least 1 KiB, too large for a git-lfs pointer, comes from raw.githubusercontent.com
/// at its stated size; a smaller one from github.com, which sends a file git-lfs keeps on to its
/// bytes, so its size is the download's to tell.
#[test]
fn only_a_file_too_large_for_a_pointer_keeps_githubs_size() {
    let recorded = Recorded::default()
        .on(&format!("{API}/commits/main"), 200, COMMIT)
        .on(
            &format!("{API}/git/trees/{COMMIT}?recursive=1"),
            200,
            tree(
                &serde_json::json!([blob("model #1.bin", 131), blob("rows.csv", 1024)]),
                false,
            ),
        );
    let listing =
        block_on(folder("https://github.com/lab/survey/tree/main").listing(&recorded, false))
            .unwrap();
    let files: Vec<(&str, Option<u64>)> = listing
        .files
        .iter()
        .map(|file| (file.url.as_str(), file.size))
        .collect();
    assert_eq!(
        files,
        [
            (
                "https://github.com/lab/survey/raw/71e2436a092d714350de0fc409ca8a8714e7e78f/model%20%231.bin",
                None
            ),
            (
                "https://raw.githubusercontent.com/lab/survey/71e2436a092d714350de0fc409ca8a8714e7e78f/rows.csv",
                Some(1024)
            ),
        ]
    );
}

/// past its limits github cuts a tree's answer short, so the folders on the way to the one
/// asked for are listed one by one, then its subtrees whole, each cut short one in turn.
#[test]
fn a_tree_github_cuts_short_is_walked_folder_by_folder() {
    let at = |id: &str, recursive: bool| {
        let whole = if recursive { "?recursive=1" } else { "" };
        format!("{API}/git/trees/{id}{whole}")
    };
    let recorded = Recorded::default()
        .on(&format!("{API}/commits/main"), 200, COMMIT)
        .on(
            &at(COMMIT, true),
            200,
            tree(&serde_json::json!([subtree("other", "o")]), true),
        )
        .on(
            &at(COMMIT, false),
            200,
            tree(
                &serde_json::json!([
                    subtree("data", "d"),
                    subtree("other", "o"),
                    blob("top.csv", 1)
                ]),
                false,
            ),
        )
        .on(
            &at("d", false),
            200,
            tree(
                &serde_json::json!([
                    subtree("survey", "s"),
                    subtree("skip", "k"),
                    blob("readme.md", 1)
                ]),
                false,
            ),
        )
        .on(
            &at("s", true),
            200,
            tree(
                &serde_json::json!([subtree("big", "b"), blob("big/a.csv", 1)]),
                true,
            ),
        )
        .on(
            &at("s", false),
            200,
            tree(
                &serde_json::json!([
                    subtree("big", "b"),
                    subtree("small", "m"),
                    blob("rows.csv", 2)
                ]),
                false,
            ),
        )
        .on(
            &at("b", true),
            200,
            tree(
                &serde_json::json!([blob("a.csv", 3), blob("b.csv", 4)]),
                false,
            ),
        )
        .on(
            &at("m", true),
            200,
            tree(&serde_json::json!([blob("c.csv", 5)]), false),
        );
    let link = "https://github.com/lab/survey/tree/main/data/survey";
    let listing = block_on(folder(link).listing(&recorded, false)).unwrap();
    let mut listed = paths(&listing);
    listed.sort_unstable();
    assert_eq!(
        listed,
        ["big/a.csv", "big/b.csv", "rows.csv", "small/c.csv"]
    );
    let asked = recorded.asked();
    for skipped in [at("o", false), at("k", false), at("o", true), at("k", true)] {
        assert!(!asked.contains(&skipped), "{skipped}: {asked:?}");
    }
    assert_eq!(asked.len(), 8, "{asked:?}");
}

/// a folder of more entries than github's api gives at once, even alone, cannot be listed whole,
/// so it is refused with what to pin instead, never pinned in part.
#[test]
fn a_folder_github_cannot_list_whole_is_refused() {
    let at = |recursive: bool| {
        let whole = if recursive { "?recursive=1" } else { "" };
        format!("{API}/git/trees/{COMMIT}{whole}")
    };
    let cut = tree(&serde_json::json!([blob("a.csv", 1)]), true);
    let recorded = Recorded::default()
        .on(&format!("{API}/commits/main"), 200, COMMIT)
        .on(&at(true), 200, cut.clone())
        .on(&at(false), 200, cut);
    let link = "https://github.com/lab/survey/tree/main";
    let error = block_on(folder(link).listing(&recorded, false)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(
        error.message,
        "github lists only part of the top folder of lab/survey, which holds more entries than its api gives at once"
    );
    assert_eq!(
        error.help.as_deref(),
        Some("copy the link of a folder inside it that holds fewer files")
    );
}

/// a folder the tree lacks, or a file the link names as a folder, says which, and so does a
/// folder of git's links alone.
#[test]
fn a_folder_missing_a_file_or_links_alone_say_which() {
    let recorded = |entries: serde_json::Value| {
        Recorded::default()
            .on(&format!("{API}/commits/main"), 200, COMMIT)
            .on(
                &format!("{API}/git/trees/{COMMIT}?recursive=1"),
                200,
                tree(&entries, false),
            )
    };
    let link = "https://github.com/lab/survey/tree/main/data";
    for (entries, kind, message, help) in [
        (
            serde_json::json!([blob("other.csv", 1)]),
            Kind::ReferenceUnresolved,
            "https://github.com/lab/survey has no folder data at main",
            "check the folder's name at https://github.com/lab/survey/tree/main; git keeps no empty folder",
        ),
        (
            serde_json::json!([blob("data", 1)]),
            Kind::ReferenceInvalid,
            "data in https://github.com/lab/survey at main is a file, not a folder",
            "pin that one file by its own link: https://github.com/lab/survey/blob/main/data",
        ),
        (
            serde_json::json!([
                subtree("data", "d"),
                {"path": "data/vendor", "mode": "160000", "type": "commit", "sha": COMMIT},
            ]),
            Kind::ReferenceUnresolved,
            "https://github.com/lab/survey/tree/main/data holds no file, only 1 submodule, which git keeps as links rather than files: vendor",
            "copy the link of a folder that holds the files themselves",
        ),
    ] {
        let error = block_on(folder(link).listing(&recorded(entries), false)).unwrap_err();
        assert_eq!(error.kind, kind, "{message}");
        assert_eq!(error.message, message);
        assert_eq!(error.help.as_deref(), Some(help));
    }
}

/// github shows a private repository to a key that can read it, so a run with a key asks first,
/// and refuses one before its tree is asked; without a key, github shows no such repository.
#[test]
fn a_private_repository_is_refused_before_its_tree_is_asked() {
    let link = "https://github.com/lab/survey/tree/main/data";
    let recorded = Recorded::default().on(API, 200, r#"{"full_name":"lab/survey","private":true}"#);
    let error = block_on(folder(link).listing(&recorded, true)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(error.need.is_none(), "no sign-in lifts it");
    assert_eq!(recorded.asked(), [API]);

    let recorded = Recorded::default().on(
        &format!("{API}/commits/main"),
        404,
        r#"{"message":"Not Found"}"#,
    );
    let error = block_on(folder(link).listing(&recorded, false)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "github has no repository lab/survey, or it is private"
    );
    assert!(error.need.is_none(), "a key would not get its files");
    let recorded = Recorded::default().on(API, 404, r#"{"message":"Not Found"}"#);
    let error = block_on(folder(link).listing(&recorded, true)).unwrap_err();
    assert_eq!(
        error.message,
        "github has no repository lab/survey, or this key cannot see it"
    );
}

const PROJECT: &str = "https://gitlab.com/api/v4/projects/lab%2Fgroup%2Fsurvey";
const FIRST_PAGE: &str = "https://gitlab.com/api/v4/projects/lab%2Fgroup%2Fsurvey/repository/tree?path=data&ref=71e2436a092d714350de0fc409ca8a8714e7e78f&recursive=true&per_page=100&pagination=keyset";
const NEXT_PAGE: &str = "https://gitlab.com/api/v4/projects/lab%2Fgroup%2Fsurvey/repository/tree?page_token=b2Zm&path=data&ref=71e2436a092d714350de0fc409ca8a8714e7e78f&recursive=true&per_page=100&pagination=keyset";

fn entry(path: &str, kind: &str, mode: &str) -> serde_json::Value {
    serde_json::json!({"id": "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391", "name": path.rsplit('/').next(), "type": kind, "path": path, "mode": mode})
}

/// gitlab's answer that `main` is at [`COMMIT`], from the api of the project at `project`.
fn commit(project: &str) -> Recorded {
    Recorded::default().on(
        &format!("{project}/repository/commits/main"),
        200,
        serde_json::json!({"id": COMMIT, "short_id": "71e2436a"}).to_string(),
    )
}

/// gitlab lists a folder in pages, each followed only within the same listing, and gives no
/// size, so the download tells it.
#[test]
fn a_gitlab_folder_is_read_page_by_page() {
    let recorded = commit(PROJECT)
        .on(
            FIRST_PAGE,
            200,
            serde_json::json!([
                entry("data/2020", "tree", "040000"),
                entry("data/rows.csv", "blob", "100644")
            ])
            .to_string(),
        )
        .next(FIRST_PAGE, NEXT_PAGE)
        .on(
            NEXT_PAGE,
            200,
            serde_json::json!([
                entry("data/2020/notes.txt", "blob", "100755"),
                entry("data/latest.csv", "blob", "120000")
            ])
            .to_string(),
        );
    let link = "https://gitlab.com/lab/group/survey/-/tree/main/data";
    let listing = block_on(folder(link).listing(&recorded, false)).unwrap();
    assert_eq!(paths(&listing), ["rows.csv", "2020/notes.txt"]);
    assert!(listing.files.iter().all(|file| file.size.is_none()));
    assert_eq!(
        listing.files[1].url.as_str(),
        format!("https://gitlab.com/lab/group/survey/-/raw/{COMMIT}/data/2020/notes.txt")
    );
    assert_eq!(
        listing.warnings,
        [format!(
            "{link} holds 1 symbolic link, which git keeps as a link rather than a file, so it is left out: latest.csv"
        )]
    );
    assert_eq!(listing.access, crate::source::Access::Public);
    assert_eq!(
        recorded.asked(),
        [
            format!("{PROJECT}/repository/commits/main"),
            FIRST_PAGE.to_owned(),
            NEXT_PAGE.to_owned()
        ]
    );
    let signed = block_on(folder(link).listing(&recorded.clone(), true)).unwrap();
    assert_eq!(
        signed.access,
        crate::source::Access::Unknown,
        "a key may have gone"
    );
}

/// a page that leads outside its own listing is refused before it is asked, and so is an entry
/// outside the folder asked for; a folder the tree lacks and a project gitlab does not show say
/// which they are.
#[test]
fn gitlab_answers_outside_the_folder_are_refused() {
    let link = "https://gitlab.com/lab/group/survey/-/tree/main/data";
    let page = serde_json::json!([entry("data/rows.csv", "blob", "100644")]).to_string();
    let elsewhere = commit(PROJECT).on(FIRST_PAGE, 200, page).next(
        FIRST_PAGE,
        "https://gitlab.com/api/v4/projects/lab%2Fother/repository/tree?page_token=b2Zm",
    );
    let error = block_on(folder(link).listing(&elsewhere, false)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(
        error
            .message
            .contains("a page's next page is not one of this listing's"),
        "{}",
        error.message
    );
    assert_eq!(elsewhere.asked().len(), 2);

    let outside = commit(PROJECT).on(
        FIRST_PAGE,
        200,
        serde_json::json!([entry("database/rows.csv", "blob", "100644")]).to_string(),
    );
    let error = block_on(folder(link).listing(&outside, false)).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(
        error
            .message
            .contains("\"database/rows.csv\", which is not in the folder asked for"),
        "{}",
        error.message
    );

    let missing = commit(PROJECT).on(FIRST_PAGE, 404, r#"{"message":"404 Tree Not Found"}"#);
    let error = block_on(folder(link).listing(&missing, false)).unwrap_err();
    assert_eq!(
        error.message,
        "https://gitlab.com/lab/group/survey has no folder data at main"
    );

    let hidden = Recorded::default().on(
        &format!("{PROJECT}/repository/commits/main"),
        404,
        r#"{"message":"404 Project Not Found"}"#,
    );
    let error = block_on(folder(link).listing(&hidden, false)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(
        error
            .help
            .unwrap_or_default()
            .contains("a private project's files need a key that can read it"),
    );
}

/// a folder on another gitlab server is asked of that server alone and its files link there,
/// where the key saved for that server goes and no other key does.
#[test]
fn a_folder_on_another_server_asks_and_links_that_server_alone() {
    let server = "https://code.example.org:8443/api/v4/projects/lab%2Fsurvey";
    let first = format!(
        "{server}/repository/tree?ref={COMMIT}&recursive=true&per_page=100&pagination=keyset"
    );
    let recorded = commit(server).on(
        &first,
        200,
        serde_json::json!([entry("rows.csv", "blob", "100644")]).to_string(),
    );
    let reference = Reference::parse("https://code.example.org:8443/lab/survey/-/tree/main")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&recorded)).unwrap();
    let origin = Url::parse("https://code.example.org:8443/")
        .unwrap()
        .origin();
    for asked in recorded.asked() {
        assert_eq!(Url::parse(&asked).unwrap().origin(), origin, "{asked}");
    }
    for file in &listing.files {
        assert_eq!(file.url.origin(), origin, "{}", file.url);
    }
    assert!(
        matches!(listing.resolved.access().api, access::Origins::Written),
        "the server's own api alone"
    );
    assert_eq!(listing.resolved.written_origin(), Some(origin));
}
