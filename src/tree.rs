//! the identity of a dataset's effective tree: the files a pipeline reading its folder sees, after
//! archives unpack and members tables choose, by their paths in the dataset, sizes and SHA-256.
//! data.lock keeps it with each dataset's pin, and with a dataset made from others theirs as they
//! were, so nothing else may change it: not a link, a time, a note or where the dataset lands.
//!
//! it is the tree hash data.lock gives an archive as `unpacks_to` ([`tree_hash`]), so an archive
//! whose files a dataset places as they are has the dataset's identity, and data.lock's version
//! versions both.

use crate::cache::{Cache, Member, tree_hash};
use crate::digest::Sha256;
use crate::plan::{self, Plan};
use crate::project::lock::Locked;
use crate::project::lock::archives::Archive;
use crate::project::manifest::Entry;

/// the identity of `locked`'s files as `entry`'s members tables keep them, or none when this
/// machine cannot tell: an archive it never unpacked, or members tables that do not fit the pin.
pub(crate) fn effective(entry: &Entry, locked: &Locked, cache: &Cache) -> Option<Sha256> {
    let _span = tracing::debug_span!("tree").entered();
    effective_with(entry, locked, &|archive| archive.recorded(cache))
}

/// [`effective`] with each archive's files as `members` gives them.
fn effective_with(
    entry: &Entry,
    locked: &Locked,
    members: &dyn Fn(&Archive<'_>) -> Option<Vec<Member>>,
) -> Option<Sha256> {
    // without an archive each file is where data.lock lists it, which no members table moves
    if locked.files.iter().all(|file| file.unpacks_to.is_none()) {
        return Some(tree_hash(
            locked
                .files
                .iter()
                .map(|file| (file.path.as_str(), file.size, file.sha256)),
        ));
    }
    let mut plan = Plan::default();
    plan::expand(
        &mut plan,
        &locked.name,
        "",
        false,
        &locked.files,
        &entry.source.members,
        members,
    )
    .ok()?;
    if !plan.unknown.is_empty() {
        return None;
    }
    Some(tree_hash(plan.targets.iter().map(|target| {
        (target.path.as_str(), target.size, target.sha256)
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest;
    use crate::project::lock::LockedFile;
    use crate::project::manifest::Manifest;
    use crate::source::Access;

    fn file(path: &str, bytes: &[u8]) -> LockedFile {
        LockedFile {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: digest::sha256(bytes),
            md5: None,
            url: format!("https://example.org/{path}"),
            unpacks_to: None,
        }
    }

    fn archive(path: &str, bytes: &[u8]) -> LockedFile {
        LockedFile {
            unpacks_to: Some(digest::sha256(b"its files")),
            ..file(path, bytes)
        }
    }

    fn locked(files: Vec<LockedFile>) -> Locked {
        Locked {
            name: "iris".to_owned(),
            reference: "https://example.org/iris".to_owned(),
            spec: String::new(),
            extract: true,
            decompress: false,
            web_page: false,
            resolved: "https://example.org/iris".to_owned(),
            title: None,
            license: None,
            doi: None,
            access: Access::Unknown,
            timestamp: "2025-11-17T00:00:00Z".to_owned(),
            tree: None,
            from: Vec::new(),
            files,
        }
    }

    fn entry(toml: &str) -> Entry {
        let text = format!("[datasets.iris]\nref = \"https://example.org/iris\"\n{toml}");
        Manifest::parse(&text).unwrap().datasets["iris"].clone()
    }

    fn member(path: &str, bytes: &[u8]) -> Member {
        Member {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: digest::sha256(bytes),
        }
    }

    /// the tree hash of `lines`, written out by hand, so the test does not trust the code.
    fn expected(lines: &[&str]) -> String {
        let mut text = String::new();
        for line in lines {
            text.push_str(line);
            text.push('\n');
        }
        format!("sha256:{}", digest::sha256(text.as_bytes()))
    }

    fn shown(tree: Option<Sha256>) -> Option<String> {
        tree.map(|tree| format!("sha256:{tree}"))
    }

    #[test]
    fn the_identity_is_the_tree_hash_of_sorted_lines_of_path_size_and_sha256() {
        let iris = locked(vec![
            file("b/iris.csv", b"iris\n"),
            file("a.csv", b"setosa\n"),
        ]);
        assert_eq!(
            shown(effective_with(&entry(""), &iris, &|_| None)),
            Some(expected(&[
                &format!("a.csv\t7\t{}", digest::sha256(b"setosa\n")),
                &format!("b/iris.csv\t5\t{}", digest::sha256(b"iris\n")),
            ]))
        );
        let empty = locked(Vec::new());
        assert_eq!(
            shown(effective_with(&entry(""), &empty, &|_| None)),
            Some(expected(&[]))
        );
    }

    #[test]
    fn links_times_and_destinations_leave_the_identity_alone() {
        let one = locked(vec![file("iris.csv", b"setosa\n")]);
        let mut other = one.clone();
        other.files[0].url = "https://mirror.example.org/elsewhere.csv".to_owned();
        other.files[0].md5 = Some(digest::Md5::from_hex(&"0".repeat(32)).unwrap());
        other.reference = "https://example.org/moved".to_owned();
        other.timestamp = "2030-01-01T00:00:00Z".to_owned();
        other.title = Some("iris, again".to_owned());
        let placed = entry("");
        let elsewhere = entry("path = \"flowers\"\ncopy = true\n");
        let tree = effective_with(&placed, &one, &|_| None);
        assert!(tree.is_some());
        assert_eq!(effective_with(&elsewhere, &other, &|_| None), tree);
    }

    #[test]
    fn bytes_sizes_and_paths_change_the_identity() {
        let tree = |files| effective_with(&entry(""), &locked(files), &|_| None);
        let first = tree(vec![file("iris.csv", b"setosa\n")]);
        for changed in [
            vec![file("iris.csv", b"virginica\n")],
            vec![file("flowers.csv", b"setosa\n")],
            vec![file("iris.csv", b"setosa\n"), file("more.csv", b"")],
        ] {
            assert_ne!(tree(changed), first);
        }
    }

    #[test]
    fn an_archive_counts_as_the_files_it_unpacks_to() {
        let files = [member("iris/a.csv", b"a\n"), member("iris/b.csv", b"b\n")];
        let unpacked = |_: &Archive<'_>| Some(files.to_vec());
        let zipped = locked(vec![archive("iris.zip", b"zip bytes")]);
        let recompressed = locked(vec![archive("iris.zip", b"other zip bytes")]);
        let tree = effective_with(&entry(""), &zipped, &unpacked);
        assert_eq!(effective_with(&entry(""), &recompressed, &unpacked), tree);
        // an archive alone in its dataset lands without its files' shared top folder
        let loose = locked(vec![file("a.csv", b"a\n"), file("b.csv", b"b\n")]);
        assert_eq!(effective_with(&entry(""), &loose, &|_| None), tree);
        assert_eq!(
            shown(tree),
            Some(expected(&[
                &format!("a.csv\t2\t{}", digest::sha256(b"a\n")),
                &format!("b.csv\t2\t{}", digest::sha256(b"b\n")),
            ]))
        );
    }

    #[test]
    fn a_members_table_changes_the_files_a_pipeline_sees() {
        let files = [member("iris/a.csv", b"a\n"), member("iris/b.csv", b"b\n")];
        let unpacked = |_: &Archive<'_>| Some(files.to_vec());
        let zipped = locked(vec![archive("iris.zip", b"zip bytes")]);
        let whole = effective_with(&entry(""), &zipped, &unpacked);
        let chosen = entry("[datasets.iris.members.\"iris.zip\"]\nselect = [\"iris/a.csv\"]\n");
        let some = effective_with(&chosen, &zipped, &unpacked);
        assert!(some.is_some());
        assert_ne!(some, whole);
        let only_a = locked(vec![file("a.csv", b"a\n")]);
        assert_eq!(effective_with(&entry(""), &only_a, &|_| None), some);
    }

    #[test]
    fn a_packed_archive_counts_as_itself() {
        let zipped = locked(vec![archive("iris.zip", b"zip bytes")]);
        let mut packed = zipped.clone();
        packed.files[0].unpacks_to = None;
        let tree = effective_with(&entry("extract = false\n"), &packed, &|_| None);
        assert_eq!(
            shown(tree),
            Some(expected(&[&format!(
                "iris.zip\t9\t{}",
                digest::sha256(b"zip bytes")
            )]))
        );
    }

    #[test]
    fn an_archive_never_unpacked_here_leaves_the_identity_unknown() {
        let zipped = locked(vec![archive("iris.zip", b"zip bytes"), file("README", b"")]);
        assert_eq!(effective_with(&entry(""), &zipped, &|_| None), None);
        let alone = locked(vec![archive("iris.zip", b"zip bytes")]);
        let unfitting = entry("[datasets.iris.members.\"iris.zip\"]\nselect = [\"none/*\"]\n");
        let files = [member("iris/a.csv", b"a\n")];
        let tree = effective_with(&unfitting, &alone, &|_| Some(files.to_vec()));
        assert_eq!(
            tree, None,
            "a members table that keeps nothing is no tree to compare"
        );
    }
}
