//! data.toml's `required_version`: the fetchloom versions a project works with, written as uv's
//! `required-version` is, comparisons such as `>=0.2.0` joined by commas. it is read before every
//! other key, so a project that needs a newer fetchloom says so instead of failing on a key that
//! newer fetchloom added.

use std::cmp::Ordering;
use std::fmt;

use super::INSTALL;

/// the key, at the top of data.toml.
pub(crate) const KEY: &str = "required_version";

/// this fetchloom's version.
pub(crate) const RUNNING: &str = env!("CARGO_PKG_VERSION");

/// the versions a `required_version` allows: each of its comparisons holds.
#[derive(Debug)]
pub(crate) struct Range {
    written: String,
    clauses: Vec<Clause>,
}

#[derive(Debug)]
struct Clause {
    op: Op,
    release: Vec<u64>,
    /// `==0.2.*` or `!=0.2.*`: only as many numbers as are written count.
    prefix: bool,
    /// what `===` compares the version's text with.
    exactly: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    /// `~=0.2.1`: 0.2.1 or newer, below 0.3.
    Compatible,
    Equal,
    NotEqual,
    Below,
    AtMost,
    Above,
    AtLeast,
    /// `===`: the version's text, whole.
    Exactly,
}

/// longest first, so `<=` is not read as `<`.
const OPS: [(&str, Op); 8] = [
    ("===", Op::Exactly),
    ("~=", Op::Compatible),
    ("==", Op::Equal),
    ("!=", Op::NotEqual),
    ("<=", Op::AtMost),
    (">=", Op::AtLeast),
    ("<", Op::Below),
    (">", Op::Above),
];

/// how a fetchloom fares against a [`Range`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Allowed,
    /// a newer fetchloom is what the range asks for.
    Newer,
    /// the range leaves it out otherwise, as an older one or this one exactly.
    Other,
}

/// a fetchloom version: its release numbers, and whether it is a pre-release of them, as
/// `0.2.0-rc.1`, which comes before `0.2.0`.
struct Version<'a> {
    text: &'a str,
    release: Vec<u64>,
    pre: bool,
}

impl Range {
    /// the range `written` gives, or why it gives none, in words that follow "because".
    pub(crate) fn parse(written: &str) -> Result<Range, String> {
        let written = written.trim();
        if written.is_empty() {
            return Err("it names no version".to_owned());
        }
        let clauses = written
            .split(',')
            .map(Clause::parse)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Range {
            written: written.to_owned(),
            clauses,
        })
    }

    /// how fetchloom `running`, as Cargo.toml writes a version, fares against the range.
    pub(crate) fn verdict(&self, running: &str) -> Verdict {
        let running = Version::of(running);
        let refusing: Vec<&Clause> = self
            .clauses
            .iter()
            .filter(|clause| !clause.allows(&running))
            .collect();
        if refusing.is_empty() {
            Verdict::Allowed
        } else if refusing.iter().any(|clause| clause.wants_newer(&running)) {
            Verdict::Newer
        } else {
            Verdict::Other
        }
    }

    /// what to do when `verdict` is not [`Verdict::Allowed`].
    pub(crate) fn help(&self, verdict: Verdict) -> String {
        match verdict {
            Verdict::Newer => format!(
                "update fetchloom to a version that matches {self}, as {INSTALL} shows, then run the same command again"
            ),
            Verdict::Allowed | Verdict::Other => format!(
                "install a fetchloom that matches {self}, as {INSTALL} shows, then run the same command again"
            ),
        }
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.written)
    }
}

