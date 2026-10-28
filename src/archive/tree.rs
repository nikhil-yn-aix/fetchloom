//! the tree hash: one digest for every file an archive unpacks to, the same on every platform.

use std::fmt::Write as _;

use super::Member;
use crate::digest::{Hasher, Sha256};

/// the SHA-256 of one line per member, sorted by path: the path, a tab, the size, a tab, the
/// SHA-256 in hex and a newline. `data.lock` records it as `unpacks_to`.
pub(crate) fn tree_hash(members: &[Member]) -> Sha256 {
    let mut sorted: Vec<&Member> = members.iter().collect();
    sorted.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let mut hasher = Hasher::default();
    let mut line = String::new();
    for member in sorted {
        line.clear();
        let _ = writeln!(line, "{}\t{}\t{}", member.path, member.size, member.sha256);
        hasher.update(line.as_bytes());
    }
    hasher.finish().0
}

/// the one top folder every member sits under, when there is one, so unpacking need not repeat
/// it.
pub(crate) fn common_top(members: &[Member]) -> Option<&str> {
    let (top, _) = members.first()?.path.split_once('/')?;
    members
        .iter()
        .all(|member| {
            member
                .path
                .strip_prefix(top)
                .is_some_and(|rest| rest.starts_with('/'))
        })
        .then_some(top)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest;

    fn member(path: &str, bytes: &[u8]) -> Member {
        Member {
            path: path.to_owned(),
            size: bytes.len() as u64,
            sha256: digest::sha256(bytes),
        }
    }

    fn known_tree() -> Vec<Member> {
        vec![
            member("b.txt", b"abc"),
            member("a/c.txt", b""),
            member("a b.txt", b"x"),
            member("\u{e9}.txt", b"\n"),
            member("data/\u{540d}\u{524d}.csv", b"1,2\n"),
        ]
    }

    #[test]
    fn a_known_tree_matches_python() {
        // python: sha256("".join(f"{p}\t{len(d)}\t{sha256(d).hexdigest()}\n" for p, d in sorted(files.items())).encode())
        assert_eq!(
            tree_hash(&known_tree()).hex(),
            "fbeb559c9ccd9ab662f75914d0c93c7f2c94333ed82b6c4a151d0d342cb1e358"
        );
    }

    #[test]
    fn the_order_members_arrive_in_does_not_matter() {
        let mut reversed = known_tree();
        reversed.reverse();
        assert_eq!(tree_hash(&reversed), tree_hash(&known_tree()));
    }

    #[test]
    fn an_empty_tree_is_the_hash_of_no_lines() {
        assert_eq!(tree_hash(&[]), digest::sha256(b""));
    }

    #[test]
    fn one_top_folder_is_found() {
        let members = [member("data/a.csv", b""), member("data/sub/b.csv", b"")];
        assert_eq!(common_top(&members), Some("data"));
        assert_eq!(common_top(&[member("data/a.csv", b"")]), Some("data"));
    }

    #[test]
    fn no_top_folder_when_members_spread_out() {
        let at_root = [member("data/a.csv", b""), member("b.csv", b"")];
        assert_eq!(common_top(&at_root), None);
        let two_tops = [member("data/a.csv", b""), member("other/b.csv", b"")];
        assert_eq!(common_top(&two_tops), None);
        let same_prefix = [member("data/a.csv", b""), member("data2/b.csv", b"")];
        assert_eq!(common_top(&same_prefix), None);
        assert_eq!(common_top(&[member("a.csv", b"")]), None);
        assert_eq!(common_top(&[]), None);
    }
}
