//! what each archive unpacked to, kept under `unpacked/` by the archive's SHA-256. a record is
//! the archive's tree: one line per file, sorted by path, holding the path, a tab, the size, a
//! tab and the SHA-256 in hex. the record's own SHA-256 is the tree hash `data.lock` keeps as
//! `unpacks_to`, so a record proves itself against the lock.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::PathBuf;

use super::Cache;
use crate::digest::{self, Sha256};

/// a file an archive unpacked to, stored in the cache under its SHA-256.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    /// in NFC, with `/` between folders.
    pub(crate) path: String,
    pub(crate) size: u64,
    pub(crate) sha256: Sha256,
}

/// the lines of a tree, sorted by path.
pub(crate) fn tree(members: &[Member]) -> String {
    let mut sorted: Vec<&Member> = members.iter().collect();
    sorted.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    let mut text = String::with_capacity(members.len() * 96);
    for member in sorted {
        let _ = writeln!(text, "{}\t{}\t{}", member.path, member.size, member.sha256);
    }
    text
}

/// the tree hash `data.lock` records as `unpacks_to`.
pub(crate) fn tree_hash(members: &[Member]) -> Sha256 {
    digest::sha256(tree(members).as_bytes())
}

impl Cache {
    /// records what `archive` unpacked to, and returns the tree hash.
    pub(crate) fn record_unpacked(
        &self,
        archive: &Sha256,
        members: &[Member],
    ) -> io::Result<Sha256> {
        let text = tree(members);
        self.write_atomically(&self.unpacked_path(archive), text.as_bytes())?;
        Ok(digest::sha256(text.as_bytes()))
    }

    /// what `archive` unpacked to on this machine. given a tree hash, only a record of that tree
    /// counts.
    pub(crate) fn unpacked(&self, archive: &Sha256, tree: Option<&Sha256>) -> Option<Vec<Member>> {
        let bytes = fs::read(self.unpacked_path(archive)).ok()?;
        if tree.is_some_and(|tree| digest::sha256(&bytes) != *tree) {
            return None;
        }
        parse(std::str::from_utf8(&bytes).ok()?)
    }

    /// whether every file `archive` unpacked to is in the cache, so its own bytes are not needed.
    pub(crate) fn holds_unpacked(&self, archive: &Sha256, tree: Option<&Sha256>) -> bool {
        self.unpacked(archive, tree).is_some_and(|members| {
            members
                .iter()
                .all(|member| self.find(&member.sha256, Some(member.size)).is_some())
        })
    }

    /// the tree hash of what `archive` unpacked to on this machine, when it was recorded.
    pub(crate) fn unpacked_tree(&self, archive: &Sha256) -> Option<Sha256> {
        let bytes = fs::read(self.unpacked_path(archive)).ok()?;
        parse(std::str::from_utf8(&bytes).ok()?)?;
        Some(digest::sha256(&bytes))
    }

    /// every archive with a record.
    pub(crate) fn unpacked_archives(&self) -> Vec<Sha256> {
        fs::read_dir(self.root.join("unpacked"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| Sha256::from_hex(&entry.file_name().to_string_lossy()))
            .collect()
    }

    pub(super) fn unpacked_path(&self, archive: &Sha256) -> PathBuf {
        self.root.join("unpacked").join(archive.hex())
    }
}

fn parse(text: &str) -> Option<Vec<Member>> {
    text.lines()
        .map(|line| {
            let mut parts = line.split('\t');
            let member = Member {
                path: parts.next()?.to_owned(),
                size: parts.next()?.parse().ok()?,
                sha256: Sha256::from_hex(parts.next()?)?,
            };
            parts.next().is_none().then_some(member)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{cache, store};

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
    fn a_record_is_read_back_sorted_and_checked_against_its_tree() {
        let (_dir, cache) = cache();
        let archive = digest::sha256(b"archive");
        let tree = cache.record_unpacked(&archive, &known_tree()).unwrap();
        assert_eq!(tree, tree_hash(&known_tree()));
        let mut sorted = known_tree();
        sorted.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(cache.unpacked(&archive, Some(&tree)), Some(sorted.clone()));
        assert_eq!(cache.unpacked(&archive, None), Some(sorted));
        let other = digest::sha256(b"another tree");
        assert_eq!(cache.unpacked(&archive, Some(&other)), None);
        assert_eq!(cache.unpacked(&other, None), None, "no record");
        assert_eq!(cache.unpacked_archives(), [archive]);
    }

    #[test]
    fn a_damaged_record_counts_as_none() {
        let (_dir, cache) = cache();
        let archive = digest::sha256(b"archive");
        fs::write(cache.unpacked_path(&archive), "a.csv\tnot a size\tff\n").unwrap();
        assert_eq!(cache.unpacked(&archive, None), None);
    }

    #[test]
    fn an_archive_is_held_unpacked_only_while_every_member_is_cached() {
        let (_dir, cache) = cache();
        let archive = digest::sha256(b"archive");
        let kept = store(&cache, b"kept");
        let members = [
            Member {
                path: "a.csv".to_owned(),
                size: kept.size,
                sha256: kept.digest,
            },
            member("b.csv", b"never stored"),
        ];
        let tree = cache.record_unpacked(&archive, &members[..1]).unwrap();
        assert!(cache.holds_unpacked(&archive, Some(&tree)));
        cache.record_unpacked(&archive, &members).unwrap();
        assert!(!cache.holds_unpacked(&archive, None));
    }
}
