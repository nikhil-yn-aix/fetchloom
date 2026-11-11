//! the two sides git leaves in a file when a merge could not combine them: lines at the start of
//! a line, `<<<<<<<` before our side, `|||||||` before the merge base in git's diff3 and zdiff3
//! styles, `=======` before their side and `>>>>>>>` after it. the lines outside them are on
//! both sides. zdiff3 also moves the lines both sides share at either end of a conflict out of
//! it, though the base may never have had them, so a base read from its conflict lines may hold
//! a table cut in two.

use crate::error::{Error, Kind};

/// a file as it is on each side of a merge, and as it was at the merge base when every conflict
/// shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sides {
    pub(crate) ours: String,
    pub(crate) theirs: String,
    /// the lines outside the conflicts, with each conflict's base where the conflict was.
    pub(crate) base: Option<String>,
    /// [`Sides::base`] without the lines outside the conflicts that start or end a table a
    /// conflict cuts, the lines zdiff3 may have moved out of it: a second reading of the base,
    /// for when the first does not read. none when it is the first.
    pub(crate) trimmed: Option<String>,
    /// whether every conflict showed git's merge base, as the diff3 and zdiff3 styles do.
    pub(crate) shows_base: bool,
    /// each side's file whole, ours then theirs, as git holds it for the merge, rebase,
    /// cherry-pick or revert: lines git merged by itself belong to the side that wrote them, not
    /// to both, as they do in `ours` and `theirs`. none when git cannot tell.
    pub(crate) whole: Option<[String; 2]>,
}

/// a stretch of a file with conflict lines, as the base sees it: lines outside every conflict, or
/// what one conflict shows of the base.
enum Part<'a> {
    Outside(Vec<&'a str>),
    Base(Vec<&'a str>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    Ours,
    Base,
    Theirs,
    End,
}

/// the conflict line `line` is, without its line break.
fn marker(line: &str) -> Option<Marker> {
    if line == "=======" {
        return Some(Marker::Theirs);
    }
    [
        ("<<<<<<<", Marker::Ours),
        ("|||||||", Marker::Base),
        (">>>>>>>", Marker::End),
    ]
    .into_iter()
    .find(|(start, _)| {
        line.strip_prefix(start)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
    })
    .map(|(_, marker)| marker)
}

/// `line` without its line break, which may be a windows one.
fn bare(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}

/// the number, from 0, of the first conflict line in `text`, if it has one.
pub(crate) fn first(text: &str) -> Option<usize> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.split_inclusive('\n')
        .position(|line| marker(bare(line)).is_some())
}

/// the sides of `text`, the file `file`, or none when it holds no conflict lines. lines out of
/// git's order, or a conflict left open, are refused.
pub(crate) fn split(text: &str, file: &str) -> Result<Option<Sides>, Error> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut sides = Sides {
        ours: String::with_capacity(text.len()),
        theirs: String::with_capacity(text.len()),
        ..Sides::default()
    };
    let mut parts: Vec<Part<'_>> = Vec::new();
    let (mut conflicts, mut based) = (0, 0);
    let mut inside: Option<Marker> = None;
    let mut last = 0;
    for (number, line) in text.split_inclusive('\n').enumerate() {
        last = number;
        inside = match (marker(bare(line)), inside) {
            (Some(Marker::Ours), None) => {
                conflicts += 1;
                parts.push(Part::Base(Vec::new()));
                Some(Marker::Ours)
            }
            (Some(Marker::Base), Some(Marker::Ours)) => {
                based += 1;
                Some(Marker::Base)
            }
            (Some(Marker::Theirs), Some(Marker::Ours | Marker::Base)) => Some(Marker::Theirs),
            (Some(Marker::End), Some(Marker::Theirs)) => None,
            (Some(_), _) => return Err(out_of_order(file, number)),
            (None, None) => {
                sides.ours.push_str(line);
                sides.theirs.push_str(line);
                match parts.last_mut() {
                    Some(Part::Outside(lines)) => lines.push(line),
                    _ => parts.push(Part::Outside(vec![line])),
                }
                None
            }
            (None, Some(side)) => {
                match side {
                    Marker::Ours => sides.ours.push_str(line),
                    Marker::Base => {
                        if let Some(Part::Base(lines)) = parts.last_mut() {
                            lines.push(line);
                        }
                    }
                    Marker::Theirs | Marker::End => sides.theirs.push_str(line),
                }
                Some(side)
            }
        };
    }
    if inside.is_some() {
        return Err(out_of_order(file, last));
    }
    if conflicts == 0 {
        return Ok(None);
    }
    sides.shows_base = based == conflicts;
    if sides.shows_base {
        let base = whole(&parts, &[]);
        let trimmed = whole(&parts, &cut_tables(&parts));
        sides.trimmed = (trimmed != base).then_some(trimmed);
        sides.base = Some(base);
    }
    Ok(Some(sides))
}