impl Clause {
    fn parse(text: &str) -> Result<Clause, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("a comma has no comparison on one side".to_owned());
        }
        let Some((op, rest)) = OPS
            .iter()
            .find_map(|(sign, op)| Some((*op, text.strip_prefix(sign)?.trim_start())))
        else {
            return Err(if release(text).is_some() {
                format!("{text} has no comparison before it, such as >={text} for {text} or newer")
            } else {
                format!("{text} is no comparison such as >=0.1.0")
            });
        };
        if op == Op::Exactly {
            if rest.is_empty() || rest.contains(char::is_whitespace) {
                return Err(format!("{text} needs one version after ==="));
            }
            return Ok(Clause {
                op,
                release: Vec::new(),
                prefix: false,
                exactly: rest.to_owned(),
            });
        }
        let (number, prefix) = match rest.strip_suffix(".*") {
            Some(number) => (number, true),
            None => (rest, false),
        };
        if prefix && !matches!(op, Op::Equal | Op::NotEqual) {
            return Err(format!("{text} ends in .*, which only == and != take"));
        }
        if number.is_empty() {
            return Err(format!("{text} needs a version after it"));
        }
        let Some(release) = release(number) else {
            return Err(format!(
                "{rest} is no version of numbers and dots, such as 0.1.0"
            ));
        };
        if op == Op::Compatible && release.len() < 2 {
            return Err(format!("{text} needs two numbers or more, as in ~=0.1"));
        }
        Ok(Clause {
            op,
            release,
            prefix,
            exactly: String::new(),
        })
    }

    fn allows(&self, running: &Version<'_>) -> bool {
        let order = running.order(&self.release);
        match self.op {
            Op::Exactly => running.text.eq_ignore_ascii_case(&self.exactly),
            Op::Equal if self.prefix => running.starts(&self.release),
            Op::NotEqual if self.prefix => !running.starts(&self.release),
            Op::Equal => order == Ordering::Equal,
            Op::NotEqual => order != Ordering::Equal,
            Op::AtLeast => order != Ordering::Less,
            Op::Above => order == Ordering::Greater,
            Op::AtMost => order != Ordering::Greater,
            // a pre-release of the version itself is not below it
            Op::Below => order == Ordering::Less && !running.releases(&self.release),
            Op::Compatible => {
                order != Ordering::Less && running.starts(&self.release[..self.release.len() - 1])
            }
        }
    }

    /// whether this comparison, refusing `running`, asks for a newer one.
    fn wants_newer(&self, running: &Version<'_>) -> bool {
        match self.op {
            Op::Equal if self.prefix => {
                compare(&padded(&running.release, self.release.len()), &self.release)
                    == Ordering::Less
            }
            Op::Equal | Op::AtLeast | Op::Compatible => {
                running.order(&self.release) == Ordering::Less
            }
            Op::Above => running.order(&self.release) != Ordering::Greater,
            Op::NotEqual | Op::Below | Op::AtMost | Op::Exactly => false,
        }
    }
}

impl Version<'_> {
    /// the version `text` names, as Cargo.toml writes one: numbers and dots, then a pre-release
    /// after `-`, then build words after `+`, which do not count.
    fn of(text: &str) -> Version<'_> {
        let numbered = text.split_once('+').map_or(text, |(numbered, _)| numbered);
        let (numbers, pre) = numbered.split_once('-').unwrap_or((numbered, ""));
        Version {
            text,
            release: release(numbers).unwrap_or_default(),
            pre: !pre.is_empty(),
        }
    }

    fn order(&self, release: &[u64]) -> Ordering {
        compare(&self.release, release).then(if self.pre {
            Ordering::Less
        } else {
            Ordering::Equal
        })
    }

    /// whether its numbers begin with `prefix`, the missing ones read as 0.
    fn starts(&self, prefix: &[u64]) -> bool {
        padded(&self.release, prefix.len())[..prefix.len()] == *prefix
    }

    /// whether it is `release` or a pre-release of it.
    fn releases(&self, release: &[u64]) -> bool {
        compare(&self.release, release) == Ordering::Equal
    }
}

/// the numbers of `text`, as `0.2.10` gives 0, 2 and 10, with an optional `v` before them, as
/// PEP 440 reads one; none for anything else, as a pre-release or a sign.
fn release(text: &str) -> Option<Vec<u64>> {
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    text.split('.')
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            part.parse().ok()
        })
        .collect()
}

/// `release` with 0s added until it has `length` numbers.
fn padded(release: &[u64], length: usize) -> Vec<u64> {
    let mut padded = release.to_vec();
    if padded.len() < length {
        padded.resize(length, 0);
    }
    padded
}

