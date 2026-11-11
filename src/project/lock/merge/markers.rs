//! the two sides git leaves in a file when a merge could not combine them: lines at the start of
//! a line, `<<<<<<<` before our side, `|||||||` before the merge base in git's diff3 and zdiff3
//! styles, `=======` before their side and `>>>>>>>` after it. the lines outside them are on
//! both sides.

use crate::error::{Error, Kind};

/// a file as it is on each side of a merge, and as it was at the merge base when every conflict
/// shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Sides {
    pub(crate) ours: String,
    pub(crate) theirs: String,
    pub(crate) base: Option<String>,
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
        base: None,
    };
    let mut base = String::new();
    let (mut conflicts, mut based) = (0, 0);
    let mut inside: Option<Marker> = None;
    let mut last = 0;
    for (number, line) in text.split_inclusive('\n').enumerate() {
        last = number;
        inside = match (marker(bare(line)), inside) {
            (Some(Marker::Ours), None) => {
                conflicts += 1;
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
                base.push_str(line);
                None
            }
            (None, Some(side)) => {
                match side {
                    Marker::Ours => sides.ours.push_str(line),
                    Marker::Base => base.push_str(line),
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
    sides.base = (based == conflicts).then_some(base);
    Ok(Some(sides))
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
                base: None,
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
