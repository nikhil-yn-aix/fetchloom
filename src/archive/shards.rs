//! numbered tar files in one folder: the shards webdataset loaders read packed. unpacking them
//! turns a few large files into many small ones, which costs inodes and a cluster's metadata
//! server, so a person who sees them before anything downloads can keep them packed.

use std::collections::HashMap;

use crate::{glob, units};

/// numbered tar shards in one folder of a listing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shards<'a> {
    /// the folder that holds them, with `/` between folders, empty at the top of the listing.
    pub(crate) folder: &'a str,
    pub(crate) count: u64,
    /// the part of each name before its number.
    prefix: &'a str,
    /// the ending every one of them has as written, none when they differ, as `.tar` and `.TAR`.
    ending: Option<&'a str>,
}

/// the fewest numbered tars that look like shards rather than a dataset's few parts.
const FEWEST: u64 = 8;

/// the endings of a shard, compared without case.
const ENDINGS: [&str; 3] = [".tar", ".tar.gz", ".tgz"];

impl Shards<'_> {
    /// the hint for these shards in `owner`, a dataset or a link, with `keep`, how the command
    /// that found them keeps archives packed, and for the project's dataset `dataset`, the one
    /// members table that keeps only some files of every shard.
    pub(crate) fn hint(&self, owner: &str, keep: &str, dataset: Option<&str>) -> String {
        let packed = format!(
            "{owner} holds {}, which webdataset loaders read packed. keep them packed with {keep}",
            self.told()
        );
        match dataset {
            Some(dataset) => format!(
                "{packed}. to unpack them and keep only some files of each, write one members table, [datasets.{dataset}.members.\"{}\"]",
                self.key()
            ),
            None => packed,
        }
    }

    /// how many there are, and the folder that holds them, as `8 numbered .tar shards in wds`.
    pub(crate) fn told(&self) -> String {
        let shards = units::count(self.count, "numbered .tar shard", "numbered .tar shards");
        if self.folder.is_empty() {
            shards
        } else {
            format!("{shards} in {}", self.folder)
        }
    }

    /// the members table key that matches every one of them, as `wds/shard-*.tar`: the folder
    /// and prefix as written, `*` for the number, and the ending they share.
    pub(crate) fn key(&self) -> String {
        let folder = if self.folder.is_empty() {
            String::new()
        } else {
            format!("{}/", glob::escape(self.folder))
        };
        let ending = self.ending.map(glob::escape).unwrap_or_default();
        format!("{folder}{}*{ending}", glob::escape(self.prefix))
    }
}

/// the shards seen so far of one folder, prefix and digit width.
struct Group<'a> {
    count: u64,
    /// the ending they all have as written, none once two differ.
    ending: Option<&'a str>,
}

/// the largest group of at least 8 files in one folder named `<prefix><digits>.tar`, `.tar.gz`
/// or `.tgz`, such as `train-000123.tar` or `00000000.tar`, with one prefix and one digit width.
/// one pass over `paths`, keyed by slices of them, so a listing of millions costs no copy of it.
pub(crate) fn shards<'a>(paths: impl IntoIterator<Item = &'a str>) -> Option<Shards<'a>> {
    let mut groups: HashMap<(&str, &str, usize), Group<'a>> = HashMap::new();
    let mut most: Option<Shards<'a>> = None;
    for path in paths {
        let Some((folder, prefix, width, ending)) = numbered(path) else {
            continue;
        };
        let group = groups.entry((folder, prefix, width)).or_insert(Group {
            count: 0,
            ending: Some(ending),
        });
        group.count += 1;
        if group.ending != Some(ending) {
            group.ending = None;
        }
        if group.count >= FEWEST && most.is_none_or(|most| group.count > most.count) {
            most = Some(Shards {
                folder,
                count: group.count,
                prefix,
                ending: group.ending,
            });
        }
    }
    most
}

