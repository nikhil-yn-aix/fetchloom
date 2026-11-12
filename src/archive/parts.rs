//! archives cut into parts, as `split` and 7-Zip cut `scans.zip.001` to `.003`, which read in
//! order are the archive. rar volumes and spanned zips, which only their tools join, stay packed.

use std::collections::HashMap;

use super::{Packed, by_name};

/// what the names of a listing's files say about archives in parts, each file by its index.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Split {
    /// each archive whose parts are all there, its parts in order.
    pub(crate) sets: Vec<Vec<usize>>,
    /// each archive with a part missing after its first: the parts there, in order, and the path
    /// of the first part missing.
    pub(crate) incomplete: Vec<(Vec<usize>, String)>,
    /// rar volumes and spanned zips, which stay packed.
    pub(crate) volumes: Vec<Volumes>,
}

/// an archive in volumes only its own tools join.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Volumes {
    /// every volume, first the one those tools open: the first rar volume or a spanned `.zip`.
    pub(crate) files: Vec<usize>,
    /// `rar` or `zip`.
    pub(crate) format: &'static str,
}

/// the split archives, rar volumes and spanned zips among `paths`. parts group by folder, archive
/// name and numbering, from a first part on: a lone `scans.zip.002` is a plain file.
pub(crate) fn split<'a>(paths: impl IntoIterator<Item = &'a str>) -> Split {
    let mut groups: HashMap<Group<'_>, Vec<(&str, usize)>> = HashMap::new();
    let mut rars: HashMap<&str, Vec<(u64, usize)>> = HashMap::new();
    let mut zips: HashMap<String, usize> = HashMap::new();
    // for each stem, the `.z01` and on that a spanned zip's `.zip` follows
    let mut spans: HashMap<String, Vec<(&str, usize)>> = HashMap::new();
    for (index, path) in paths.into_iter().enumerate() {
        if let Some(part) = part(path) {
            let group = Group {
                archive: part.archive,
                separator: part.separator,
                alphabet: part.alphabet,
            };
            groups.entry(group).or_default().push((part.counter, index));
        } else if let Some((stem, number)) = rar_volume(path) {
            rars.entry(stem).or_default().push((number, index));
        } else if let Some((stem, ending)) = name_ending(path) {
            let ending = ending.to_ascii_lowercase();
            if ending == "zip" {
                zips.insert(stem.to_ascii_lowercase(), index);
            } else if ending.len() == 3 && ending.starts_with('z') && is_digits(&ending[1..]) {
                let span = spans.entry(stem.to_ascii_lowercase()).or_default();
                span.push((&path[path.len() - 2..], index));
            }
        }
    }
    let mut split = Split::default();
    for (group, mut parts) in groups {
        let Group {
            archive,
            separator,
            alphabet,
        } = group;
        parts.sort_by(|a, b| (a.0.len(), a.0).cmp(&(b.0.len(), b.0)));
        let counters: Vec<&str> = parts.iter().map(|(counter, _)| *counter).collect();
        let Some((scheme, found)) = numbering(&counters, alphabet) else {
            continue;
        };
        let indexes: Vec<usize> = parts.iter().map(|(_, index)| *index).collect();
        if found == counters.len() {
            split.sets.push(indexes);
        } else if let Some(missing) = scheme.counter(found as u64) {
            split
                .incomplete
                .push((indexes, format!("{archive}{separator}{missing}")));
        }
    }
    for (_, mut volumes) in rars {
        volumes.sort_unstable();
        split.volumes.push(Volumes {
            files: volumes.into_iter().map(|(_, index)| index).collect(),
            format: "rar",
        });
    }
    for (stem, index) in zips {
        let Some(mut spanned) = spans.remove(&stem) else {
            continue;
        };
        spanned.sort_unstable();
        if spanned[0].0 == "01" {
            let files = std::iter::once(index).chain(spanned.into_iter().map(|(_, index)| index));
            split.volumes.push(Volumes {
                files: files.collect(),
                format: "zip",
            });
        }
    }
    split.sets.sort_unstable();
    split.incomplete.sort_unstable();
    split
        .volumes
        .sort_unstable_by_key(|volumes| volumes.files[0]);
    split
}

/// the path of the whole archive `part` is a part of: `raw/scans.zip` for `raw/scans.zip.001`.
pub(crate) fn whole(part: &str) -> Option<&str> {
    self::part(part).map(|part| part.archive)
}

