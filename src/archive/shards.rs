//! numbered tar files in one folder: the shards webdataset loaders read packed. unpacking them
//! turns a few large files into many small ones, which costs inodes and a cluster's metadata
//! server, so a person who sees them before anything downloads can keep them packed.

use std::collections::HashMap;

use crate::units;

/// numbered tar shards in one folder of a listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shards<'a> {
    /// the folder that holds them, with `/` between folders, empty at the top of the listing.
    pub(crate) folder: &'a str,
    pub(crate) count: u64,
}

/// the fewest numbered tars that look like shards rather than a dataset's few parts.
const FEWEST: u64 = 8;

/// the endings of a shard, compared without case.
const ENDINGS: [&str; 3] = [".tar", ".tar.gz", ".tgz"];

impl Shards<'_> {
    /// the hint for these shards in `owner`, a dataset or a link, ending with `keep`, how the
    /// command that found them keeps archives packed.
    pub(crate) fn hint(&self, owner: &str, keep: &str) -> String {
        let inside = if self.folder.is_empty() {
            String::new()
        } else {
            format!(" in {}", self.folder)
        };
        format!(
            "{owner} holds {}{inside}, which webdataset loaders read packed. keep them packed with {keep}",
            units::count(self.count, "numbered .tar shard", "numbered .tar shards")
        )
    }
}

/// the largest group of at least 8 files in one folder named `<prefix><digits>.tar`, `.tar.gz`
/// or `.tgz`, such as `train-000123.tar` or `00000000.tar`, with one prefix and one digit width.
/// one pass over `paths`, keyed by slices of them, so a listing of millions costs no copy of it.
pub(crate) fn shards<'a>(paths: impl IntoIterator<Item = &'a str>) -> Option<Shards<'a>> {
    let mut groups: HashMap<(&str, &str, usize), u64> = HashMap::new();
    let mut most: Option<Shards<'a>> = None;
    for path in paths {
        let Some((folder, prefix, width)) = numbered(path) else {
            continue;
        };
        let count = groups.entry((folder, prefix, width)).or_default();
        *count += 1;
        if *count >= FEWEST && most.is_none_or(|most| *count > most.count) {
            most = Some(Shards {
                folder,
                count: *count,
            });
        }
    }
    most
}

/// the folder, the prefix and the number of digits of a tar named `<prefix><digits>` and a
/// shard's ending, or none for any other file.
fn numbered(path: &str) -> Option<(&str, &str, usize)> {
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    let stem = ENDINGS.iter().find_map(|ending| {
        let at = name.len().checked_sub(ending.len())?;
        name.get(at..)?
            .eq_ignore_ascii_case(ending)
            .then(|| &name[..at])
    })?;
    let width = stem.bytes().rev().take_while(u8::is_ascii_digit).count();
    (width > 0).then(|| (folder, &stem[..stem.len() - width], width))
}

#[cfg(test)]
mod tests {
    use super::{Shards, shards};

    fn numbered(folder: &str, prefix: &str, ending: &str, count: usize) -> Vec<String> {
        (0..count)
            .map(|number| format!("{folder}{prefix}{number:06}{ending}"))
            .collect()
    }

    fn found(paths: &[String]) -> Option<Shards<'_>> {
        shards(paths.iter().map(String::as_str))
    }

    #[test]
    fn numbered_tar_shards_are_noticed() {
        for ending in [".tar", ".tar.gz", ".tgz", ".TAR", ".Tar.Gz"] {
            assert_eq!(
                found(&numbered("", "train-", ending, 8)),
                Some(Shards {
                    folder: "",
                    count: 8
                }),
                "{ending}"
            );
        }
        let mut beside = numbered("wds/val/", "shard-", ".tar", 8);
        beside.extend(numbered("wds/train/", "shard-", ".tar", 9));
        beside.extend(["README.md".to_owned(), "wds/train/index.json".to_owned()]);
        assert_eq!(
            found(&beside),
            Some(Shards {
                folder: "wds/train",
                count: 9
            }),
            "the folder with the most shards"
        );
    }

    #[test]
    fn few_mixed_or_unnumbered_tars_are_not() {
        assert_eq!(found(&numbered("", "train-", ".tar", 7)), None, "seven");
        let mut split = numbered("a/", "train-", ".tar", 4);
        split.extend(numbered("b/", "train-", ".tar", 4));
        assert_eq!(found(&split), None, "four in each of two folders");
        let prefixes: Vec<String> = ('a'..='h')
            .map(|prefix| format!("{prefix}-1.tar"))
            .collect();
        assert_eq!(found(&prefixes), None, "eight prefixes");
        let widths: Vec<String> = (1..=8)
            .map(|width| format!("part-{}.tar", "7".repeat(width)))
            .collect();
        assert_eq!(found(&widths), None, "eight digit widths");
        let unnumbered: Vec<String> = ('a'..='h').map(|name| format!("{name}.tar")).collect();
        assert_eq!(found(&unnumbered), None, "no digits");
        for ending in [".zip", ".tar.xz", ".tar.bak", "tar"] {
            assert_eq!(found(&numbered("", "train-", ending, 8)), None, "{ending}");
        }
        let parts: Vec<String> = (0..8).map(|part| format!("train.tar.{part:03}")).collect();
        assert_eq!(found(&parts), None, "the parts of one split tar");
    }

    #[test]
    fn digit_only_names_count() {
        assert_eq!(
            found(&numbered("", "", ".tar", 8)),
            Some(Shards {
                folder: "",
                count: 8
            })
        );
        let tens: Vec<String> = (10..18).map(|number| format!("{number}.tar")).collect();
        assert_eq!(
            found(&tens),
            Some(Shards {
                folder: "",
                count: 8
            })
        );
    }

    /// a listing as large as a big webdataset's, counted in its one pass.
    #[test]
    fn a_hundred_thousand_shards_are_counted() {
        let paths = numbered("train/", "", ".tar", 100_000);
        assert_eq!(
            found(&paths),
            Some(Shards {
                folder: "train",
                count: 100_000
            })
        );
    }

    #[test]
    fn the_hint_names_the_shards_and_how_to_keep_them_packed() {
        let top = Shards {
            folder: "",
            count: 1024,
        };
        assert_eq!(
            top.hint("shards", "--no-extract, or extract = false in data.toml"),
            "shards holds 1,024 numbered .tar shards, which webdataset loaders read packed. keep them packed with --no-extract, or extract = false in data.toml"
        );
        let inside = Shards {
            folder: "wds/train",
            count: 8,
        };
        assert_eq!(
            inside.hint("s3://bucket/", "--no-extract"),
            "s3://bucket/ holds 8 numbered .tar shards in wds/train, which webdataset loaders read packed. keep them packed with --no-extract"
        );
    }
}