/// the folder, the prefix, the number of digits and the ending as written of a tar named
/// `<prefix><digits>` and a shard's ending, or none for any other file.
fn numbered(path: &str) -> Option<(&str, &str, usize, &str)> {
    let (folder, name) = path.rsplit_once('/').unwrap_or(("", path));
    let at = ENDINGS.iter().find_map(|ending| {
        let at = name.len().checked_sub(ending.len())?;
        name.get(at..)?.eq_ignore_ascii_case(ending).then_some(at)
    })?;
    let stem = &name[..at];
    let width = stem.bytes().rev().take_while(u8::is_ascii_digit).count();
    (width > 0).then(|| (folder, &stem[..stem.len() - width], width, &name[at..]))
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

    /// the folder, the count and the members table key of the shards among `paths`.
    fn seen(paths: &[String]) -> Option<(&str, u64, String)> {
        found(paths).map(|shards| (shards.folder, shards.count, shards.key()))
    }

    #[test]
    fn numbered_tar_shards_are_noticed() {
        for ending in [".tar", ".tar.gz", ".tgz", ".TAR", ".Tar.Gz"] {
            assert_eq!(
                seen(&numbered("", "train-", ending, 8)),
                Some(("", 8, format!("train-*{ending}"))),
                "{ending}"
            );
        }
        let mut beside = numbered("wds/val/", "shard-", ".tar", 8);
        beside.extend(numbered("wds/train/", "shard-", ".tar", 9));
        beside.extend(["README.md".to_owned(), "wds/train/index.json".to_owned()]);
        assert_eq!(
            seen(&beside),
            Some(("wds/train", 9, "wds/train/shard-*.tar".to_owned())),
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
            seen(&numbered("", "", ".tar", 8)),
            Some(("", 8, "*.tar".to_owned()))
        );
        let tens: Vec<String> = (10..18).map(|number| format!("{number}.tar")).collect();
        assert_eq!(seen(&tens), Some(("", 8, "*.tar".to_owned())));
    }

    /// a listing as large as a big webdataset's, counted in its one pass.
    #[test]
    fn a_hundred_thousand_shards_are_counted() {
        let paths = numbered("train/", "", ".tar", 100_000);
        assert_eq!(
            found(&paths).map(|shards| (shards.folder, shards.count)),
            Some(("train", 100_000))
        );
    }

    /// the key matches every shard of the group as written: its ending only when they share
    /// one, and a `[`, `*` or `?` of the folder or prefix as itself.
    #[test]
    fn the_key_holds_the_group_s_own_folder_prefix_and_ending() {
        let mut mixed = numbered("", "train-", ".tar", 7);
        mixed.push("train-000007.tgz".to_owned());
        assert_eq!(seen(&mixed), Some(("", 8, "train-*".to_owned())));
        assert_eq!(
            seen(&numbered("v[1]/", "part?", ".tar", 8)),
            Some(("v[1]", 8, "v[[]1]/part[?]*.tar".to_owned()))
        );
    }

    #[test]
    fn the_hint_names_the_shards_and_how_to_keep_them_packed() {
        let paths = numbered("", "train-", ".tar", 1024);
        let top = found(&paths).unwrap();
        assert_eq!(
            top.hint(
                "shards",
                "--no-extract, or extract = false in data.toml",
                Some("shards")
            ),
            "shards holds 1,024 numbered .tar shards, which webdataset loaders read packed. keep them packed with --no-extract, or extract = false in data.toml. to unpack them and keep only some files of each, write one members table, [datasets.shards.members.\"train-*.tar\"]"
        );
        let paths = numbered("wds/train/", "shard-", ".tar", 8);
        let inside = found(&paths).unwrap();
        assert_eq!(
            inside.hint("s3://bucket/", "--no-extract", None),
            "s3://bucket/ holds 8 numbered .tar shards in wds/train, which webdataset loaders read packed. keep them packed with --no-extract"
        );
    }
}