/// the parts of one archive under one numbering.
#[derive(PartialEq, Eq, Hash)]
struct Group<'a> {
    archive: &'a str,
    separator: &'a str,
    alphabet: Alphabet,
}

/// the digits or letters a part's counter is written in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Alphabet {
    Digits,
    Letters,
}

impl Alphabet {
    fn of(counter: &str) -> Option<Alphabet> {
        if is_digits(counter) {
            Some(Alphabet::Digits)
        } else if !counter.is_empty() && counter.bytes().all(|byte| byte.is_ascii_lowercase()) {
            Some(Alphabet::Letters)
        } else {
            None
        }
    }

    fn characters(self) -> &'static [u8] {
        match self {
            Alphabet::Digits => b"0123456789",
            Alphabet::Letters => b"abcdefghijklmnopqrstuvwxyz",
        }
    }
}

/// one file's name read as a part: `raw/scans.zip`, `.` and `001` for `raw/scans.zip.001`.
struct Part<'a> {
    archive: &'a str,
    separator: &'a str,
    counter: &'a str,
    alphabet: Alphabet,
}

/// `path` read as a part of an archive fetchloom unpacks: its path, `.` or a `.part` separator,
/// then a counter, of two or more characters after a bare `.` so `data.tar.gz.1` stays a file.
fn part(path: &str) -> Option<Part<'_>> {
    let (stem, tail) = name_ending(path)?;
    if by_name(path).is_some() {
        return None;
    }
    let (archive, counter, worded) = match tail.get(..4) {
        Some(word) if is_part(word) => {
            let rest = &tail[4..];
            (stem, rest.strip_prefix(['-', '_']).unwrap_or(rest), true)
        }
        _ => match name_ending(stem) {
            Some((archive, word)) if is_part(word) => (archive, tail, true),
            _ => (stem, tail, false),
        },
    };
    let alphabet = Alphabet::of(counter)?;
    let shortest = if worded && alphabet == Alphabet::Digits {
        1
    } else {
        2
    };
    if counter.len() < shortest || by_name(archive) != Some(Packed::Archive) {
        return None;
    }
    let separator = &path[archive.len()..path.len() - counter.len()];
    Some(Part {
        archive,
        separator,
        counter,
        alphabet,
    })
}

/// a rar volume named `<stem>.part<number>.rar`, as rar 3 on names them: its stem and number.
fn rar_volume(path: &str) -> Option<(&str, u64)> {
    let (head, ending) = name_ending(path)?;
    let (stem, word) = name_ending(head)?;
    let number = word.get(4..).filter(|number| is_digits(number))?;
    if !ending.eq_ignore_ascii_case("rar") || !is_part(&word[..4]) {
        return None;
    }
    Some((stem, number.parse().ok()?))
}

/// `path` cut at the last `.` of its file name, the folder staying with what comes before.
fn name_ending(path: &str) -> Option<(&str, &str)> {
    let (stem, ending) = path.rsplit_once('.')?;
    (!ending.contains('/')).then_some((stem, ending))
}

fn is_part(word: &str) -> bool {
    word.eq_ignore_ascii_case("part")
}

fn is_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// a way tools number parts.
#[derive(Clone, Copy, Debug)]
enum Scheme {
    /// a number from `start`, zero-padded to `width`, then as wide as it grows: `split -d` with
    /// fewer than 90 parts or `-a`, and 7-Zip's `001` to `999`, then `1000`.
    Numbered { start: u64, width: usize },
    /// letters from all `a`s, all `width` wide: `split -a`.
    Lettered { width: usize },
    /// GNU split's own: two characters, until a first character would be the last of the
    /// alphabet, which then stays and two more follow: `00` to `89`, then `9000`.
    Widening { alphabet: Alphabet },
}

impl Scheme {
    /// the counter of the part at `index`, from 0, or none past what the scheme can name.
    fn counter(self, index: u64) -> Option<String> {
        match self {
            Scheme::Numbered { start, width } => Some(format!("{:0width$}", start + index)),
            Scheme::Lettered { width } => written(index, width, Alphabet::Letters),
            Scheme::Widening { alphabet } => {
                let characters = alphabet.characters();
                let size = characters.len() as u64;
                let last = char::from(*characters.last()?);
                let (mut index, mut prefix, mut width) = (index, String::new(), 2);
                loop {
                    let count = (size - 1)
                        .checked_mul(size.checked_pow(u32::try_from(width - 1).ok()?)?)?;
                    if index < count {
                        return Some(prefix + &written(index, width, alphabet)?);
                    }
                    index -= count;
                    prefix.push(last);
                    width += 1;
                }
            }
        }
    }
}

