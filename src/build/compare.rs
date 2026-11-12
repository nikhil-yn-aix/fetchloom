//! a rerun of a step compared file by file with the result data.lock pins for it.

use std::cmp::Ordering;

use serde::Serialize;

use crate::cache::Member;

/// how the files of a rerun differ from the files data.lock pins, each list sorted by path.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Compared {
    /// how many files hold the same bytes at the same path.
    pub(crate) matching: usize,
    /// the paths both hold, with other bytes.
    pub(crate) changed: Vec<String>,
    /// the paths data.lock pins that the rerun did not make.
    pub(crate) missing: Vec<String>,
    /// the paths the rerun made that data.lock does not pin.
    pub(crate) extra: Vec<String>,
}

impl Compared {
    /// how many files data.lock pins.
    pub(crate) fn pinned(&self) -> usize {
        self.matching + self.changed.len() + self.missing.len()
    }
}

/// `made`, the files of a rerun, beside `pinned`, the files data.lock pins, in either order.
pub(crate) fn compare(pinned: &[Member], made: &[Member]) -> Compared {
    let (pinned, made) = (sorted(pinned), sorted(made));
    let mut compared = Compared::default();
    let (mut was, mut now) = (pinned.iter().peekable(), made.iter().peekable());
    loop {
        match (was.peek(), now.peek()) {
            (Some(old), Some(new)) => match old.path.cmp(&new.path) {
                Ordering::Less => {
                    compared.missing.push(old.path.clone());
                    was.next();
                }
                Ordering::Greater => {
                    compared.extra.push(new.path.clone());
                    now.next();
                }
                Ordering::Equal => {
                    if (old.size, old.sha256) == (new.size, new.sha256) {
                        compared.matching += 1;
                    } else {
                        compared.changed.push(old.path.clone());
                    }
                    was.next();
                    now.next();
                }
            },
            (Some(old), None) => {
                compared.missing.push(old.path.clone());
                was.next();
            }
            (None, Some(new)) => {
                compared.extra.push(new.path.clone());
                now.next();
            }
            (None, None) => return compared,
        }
    }
}

fn sorted(members: &[Member]) -> Vec<&Member> {
    let mut sorted: Vec<&Member> = members.iter().collect();
    sorted.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    sorted
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

    /// each kind is named whatever order either list comes in.
    #[test]
    fn changed_missing_and_extra_are_named() {
        let pinned = [
            member("c.csv", b"c"),
            member("a.csv", b"a"),
            member("b.csv", b"b"),
            member("e.csv", b"e"),
        ];
        let made = [
            member("e.csv", b"e"),
            member("d.csv", b"d"),
            member("b.csv", b"B"),
            member("a.csv", b"a"),
        ];
        let compared = compare(&pinned, &made);
        assert_eq!(
            compared,
            Compared {
                matching: 2,
                changed: vec!["b.csv".to_owned()],
                missing: vec!["c.csv".to_owned()],
                extra: vec!["d.csv".to_owned()],
            }
        );
        assert_eq!(compared.pinned(), 4);
        let same = compare(&pinned, &pinned);
        assert_eq!(same.matching, 4);
        assert!(same.changed.is_empty() && same.missing.is_empty() && same.extra.is_empty());
    }
}