/// compares two releases as PEP 440 does, the shorter read with 0s added.
fn compare(a: &[u64], b: &[u64]) -> Ordering {
    let length = a.len().max(b.len());
    padded(a, length).cmp(&padded(b, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(range: &str, running: &str) -> Verdict {
        Range::parse(range)
            .unwrap_or_else(|reason| panic!("{range}: {reason}"))
            .verdict(running)
    }

    /// each comparison allows what uv's `required-version` allows, and says when a newer
    /// fetchloom is what it asks for.
    #[test]
    fn a_range_allows_the_versions_uv_would() {
        use Verdict::{Allowed, Newer, Other};
        for (range, running, expected) in [
            (">=0.1.0", "0.1.0", Allowed),
            (">=0.1", "0.1.0", Allowed),
            (">= 0.1.0", "0.1.3", Allowed),
            (">=0.2.0", "0.1.0", Newer),
            (">=0.2.0", "0.2.0-rc.1", Newer),
            (">=0.2.0", "0.10.0", Allowed),
            (">=v0.2", "0.2.0", Allowed),
            (">0.1.0", "0.1.0", Newer),
            (">0.1.0", "0.1.1", Allowed),
            ("==0.1.0", "0.1.0", Allowed),
            ("==0.1", "0.1.0", Allowed),
            ("==0.2.0", "0.1.0", Newer),
            ("==0.1.0", "0.2.0", Other),
            ("==0.1.0", "0.1.0+build.7", Allowed),
            ("==0.2.*", "0.2.9", Allowed),
            ("==0.2.*", "0.2.0-rc.1", Allowed),
            ("==0.3.*", "0.2.9", Newer),
            ("==0.1.*", "0.2.0", Other),
            ("!=0.1.0", "0.1.0", Other),
            ("!=0.1.*", "0.1.4", Other),
            ("!=0.1.*", "0.2.0", Allowed),
            ("<0.2", "0.1.9", Allowed),
            ("<0.2", "0.2.0", Other),
            ("<0.2.0", "0.2.0-rc.1", Other),
            ("<=0.2", "0.2.0", Allowed),
            ("<=0.2", "0.2.1", Other),
            ("~=0.2", "0.9.0", Allowed),
            ("~=0.2.1", "0.2.0", Newer),
            ("~=0.2.1", "0.2.7", Allowed),
            ("~=0.2.1", "0.3.0", Other),
            ("===0.1.0", "0.1.0", Allowed),
            ("===0.1", "0.1.0", Other),
            (">=0.1, <0.3", "0.2.0", Allowed),
            (">=0.3, <0.4", "0.2.0", Newer),
            (">=0.1, <0.2", "0.2.0", Other),
            (" >=0.1 , !=0.1.5 ", "0.1.5", Other),
        ] {
            assert_eq!(verdict(range, running), expected, "{range} with {running}");
        }
    }

    /// a value that is no range says why, in words that follow "because".
    #[test]
    fn a_value_that_is_no_range_says_why() {
        for (written, reason) in [
            ("", "it names no version"),
            ("  ", "it names no version"),
            (">=0.1,", "a comma has no comparison on one side"),
            (
                "0.2.0",
                "0.2.0 has no comparison before it, such as >=0.2.0 for 0.2.0 or newer",
            ),
            ("latest", "latest is no comparison such as >=0.1.0"),
            ("=>0.1", "=>0.1 is no comparison such as >=0.1.0"),
            (
                ">=0.2.0rc1",
                "0.2.0rc1 is no version of numbers and dots, such as 0.1.0",
            ),
            (
                ">=0.x",
                "0.x is no version of numbers and dots, such as 0.1.0",
            ),
            (
                ">=-1",
                "-1 is no version of numbers and dots, such as 0.1.0",
            ),
            (">=", ">= needs a version after it"),
            (">=0.2.*", ">=0.2.* ends in .*, which only == and != take"),
            ("~=1", "~=1 needs two numbers or more, as in ~=0.1"),
            ("===", "=== needs one version after ==="),
            (
                ">=99999999999999999999",
                "99999999999999999999 is no version of numbers and dots, such as 0.1.0",
            ),
        ] {
            assert_eq!(Range::parse(written).unwrap_err(), reason, "{written:?}");
        }
    }
}