/// `value` in `alphabet`'s digits, `width` of them, or none when it needs more.
fn written(mut value: u64, width: usize, alphabet: Alphabet) -> Option<String> {
    let characters = alphabet.characters();
    let size = characters.len() as u64;
    let mut text = vec![0; width];
    for at in text.iter_mut().rev() {
        *at = characters[usize::try_from(value % size).ok()?];
        value /= size;
    }
    if value != 0 {
        return None;
    }
    String::from_utf8(text).ok()
}

/// how the sorted `counters` are numbered, and how many in a row from the first it names, all for
/// a complete set. none when the first starts no numbering, as `002` does not.
fn numbering(counters: &[&str], alphabet: Alphabet) -> Option<(Scheme, usize)> {
    let width = counters.first()?.len();
    let schemes = match alphabet {
        Alphabet::Digits => vec![
            Scheme::Numbered { start: 0, width },
            Scheme::Numbered { start: 1, width },
            Scheme::Widening { alphabet },
        ],
        Alphabet::Letters => vec![Scheme::Lettered { width }, Scheme::Widening { alphabet }],
    };
    let named = |scheme: Scheme| {
        counters
            .iter()
            .enumerate()
            .take_while(|(index, counter)| {
                scheme.counter(*index as u64).as_deref() == Some(**counter)
            })
            .count()
    };
    // of the numberings that name the first counter, the one naming most in a row; at a gap, one
    // whose next counter is as wide as the counter found there
    schemes
        .into_iter()
        .map(|scheme| (scheme, named(scheme)))
        .filter(|(_, found)| *found > 0)
        .max_by_key(|(scheme, found)| {
            let next = counters.get(*found).map(|counter| counter.len());
            let fits = scheme.counter(*found as u64).map(|counter| counter.len()) == next;
            (*found, fits)
        })
}

#[cfg(test)]
mod tests {
    use super::{Split, Volumes, split, whole};

    fn found(paths: &[&str]) -> Split {
        split(paths.iter().copied())
    }

    fn sets(paths: &[&str]) -> Vec<Vec<usize>> {
        found(paths).sets
    }

    #[test]
    fn numbered_and_lettered_parts_are_one_archive_in_order() {
        assert_eq!(
            sets(&[
                "scans.zip.003",
                "README.md",
                "scans.zip.001",
                "scans.zip.002"
            ]),
            [vec![2, 3, 0]]
        );
        assert_eq!(
            sets(&["train.tar.gz.ab", "train.tar.gz.aa"]),
            [vec![1, 0]],
            "split's letters"
        );
        assert_eq!(
            sets(&["x.tar.gz.00", "x.tar.gz.01"]),
            [vec![0, 1]],
            "split -d"
        );
        assert_eq!(sets(&["x.7z.001", "x.7z.002"]), [vec![0, 1]], "7-Zip");
        assert_eq!(
            sets(&[
                "x.tar.partab",
                "x.tar.partaa",
                "x.tar.part-aa",
                "x.tar.part-ab"
            ]),
            [vec![1, 0], vec![2, 3]],
            "worded parts, one numbering each"
        );
        assert_eq!(
            sets(&[
                "d.ZIP.Part1",
                "d.ZIP.Part2",
                "e.zip.part_01",
                "f.tar.part.000"
            ]),
            [vec![0, 1], vec![2], vec![3]],
        );
    }