/// whether `line` starts a TOML table, as `[datasets.iris]` and `[[dataset]]` do.
fn starts_table(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

/// for each outside stretch of `parts`, by its place, how many lines from its start and from its
/// end belong to a table a conflict cuts: from the conflict before it to the first table that
/// starts in it, and from the last table that starts in it, its first line included, to the
/// conflict after it. a stretch with no table start belongs to its conflicts whole.
fn cut_tables(parts: &[Part<'_>]) -> Vec<(usize, usize)> {
    parts
        .iter()
        .enumerate()
        .map(|(at, part)| {
            let Part::Outside(lines) = part else {
                return (0, 0);
            };
            let after_conflict = at > 0;
            let before_conflict = at + 1 < parts.len();
            let head = if after_conflict {
                lines
                    .iter()
                    .position(|line| starts_table(line))
                    .unwrap_or(lines.len())
            } else {
                0
            };
            let tail = if before_conflict {
                lines
                    .iter()
                    .rposition(|line| starts_table(line))
                    .map_or(lines.len(), |start| lines.len() - start)
            } else {
                0
            };
            (head, tail.min(lines.len() - head))
        })
        .collect()
}

/// the base `parts` give, each outside stretch without the lines `cut` names at its start and
/// end, by its place in `parts`.
fn whole(parts: &[Part<'_>], cut: &[(usize, usize)]) -> String {
    let mut base = String::new();
    for (at, part) in parts.iter().enumerate() {
        let lines = match part {
            Part::Outside(lines) | Part::Base(lines) => lines,
        };
        let (head, tail) = cut.get(at).copied().unwrap_or_default();
        for line in &lines[head..lines.len() - tail] {
            base.push_str(line);
        }
    }
    base
}

fn out_of_order(file: &str, number: usize) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!(
            "{file} has git's conflict lines out of order, so its two sides cannot be told apart"
        ),
    )
    .detail(format!("{file}:{}", number + 1))
    .help(format!(
        "resolve the conflict in {file} by hand, then run `fetchloom lock`"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what git 2.43 wrote when two branches each added a dataset before `core`, in its three
    /// conflict styles.
    const MERGE: &str = "<<<<<<< HEAD\n[datasets.alpha]\nref = \"zenodo:3\"\n=======\n[datasets.beta]\nref = \"zenodo:2\"\n>>>>>>> theirs\n\n[datasets.core]\nref = \"zenodo:1\"\n";
    const ZDIFF3: &str = "<<<<<<< HEAD\n[datasets.alpha]\nref = \"zenodo:3\"\n||||||| b9e17f5\n=======\n[datasets.beta]\nref = \"zenodo:2\"\n>>>>>>> theirs\n\n[datasets.core]\nref = \"zenodo:1\"\n";
    const DIFF3: &str = "<<<<<<< HEAD\n[datasets.alpha]\nref = \"zenodo:3\"\n\n||||||| b9e17f5\n=======\n[datasets.beta]\nref = \"zenodo:2\"\n\n>>>>>>> theirs\n[datasets.core]\nref = \"zenodo:1\"\n";
    const OURS: &str =
        "[datasets.alpha]\nref = \"zenodo:3\"\n\n[datasets.core]\nref = \"zenodo:1\"\n";
    const THEIRS: &str =
        "[datasets.beta]\nref = \"zenodo:2\"\n\n[datasets.core]\nref = \"zenodo:1\"\n";

    #[test]
    fn markers_split_into_two_or_three_sides() {
        let sides = |text: &str| split(text, "data.toml").unwrap().unwrap();
        assert_eq!(
            sides(MERGE),
            Sides {
                ours: OURS.to_owned(),
                theirs: THEIRS.to_owned(),
                ..Sides::default()
            }
        );
        let zdiff3 = sides(ZDIFF3);
        assert_eq!(
            (zdiff3.ours.as_str(), zdiff3.theirs.as_str()),
            (OURS, THEIRS)
        );
        assert_eq!(
            zdiff3.base.as_deref(),
            Some("\n[datasets.core]\nref = \"zenodo:1\"\n")
        );
        let diff3 = sides(DIFF3);
        assert_eq!((diff3.ours.as_str(), diff3.theirs.as_str()), (OURS, THEIRS));
        assert_eq!(
            diff3.base.as_deref(),
            Some("[datasets.core]\nref = \"zenodo:1\"\n")
        );
        let windows = sides(&MERGE.replace('\n', "\r\n"));
        assert_eq!(windows.ours, OURS.replace('\n', "\r\n"));
        assert_eq!(windows.theirs, THEIRS.replace('\n', "\r\n"));
        assert_eq!(split(OURS, "data.toml").unwrap(), None);
        assert_eq!(first(OURS), None);
        assert_eq!(first(&format!("\u{feff}{ZDIFF3}")), Some(0));
        assert_eq!(
            first(THEIRS.replace("[datasets.beta]", "=======").as_str()),
            Some(0)
        );
    }

    /// zdiff3 moved lines both sides added out of each conflict, as git 2.43 wrote them when both
    /// branches added labels and one other dataset each: the base read around them leaves out
    /// every table a conflict cuts, while the base read with them does not read.
    #[test]
    fn the_base_is_read_again_around_what_zdiff3_moved_out() {
        let manifest = "<<<<<<< HEAD\n[datasets.alpha]\n||||||| 3507bb5\n=======\n[datasets.beta]\n>>>>>>> B\nref = \"local\"\n\n[datasets.kappa]\nref = \"local\"\n\n[datasets.labels]\nref = \"local\"\n";
        let sides = split(manifest, "data.toml").unwrap().unwrap();
        assert!(sides.shows_base);
        assert_eq!(
            sides.base.as_deref(),
            Some(
                "ref = \"local\"\n\n[datasets.kappa]\nref = \"local\"\n\n[datasets.labels]\nref = \"local\"\n"
            )
        );
        assert_eq!(
            sides.trimmed.as_deref(),
            Some("[datasets.kappa]\nref = \"local\"\n\n[datasets.labels]\nref = \"local\"\n")
        );
        let lock = "version = 1\n\n[[dataset]]\n<<<<<<< HEAD\nname = \"alpha\"\nfiles = [\n  { path = \"a.txt\" },\n||||||| 3507bb5\n=======\nname = \"beta\"\nfiles = [\n  { path = \"b.txt\" },\n>>>>>>> B\n]\n\n[[dataset]]\nname = \"kappa\"\nfiles = [\n  { path = \"x.txt\" },\n]\n\n[[dataset]]\nname = \"labels\"\nref = \"local\"\n<<<<<<< HEAD\nfiles = [\n  { path = \"l.txt\", size = 4 },\n||||||| 3507bb5\n=======\nfiles = [\n  { path = \"l.txt\", size = 5 },\n>>>>>>> B\n]\n";
        let sides = split(lock, "data.lock").unwrap().unwrap();
        assert_eq!(
            sides.trimmed.as_deref(),
            Some(
                "version = 1\n\n[[dataset]]\nname = \"kappa\"\nfiles = [\n  { path = \"x.txt\" },\n]\n\n"
            )
        );
        // no table is cut here, so there is no second reading, and none without a base
        assert_eq!(split(DIFF3, "data.toml").unwrap().unwrap().trimmed, None);
        assert_eq!(split(MERGE, "data.toml").unwrap().unwrap().trimmed, None);
    }

    /// a base only some conflicts show is no base.
    #[test]
    fn a_base_in_only_some_conflicts_is_none() {
        let mixed = format!(
            "{ZDIFF3}{}",
            MERGE.replace("alpha", "gamma").replace("beta", "delta")
        );
        let sides = split(&mixed, "data.toml").unwrap().unwrap();
        assert_eq!(sides.base, None);
        assert!(sides.ours.contains("gamma") && !sides.ours.contains("delta"));
    }

    #[test]
    fn markers_out_of_order_are_refused() {
        for (text, line) in [
            ("<<<<<<< HEAD\na = 1\n", 2),
            ("a = 1\n=======\n", 2),
            ("<<<<<<< HEAD\n<<<<<<< HEAD\n", 2),
            (">>>>>>> theirs\n", 1),
            ("<<<<<<< HEAD\n=======\n||||||| base\n>>>>>>> x\n", 3),
            ("<<<<<<< HEAD\n=======\nb = 2\n", 3),
        ] {
            let error = split(text, "data.lock").unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid, "{text}");
            assert_eq!(
                error.message,
                "data.lock has git's conflict lines out of order, so its two sides cannot be told apart"
            );
            assert_eq!(error.details, [format!("data.lock:{line}")], "{text}");
        }
        assert_eq!(
            split("<<<<<<<< not a marker\n", "data.toml").unwrap(),
            None,
            "only git's seven characters count"
        );
    }
}