    #[test]
    fn a_tenth_part_comes_after_the_ninth() {
        let mut paths: Vec<String> = (1..=10).map(|n| format!("x.zip.{n:03}")).collect();
        paths.reverse();
        let sorted = found(&paths.iter().map(String::as_str).collect::<Vec<_>>()).sets;
        assert_eq!(sorted, [(0..10).rev().collect::<Vec<_>>()]);
        let worded: Vec<String> = (1..=10).map(|n| format!("x.zip.part{n}")).collect();
        let split = found(&worded.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(
            split.sets,
            [(0..10).collect::<Vec<_>>()],
            "part10 after part9"
        );
    }

    #[test]
    fn numbering_past_its_first_width_continues() {
        // 7-Zip's 1000th volume, and GNU split's 91st part with its own widening
        let seven: Vec<String> = (1..=1000).map(|n| format!("big.7z.{n:03}")).collect();
        assert_eq!(seven[999], "big.7z.1000");
        let gnu: Vec<String> = (0..90)
            .map(|n| format!("big.tar.{n:02}"))
            .chain((0..10).map(|n| format!("big.tar.90{n:02}")))
            .collect();
        let letters: Vec<String> = ('a'..='y')
            .flat_map(|first| ('a'..='z').map(move |second| format!("big.zip.{first}{second}")))
            .chain(["big.zip.zaaa".to_owned(), "big.zip.zaab".to_owned()])
            .collect();
        assert_eq!(letters.len(), 652);
        for paths in [seven, gnu, letters] {
            let split = found(&paths.iter().map(String::as_str).collect::<Vec<_>>());
            assert_eq!(split.sets, [(0..paths.len()).collect::<Vec<_>>()]);
        }
    }

    #[test]
    fn a_missing_part_is_named() {
        let split = found(&["scans.zip.001", "scans.zip.003"]);
        assert_eq!(split.sets, Vec::<Vec<usize>>::new());
        assert_eq!(split.incomplete, [(vec![0, 1], "scans.zip.002".to_owned())]);
        assert_eq!(
            found(&["d/t.tar.gz.aa", "d/t.tar.gz.ac"]).incomplete,
            [(vec![0, 1], "d/t.tar.gz.ab".to_owned())]
        );
        let gnu: Vec<String> = (0..90)
            .map(|n| format!("t.tar.{n:02}"))
            .chain(["t.tar.9001".to_owned()])
            .collect();
        let split = found(&gnu.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(split.incomplete[0].1, "t.tar.9000", "GNU split's 91st part");
    }

    #[test]
    fn a_lone_part_starts_a_set_only_when_it_is_the_first() {
        assert_eq!(sets(&["scans.zip.001"]), [vec![0]], "a set of one");
        assert_eq!(sets(&["scans.zip.000"]), [vec![0]]);
        for later in [
            &["scans.zip.002"][..],
            &["scans.zip.002", "scans.zip.003"],
            &["model.zip.asc", "model.zip.sig"],
        ] {
            assert_eq!(found(later), Split::default(), "{later:?}");
        }
    }

    #[test]
    fn other_names_are_no_parts() {
        for paths in [
            &["weights.bin.001", "weights.bin.002"][..],
            &["data.tar.gz", "data.tar.xz"],
            &["data.tar.gz.1", "data.tar.gz.2"],
            &["notes.zip.md5", "notes.zip.txt"],
            &["train-000.tar", "train-001.tar"],
            &["x.rar.001", "x.rar.002"],
            &["x.zip.0a", "x.zip.part"],
        ] {
            assert_eq!(found(paths), Split::default(), "{paths:?}");
        }
    }

    #[test]
    fn parts_never_join_across_folders() {
        let split = found(&["a/x.zip.001", "b/x.zip.002", "b/x.zip.001", "a/x.zip.002"]);
        assert_eq!(split.sets, [vec![0, 3], vec![2, 1]]);
    }

    #[test]
    fn rar_volumes_and_spanned_zips_are_named_for_their_own_tools() {
        let split = found(&[
            "clips.part2.rar",
            "clips.part1.rar",
            "scans.z01",
            "scans.zip",
            "scans.z02",
            "whole.zip",
        ]);
        assert_eq!(
            split.volumes,
            [
                Volumes {
                    files: vec![1, 0],
                    format: "rar"
                },
                Volumes {
                    files: vec![3, 2, 4],
                    format: "zip"
                },
            ]
        );
        assert_eq!(split.sets, Vec::<Vec<usize>>::new());
    }

    #[test]
    fn a_part_names_its_archive() {
        assert_eq!(whole("raw/scans.zip.001"), Some("raw/scans.zip"));
        assert_eq!(whole("train.tar.gz.part-aa"), Some("train.tar.gz"));
        assert_eq!(whole("raw/scans.zip"), None);
    }
}
