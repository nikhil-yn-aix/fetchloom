//! `fetchloom lock` after a git merge it could not finish: data.toml and data.lock merged one
//! dataset at a time, each disagreement the base cannot settle left to a person's choice. nothing
//! is resolved or downloaded: data.lock keeps a pin only when it answers the merged data.toml.

pub(crate) mod markers;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::Path;

use serde::Serialize;

use self::markers::Sides;
use super::{Lock, Locked, LockedFile, Parsed, parse, spec};
use crate::error::{Error, Kind};
use crate::project::manifest::{self, Edit, Entry, Manifest};
use crate::units;

/// one side of a merge, as git's `--ours` and `--theirs` and the stages of its index name them.
/// a rebase turns them around from the branch a person is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    /// stage 2, before `=======` in a conflict: the branch checked out in a merge, the branch
    /// rebased onto in a rebase.
    Ours,
    /// stage 3, after `=======`: the branch merged in, or the commit a rebase replays.
    Theirs,
}

impl Side {
    fn other(self) -> Side {
        match self {
            Side::Ours => Side::Theirs,
            Side::Theirs => Side::Ours,
        }
    }

    /// the side as the merge's messages name it.
    fn word(self) -> &'static str {
        match self {
            Side::Ours => "ours",
            Side::Theirs => "theirs",
        }
    }

    /// the side as a message says whose it is: "our" or "their".
    fn whose(self) -> &'static str {
        match self {
            Side::Ours => "our",
            Side::Theirs => "their",
        }
    }
}

/// the side a person chose for each dataset the sides disagree on.
pub(crate) type Choices = BTreeMap<String, Side>;

/// a part of data.lock.d as one side's commit holds it, by its path from the project, when git
/// has it there.
pub(crate) type Committed<'a> = &'a dyn Fn(Side, &str) -> Option<Vec<u8>>;

/// data.toml and data.lock merged, and how each dataset came out.
#[derive(Debug)]
pub(crate) struct Merged {
    /// data.toml's merged text.
    pub(crate) manifest: String,
    /// data.lock's merged datasets, with their files, or none when neither side has a lock.
    pub(crate) lock: Option<Lock>,
    pub(crate) summary: Summary,
    /// the datasets data.lock pins other than our side did: pinned anew, pinned to other data or
    /// files, or no longer pinned.
    pub(crate) repinned: Vec<String>,
    /// what the merge left behind that a person may want back, such as one side's comments.
    pub(crate) warnings: Vec<String>,
}

/// how each dataset came out of a merge, by name, as `lock --json` reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Summary {
    /// taken from our side: only ours changed or added it, or a person chose ours.
    pub(crate) from_ours: Vec<String>,
    /// taken from their side, the same way.
    pub(crate) from_theirs: Vec<String>,
    /// alike on both sides.
    pub(crate) same: Vec<String>,
    /// removed on one side while the other left it as it was, or by a choice.
    pub(crate) removed: Vec<String>,
    /// fetched datasets no data.lock entry of either side answers, which `fetchloom lock` pins.
    pub(crate) unpinned: Vec<String>,
    /// those that need a choice, in an error's result only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) conflicts: Vec<String>,
    /// what each side holds of each dataset that needs a choice, in an error's result only.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) sides: BTreeMap<String, Held>,
}

/// what each side holds of a dataset that needs a choice, as lines to compare: each key of its
/// data.toml entry the sides hold apart, or, where data.toml has it alike, what data.lock pins.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Held {
    pub(crate) ours: Vec<String>,
    pub(crate) theirs: Vec<String>,
}

/// which version of one dataset a merge keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Take {
    /// both sides have it alike, or both lack it.
    Same,
    /// one side's version, which may be no dataset at all.
    Side(Side),
}

/// why one dataset needs a choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Why {
    /// both sides changed it since the merge base, each its own way.
    BothChanged,
    /// one side changed it and the other removed it.
    ChangedAndRemoved,
    /// both sides added it, each its own way.
    BothAdded,
    /// the sides differ and no merge base shows which one changed it.
    NoBase,
    /// only one side has it, and no base shows whether that side added it or the other removed it.
    OneSided(Side),
}

/// what the merge base held of one dataset: nothing known, or its version there, none when the
/// base lacked it.
enum Base<'a, T> {
    Unknown,
    Known(Option<&'a T>),
}

impl<T> Clone for Base<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Base<'_, T> {}

/// which version of a dataset to keep, from what our side, their side and the merge base hold of
/// it, none for a side without it. `alike` says two versions are the same.
fn decide<T>(
    ours: Option<&T>,
    theirs: Option<&T>,
    base: Base<'_, T>,
    alike: impl Fn(&T, &T) -> bool,
) -> Result<Take, Why> {
    let same = |a: Option<&T>, b: Option<&T>| match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => alike(a, b),
        _ => false,
    };
    if same(ours, theirs) {
        return Ok(Take::Same);
    }
    let Base::Known(base) = base else {
        return Err(match (ours, theirs) {
            (Some(_), None) => Why::OneSided(Side::Ours),
            (None, Some(_)) => Why::OneSided(Side::Theirs),
            _ => Why::NoBase,
        });
    };
    if same(ours, base) {
        return Ok(Take::Side(Side::Theirs));
    }
    if same(theirs, base) {
        return Ok(Take::Side(Side::Ours));
    }
    Err(match (ours, theirs, base) {
        (Some(_), Some(_), None) => Why::BothAdded,
        (Some(_), Some(_), Some(_)) => Why::BothChanged,
        _ => Why::ChangedAndRemoved,
    })
}

/// what a merge has decided so far, kept apart for data.toml's entries and data.lock's records.
#[derive(Default)]
struct Decided {
    entries: BTreeMap<String, Take>,
    records: BTreeMap<String, Take>,
    conflicts: BTreeMap<String, Why>,
    chosen: BTreeSet<String>,
    warnings: Vec<String>,
}

impl Decided {
    /// the version `decided` keeps of `name`, or the side `choice` names where the sides
    /// disagree. none, with the conflict noted, when nothing settles it.
    fn settle(
        &mut self,
        name: &str,
        decided: Result<Take, Why>,
        choice: Option<Side>,
    ) -> Option<Take> {
        match (decided, choice) {
            (Err(_), Some(side)) => {
                self.chosen.insert(name.to_owned());
                Some(Take::Side(side))
            }
            (Ok(take), _) => Some(take),
            (Err(why), None) => {
                self.conflicts.entry(name.to_owned()).or_insert(why);
                None
            }
        }
    }
}

/// data.toml's text on each side of a merge: its sides, read.
struct Manifests<'a> {
    sides: &'a Sides,
    ours: Manifest,
    theirs: Manifest,
    /// each dataset's own text on each side.
    texts: [BTreeMap<String, String>; 2],
    /// the same at the merge base, when it reads.
    base: Option<BTreeMap<String, String>>,
    /// each side whole, as git holds it, with each dataset's own text, when git gives both sides
    /// and they read.
    whole: Option<[Whole<'a>; 2]>,
}

/// one side of data.toml whole, as its branch has it.
struct Whole<'a> {
    text: &'a str,
    manifest: Manifest,
    tables: BTreeMap<String, String>,
}

impl Manifests<'_> {
    fn of(&self, side: Side) -> &Manifest {
        match side {
            Side::Ours => &self.ours,
            Side::Theirs => &self.theirs,
        }
    }

    fn text(&self, side: Side) -> &str {
        match side {
            Side::Ours => &self.sides.ours,
            Side::Theirs => &self.sides.theirs,
        }
    }

    /// `side` whole, as its branch has it, when git gave it.
    fn whole(&self, side: Side) -> Option<&Whole<'_>> {
        let [ours, theirs] = self.whole.as_ref()?;
        Some(match side {
            Side::Ours => ours,
            Side::Theirs => theirs,
        })
    }
}

/// data.toml and data.lock merged from their sides, the lists data.lock keeps read below `root`,
/// or as git's `committed` gives a part that is not there, none without git; `lock` is none when
/// neither side has one. a dataset the base cannot settle and `choices` does not name is
/// `unconfirmed`, saying what each side holds of it; `told` gets each side's pin where that is
/// its data.toml entry, as `-v` shows. an unneeded choice is `usage`; an unreadable side, or text
/// outside the datasets both changed, `project.invalid`.
pub(crate) fn merge(
    manifest: &Sides,
    lock: Option<&Sides>,
    choices: &Choices,
    root: &Path,
    told: &dyn Fn(&str),
    committed: Option<Committed<'_>>,
) -> Result<Merged, Error> {
    let manifests = read_manifests(manifest)?;
    let frame = frame(manifest)?;
    let mut decided = Decided::default();
    decide_entries(&manifests, frame, choices, &mut decided);
    let text = merged_text(&manifests, frame, &decided.entries)?;
    let merged = read_merged(&text)?;
    check_merged(&manifests, &merged, &decided.entries)?;
    let memory = super::ROOM.get().copied().unwrap_or(u64::MAX);
    let locks = lock.map(|lock| read_locks(lock, memory)).transpose()?;
    let mut records = match &locks {
        Some(locks) => {
            let records = pick_records(locks, &merged, choices, &mut decided);
            decide_removals(&manifests, locks, &merged, choices, &mut decided)?;
            Some(records)
        }
        None => None,
    };
    check_choices(choices, &decided, &manifests)?;
    let (text, merged) = match &manifests.whole {
        Some(_) if !decided.chosen.is_empty() => {
            let text = chosen_whole(&manifests, choices, &decided, &text)?;
            let merged = read_merged(&text)?;
            check_chosen(&manifests, &merged, choices, &decided)?;
            if let (Some(locks), Some(records)) = (&locks, &mut records) {
                chosen_records(records, locks, &merged, choices, &decided);
            }
            (text, merged)
        }
        _ => (text, merged),
    };
    let mut summary = summarize(&decided, &merged, records.as_deref());
    if !decided.conflicts.is_empty() {
        for name in decided.conflicts.keys() {
            let held = match (entry_held(&manifests, name), &locks) {
                (Some(held), Some(locks)) => {
                    for line in pinned_on_each_side(locks, name) {
                        told(&line);
                    }
                    held
                }
                (Some(held), None) => held,
                (None, locks) => Held {
                    ours: pins_held(locks.as_ref(), name, Side::Ours),
                    theirs: pins_held(locks.as_ref(), name, Side::Theirs),
                },
            };
            summary.sides.insert(name.clone(), held);
        }
        let shown = manifest.shows_base || lock.is_some_and(|lock| lock.shows_base);
        return Err(undecided(&decided.conflicts, &summary, shown, root));
    }
    let repinned = match (&locks, &records) {
        (Some(locks), Some(records)) => repinned(&locks.ours, records),
        _ => Vec::new(),
    };
    let lock = match (records, &locks) {
        (Some(records), Some(locks)) => {
            Some(kept(records, locks, &decided, root, committed, memory)?)
        }
        _ => None,
    };
    Ok(Merged {
        manifest: text,
        lock,
        summary,
        repinned,
        warnings: decided.warnings,
    })
}

/// the merged data.toml, read.
fn read_merged(text: &str) -> Result<Manifest, Error> {
    Manifest::parse(text).map_err(|error| {
        Error::new(
            Kind::ProjectInvalid,
            format!("the merged data.toml would not read: {}", error.message),
        )
        .help(format!(
            "merge data.toml by hand, then run `{}`",
            crate::suggested::command("lock")
        ))
    })
}

/// decides which side's data.toml entry each dataset keeps. values alike on both sides but written
/// apart, as with other comments, keep `frame`'s text.
fn decide_entries(
    manifests: &Manifests<'_>,
    frame: Side,
    choices: &Choices,
    decided: &mut Decided,
) {
    let [ours, theirs] = &manifests.texts;
    let names: BTreeSet<&String> = ours.keys().chain(theirs.keys()).collect();
    for name in names {
        let base = manifests
            .base
            .as_ref()
            .map_or(Base::Unknown, |base| Base::Known(base.get(name)));
        let mut verdict = decide(ours.get(name), theirs.get(name), base, |a, b| a == b);
        let alike = match (
            manifests.ours.datasets.get(name),
            manifests.theirs.datasets.get(name),
        ) {
            (Some(ours), Some(theirs)) => values(ours) == values(theirs),
            _ => false,
        };
        if verdict.is_err() && alike && !choices.contains_key(name) {
            decided.warnings.push(format!(
                "kept datasets.{name} as {} side writes it in data.toml; {} side differs only in comments or spacing",
                frame.whose(),
                frame.other().whose()
            ));
            verdict = Ok(Take::Same);
        }
        if let Some(take) = decided.settle(name, verdict, choices.get(name).copied()) {
            decided.entries.insert(name.clone(), take);
        }
    }
}

/// refuses a choice no conflict needed, so a mistyped name is not taken for a decision.
fn check_choices(
    choices: &Choices,
    decided: &Decided,
    manifests: &Manifests<'_>,
) -> Result<(), Error> {
    let Some((name, side)) = choices
        .iter()
        .find(|(name, _)| !decided.chosen.contains(*name))
    else {
        return Ok(());
    };
    let known = manifests.ours.datasets.contains_key(name)
        || manifests.theirs.datasets.contains_key(name)
        || decided.records.contains_key(name);
    let message = if known {
        format!("{name} needs no choice in this merge")
    } else {
        format!("{name} is on neither side of the merge")
    };
    Err(Error::new(Kind::Usage, message).help(format!("leave out --{} {name}", side.word())))
}

/// how each dataset came out; `records` is none when neither side has a data.lock.
fn summarize(decided: &Decided, merged: &Manifest, records: Option<&[Parsed]>) -> Summary {
    let mut summary = Summary::default();
    let names: BTreeSet<&String> = decided
        .entries
        .keys()
        .chain(decided.records.keys())
        .filter(|name| !decided.conflicts.contains_key(*name))
        .collect();
    let pinned = |name: &str| records.is_some_and(|records| find(records, name).is_some());
    for name in names {
        let parts = [decided.entries.get(name), decided.records.get(name)];
        let from = |side: Side| parts.contains(&Some(&Take::Side(side)));
        let list = if !merged.datasets.contains_key(name) && !pinned(name) {
            &mut summary.removed
        } else if from(Side::Theirs) {
            &mut summary.from_theirs
        } else if from(Side::Ours) {
            &mut summary.from_ours
        } else {
            &mut summary.same
        };
        list.push(name.clone());
    }
    summary.unpinned = merged
        .datasets
        .keys()
        .filter(|name| !pinned(name) && !decided.conflicts.contains_key(*name))
        .cloned()
        .collect();
    summary.conflicts = decided.conflicts.keys().cloned().collect();
    summary
}

/// the datasets `records` pin other than `ours`, our side's data.lock, did.
fn repinned(ours: &[Parsed], records: &[Parsed]) -> Vec<String> {
    let names: BTreeSet<&str> = ours.iter().chain(records).map(Parsed::name).collect();
    names
        .into_iter()
        .filter(|name| match (find(ours, name), find(records, name)) {
            (Some(before), Some(after)) => !alike(before, after),
            (before, after) => before.is_some() != after.is_some(),
        })
        .map(str::to_owned)
        .collect()
}

/// each side of data.toml, read, with each dataset's own text. a side that does not read stops
/// the merge; a base that does not read is no base.
fn read_manifests<'a>(sides: &'a Sides) -> Result<Manifests<'a>, Error> {
    let side = |text: &str, side: Side| {
        Manifest::parse(text)
            .and_then(|read| Ok((read, manifest::tables(text)?)))
            .map_err(|error| unreadable(manifest::FILE, side, &error))
    };
    let (ours, ours_texts) = side(&sides.ours, Side::Ours)?;
    let (theirs, theirs_texts) = side(&sides.theirs, Side::Theirs)?;
    let base = sides
        .base
        .as_deref()
        .filter(|base| Manifest::parse(base).is_ok())
        .and_then(|base| manifest::tables(base).ok());
    let whole = |text: &'a str| {
        Some(Whole {
            text,
            manifest: Manifest::parse(text).ok()?,
            tables: manifest::tables(text).ok()?,
        })
    };
    let whole = sides
        .whole
        .as_ref()
        .and_then(|[ours, theirs]| Some([whole(ours)?, whole(theirs)?]));
    Ok(Manifests {
        sides,
        ours,
        theirs,
        texts: [ours_texts, theirs_texts],
        base,
        whole,
    })
}

/// the side whose text outside the datasets the merge keeps, `[project]` and notes among it: the
/// side that changed it since the base, or ours when they agree.
fn frame(sides: &Sides) -> Result<Side, Error> {
    let ours = manifest::outside(&sides.ours)?;
    let theirs = manifest::outside(&sides.theirs)?;
    if ours == theirs {
        return Ok(Side::Ours);
    }
    let base = sides
        .base
        .as_deref()
        .and_then(|base| manifest::outside(base).ok());
    match base {
        Some(base) if base == theirs => Ok(Side::Ours),
        Some(base) if base == ours => Ok(Side::Theirs),
        _ => Err(Error::new(
            Kind::ProjectInvalid,
            "data.toml differs outside its datasets on each side of the merge, as in [project] or a comment",
        )
        .help(format!("in data.toml, keep one side's lines of that part and delete the other side's and git's conflict lines by hand, then run `{}`", crate::suggested::command("lock")))),
    }
}

/// the `frame` side's text, with each dataset taken from the other side copied in or taken out.
fn merged_text(
    manifests: &Manifests<'_>,
    frame: Side,
    takes: &BTreeMap<String, Take>,
) -> Result<String, Error> {
    let other = frame.other();
    let edits: Vec<(&str, Edit)> = takes
        .iter()
        .filter(|(_, take)| **take == Take::Side(other))
        .map(|(name, _)| {
            let edit = if manifests.of(other).datasets.contains_key(name) {
                Edit::From(0)
            } else {
                Edit::Out
            };
            (name.as_str(), edit)
        })
        .collect();
    manifest::with_datasets_from(manifests.text(frame), &[manifests.text(other)], &edits)
}

/// what decides a dataset's files and where they go, without the lines that cite them.
fn values(entry: &Entry) -> impl PartialEq {
    let patterns = |globs: &[crate::glob::Glob]| {
        globs
            .iter()
            .map(|glob| glob.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    let source = &entry.source;
    (
        source.reference.clone(),
        patterns(&source.select),
        patterns(&source.exclude),
        source.extract,
        source.decompress,
        entry.copy,
        entry.path.clone(),
    )
}

/// refuses a merged data.toml that does not say what the merge decided, which an unusual layout
/// could make, rather than write it.
fn check_merged(
    manifests: &Manifests<'_>,
    merged: &Manifest,
    takes: &BTreeMap<String, Take>,
) -> Result<(), Error> {
    for (name, take) in takes {
        let side = match take {
            Take::Side(side) => *side,
            Take::Same => Side::Ours,
        };
        let agrees = match (
            manifests.of(side).datasets.get(name),
            merged.datasets.get(name),
        ) {
            (None, None) => true,
            (Some(expected), Some(wrote)) => values(expected) == values(wrote),
            _ => false,
        };
        if !agrees {
            return Err(unmergeable());
        }
    }
    Ok(())
}

fn unmergeable() -> Error {
    Error::new(
        Kind::ProjectInvalid,
        "fetchloom could not write the merged data.toml in its layout",
    )
    .help(format!(
        "merge data.toml by hand, then run `{}`",
        crate::suggested::command("lock")
    ))
}

/// data.lock's datasets on each side, and at the base when it reads.
struct Locks {
    ours: Vec<Parsed>,
    theirs: Vec<Parsed>,
    base: Option<Vec<Parsed>>,
    /// whether each side is whole, as git holds it, rather than read from the conflict lines.
    whole: bool,
}

impl Locks {
    fn of(&self, side: Side) -> &[Parsed] {
        match side {
            Side::Ours => &self.ours,
            Side::Theirs => &self.theirs,
        }
    }
}

/// data.lock's sides: whole as git holds them when it gave them, so no pin mixes the lines of
/// both, else as the conflict lines give them. the merge holds all three at once, so their texts
/// together must fit `room`, the memory a container leaves, before any is parsed, and their files
/// together before any list is read.
fn read_locks(sides: &Sides, room: u64) -> Result<Locks, Error> {
    let side =
        |text: &str, side: Side| parse(text).map_err(|error| unreadable(super::FILE, side, &error));
    let [ours, theirs] = sides
        .whole
        .as_ref()
        .map_or([&sides.ours, &sides.theirs], |[ours, theirs]| {
            [ours, theirs]
        });
    let bytes = [
        ours.len(),
        theirs.len(),
        sides.base.as_ref().map_or(0, String::len),
    ]
    .into_iter()
    .map(|len| len as u64)
    .fold(0, u64::saturating_add);
    super::fits_text(
        &format!(
            "data.lock's sides, which are {} together,",
            crate::units::size(bytes)
        ),
        bytes,
        room,
    )?;
    let locks = Locks {
        ours: side(ours, Side::Ours)?,
        theirs: side(theirs, Side::Theirs)?,
        base: sides.base.as_deref().and_then(|base| parse(base).ok()),
        whole: sides.whole.is_some(),
    };
    super::fits(
        locks
            .ours
            .iter()
            .chain(&locks.theirs)
            .chain(locks.base.iter().flatten()),
        room,
    )?;
    Ok(locks)
}

/// the data.lock entries kept for `merged`, sorted by name: each answers its dataset's spec, and is
/// the chosen side's when its entry was chosen.
fn pick_records(
    locks: &Locks,
    merged: &Manifest,
    choices: &Choices,
    decided: &mut Decided,
) -> Vec<Parsed> {
    let mut kept = Vec::new();
    for (name, entry) in &merged.datasets {
        let wanted = spec(&entry.source);
        let (ours, theirs, base) = (
            answering(&locks.ours, name, &wanted),
            answering(&locks.theirs, name, &wanted),
            base_of(locks, name, find),
        );
        let verdict = match (ours, theirs) {
            (Some(_), None) => Ok(Take::Side(Side::Ours)),
            (None, Some(_)) => Ok(Take::Side(Side::Theirs)),
            _ => decide(ours, theirs, base, alike),
        };
        let choice = choices.get(name).copied();
        // a side chosen for data.toml brings its own pin
        let take = match choice {
            Some(side) if decided.chosen.contains(name) => Some(Take::Side(side)),
            _ => decided.settle(name, verdict, choice),
        };
        if let Some(take) = take {
            kept.extend(pick(take, ours, theirs).cloned());
            decided.records.insert(name.clone(), take);
        }
    }
    kept.sort_by(|a, b| a.name().cmp(b.name()));
    kept
}

/// decides each dataset a side of data.lock pins that the merged data.toml lacks. its pin goes
/// with its entry, unless a side changed or added the pin, or no base tells: then it waits for a
/// choice rather than drop a change unseen. choosing the pinning side needs git to give it whole.
fn decide_removals(
    manifests: &Manifests<'_>,
    locks: &Locks,
    merged: &Manifest,
    choices: &Choices,
    decided: &mut Decided,
) -> Result<(), Error> {
    let names: BTreeSet<&str> = locks
        .ours
        .iter()
        .chain(&locks.theirs)
        .map(Parsed::name)
        .collect();
    for name in names {
        if merged.datasets.contains_key(name) || decided.conflicts.contains_key(name) {
            continue;
        }
        let (ours, theirs) = (find(&locks.ours, name), find(&locks.theirs, name));
        let verdict = match decide(ours, theirs, base_of(locks, name, find), alike) {
            Ok(Take::Same) => continue,
            Ok(Take::Side(side)) if pick(Take::Side(side), ours, theirs).is_some() => {
                Err(Why::ChangedAndRemoved)
            }
            verdict => verdict,
        };
        let Some(take) = decided.settle(name, verdict, choices.get(name).copied()) else {
            continue;
        };
        if let Take::Side(side) = take
            && pick(take, ours, theirs).is_some()
            && manifests
                .whole(side)
                .is_none_or(|whole| !whole.manifest.datasets.contains_key(name))
        {
            return Err(entry_gone(name, side));
        }
        decided.records.insert(name.to_owned(), take);
    }
    Ok(())
}

/// the error for choosing `side` for `name`, whose entry neither conflict lines nor git give back.
fn entry_gone(name: &str, side: Side) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!(
            "{name} is not in the merged data.toml, and fetchloom cannot read its entry on {} side",
            side.whose()
        ),
    )
    .detail("git on PATH gives fetchloom each side of data.toml whole while the merge is under way")
    .help(format!(
        "copy [datasets.{name}] from {} side's data.toml into data.toml by hand, then run `{}`",
        side.whose(),
        crate::suggested::command("lock")
    ))
}

/// `text`, the merged data.toml, with each chosen dataset as its side's branch writes it, whole,
/// not with lines git merged in from the other side, nor removed by it.
fn chosen_whole(
    manifests: &Manifests<'_>,
    choices: &Choices,
    decided: &Decided,
    text: &str,
) -> Result<String, Error> {
    let now = manifest::tables(text)?;
    let sides = [Side::Ours, Side::Theirs];
    let sources = sides.map(|side| manifests.whole(side).map_or("", |whole| whole.text));
    let mut edits = Vec::new();
    for name in &decided.chosen {
        let Some(side) = choices.get(name) else {
            continue;
        };
        let Some(whole) = manifests.whole(*side) else {
            continue;
        };
        let source = usize::from(*side == Side::Theirs);
        let edit = match (now.get(name), whole.tables.get(name)) {
            (now, Some(wanted)) if now != Some(wanted) => Edit::From(source),
            (Some(_), None) => Edit::Out,
            _ => continue,
        };
        edits.push((name.as_str(), edit));
    }
    manifest::with_datasets_from(text, &sources, &edits)
}

/// refuses a merged data.toml that does not hold a chosen side's entry as that side's branch
/// writes it, as [`check_merged`] refuses one that does not say what the merge decided.
fn check_chosen(
    manifests: &Manifests<'_>,
    merged: &Manifest,
    choices: &Choices,
    decided: &Decided,
) -> Result<(), Error> {
    for name in &decided.chosen {
        let Some(whole) = choices.get(name).and_then(|side| manifests.whole(*side)) else {
            continue;
        };
        let agrees = match (whole.manifest.datasets.get(name), merged.datasets.get(name)) {
            (None, None) => true,
            (Some(expected), Some(wrote)) => values(expected) == values(wrote),
            _ => false,
        };
        if !agrees {
            return Err(unmergeable());
        }
    }
    Ok(())
}

/// `records` with each chosen dataset's pin as its side has it, when it answers `merged`'s entry.
fn chosen_records(
    records: &mut Vec<Parsed>,
    locks: &Locks,
    merged: &Manifest,
    choices: &Choices,
    decided: &Decided,
) {
    for name in &decided.chosen {
        let Some(side) = choices.get(name) else {
            continue;
        };
        let pins = locks.of(*side);
        let pin = merged
            .datasets
            .get(name)
            .and_then(|entry| answering(pins, name, &spec(&entry.source)));
        records.retain(|record| record.name() != name);
        records.extend(pin.cloned());
    }
    records.sort_by(|a, b| a.name().cmp(b.name()));
}

/// the merged data.lock of `records`, each list read below `root`, or from the commit of the side
/// it comes from, as `committed` gives it, where git's merge deleted it: a repin on one side that
/// replaced a list deletes it, though the other side still names it. a pin whose list neither
/// has waits for it, as any read leaves it. a pin that does not read stops the merge with how to
/// restore its side, never with advice to pin anew. the lists together are read within `memory`,
/// what a container leaves.
fn kept(
    records: Vec<Parsed>,
    locks: &Locks,
    decided: &Decided,
    root: &Path,
    committed: Option<Committed<'_>>,
    memory: u64,
) -> Result<Lock, Error> {
    let mut budget = super::Budget::of(&records, memory)?;
    let mut lock = Lock::default();
    for record in records {
        let name = record.name().to_owned();
        let side = match decided.records.get(&name) {
            Some(Take::Side(side)) => *side,
            _ => Side::Ours,
        };
        // the other side wrote these names, so only fetchloom's own are looked for or brought back
        let parts = match &record {
            Parsed::Source(_, Some(listed)) if super::lists::named_as_written(&name, listed) => {
                listed.parts.clone()
            }
            _ => Vec::new(),
        };
        let gone = parts.iter().any(|part| !below(root, part).is_file());
        let found = if gone {
            from_commit(&name, side, &parts, root, committed)?
        } else {
            None
        };
        // neither this folder nor the side's commit holds the list, as in a clone of a commit that
        // left data.lock.d out: the pin waits for it, written back as read
        let record = match super::missing_part(&record, root).filter(|_| found.is_none()) {
            Some(part) => match super::Waiting::of(record, part, root) {
                Ok(waiting) => {
                    super::check_resolved(waiting.pin())?;
                    lock.waiting.push(waiting);
                    continue;
                }
                Err(record) => *record,
            },
            None => record,
        };
        let read = super::resolve_from(
            vec![record],
            Some(super::lists::Parts::Folder(
                found.as_ref().map_or(root, |found| found.path()),
            )),
            &mut budget,
        )
        .map_err(|error| {
            let broken = [super::recovery(), super::lists::recovery()];
            if error
                .help
                .as_deref()
                .is_some_and(|help| broken.iter().any(|broken| broken == help))
            {
                unreadable_pin(&name, side, locks.whole, &error)
            } else {
                error
            }
        })?;
        lock.datasets.extend(read.datasets);
        lock.stated.extend(read.stated);
    }
    lock.waiting.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(lock)
}

/// a folder holding the parts of `name`'s list, `parts`: each as it is below `root`, or as `side`'s
/// commit holds it where it is not there. a part's name holds its contents' hash and reading checks
/// it against data.lock, so a part from git is the one data.lock names. none when the commit lacks
/// one too; without git to ask, the error names the command that brings it back. refused, as
/// reading a list is, when data.lock.d is a link or a file.
fn from_commit(
    name: &str,
    side: Side,
    parts: &[String],
    root: &Path,
    committed: Option<Committed<'_>>,
) -> Result<Option<tempfile::TempDir>, Error> {
    super::lists::folder(root)?;
    let Some(committed) = committed else {
        let gone = parts.iter().find(|part| !below(root, part).is_file());
        return Err(list_gone(name, side, gone.map_or("", String::as_str), root));
    };
    let found = tempfile::tempdir().map_err(|err| {
        Error::io(
            format_args!("could not make a folder for {name}'s list"),
            &err,
        )
    })?;
    for part in parts {
        let bytes = match read_part(&below(root, part)) {
            Ok(bytes) => bytes,
            Err(_) => match committed(side, part) {
                Some(bytes) => bytes,
                None => return Ok(None),
            },
        };
        let path = below(found.path(), part);
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, bytes));
        written.map_err(|err| Error::io(format_args!("could not keep {part} for {name}"), &err))?;
    }
    Ok(Some(found))
}

/// the most bytes of a list part read whole, which is the most git gives for one too.
const PART_MOST: u64 = 256 << 20;

/// the bytes of the part at `path`, a plain file read without following a link, up to
/// [`PART_MOST`]: past that, its hash, checked later, cannot match.
fn read_part(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    crate::volume::read_unfollowed(path)?
        .take(PART_MOST)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// `part`, a path from the project with `/` between folders, below `folder`.
fn below(folder: &Path, part: &str) -> std::path::PathBuf {
    folder.join(part.replace('/', std::path::MAIN_SEPARATOR_STR))
}

/// the error for `part` of `name`'s list on `side`, which is not in data.lock.d below `root`,
/// without git to read it from: the command that brings it back from that side's commit.
fn list_gone(name: &str, side: Side, part: &str, root: &Path) -> Error {
    let checkout =
        |commit: &str| crate::suggested::git(root, &format!("checkout {commit} -- {part}"));
    let help = match side {
        Side::Ours => format!(
            "bring it back from our side's commit with `{}`, then run the same command again",
            checkout("HEAD")
        ),
        Side::Theirs => format!(
            "bring it back from their side's commit with `{}`, or `{}` in a rebase, then run the same command again",
            checkout("MERGE_HEAD"),
            checkout("REBASE_HEAD")
        ),
    };
    Error::new(
        Kind::ProjectInvalid,
        format!(
            "{name} on {} side of data.lock names {part}, which is not there",
            side.whose()
        ),
    )
    .detail(
        "a merge deletes a list one side's repin replaced, though the other side still names it",
    )
    .help(help)
}

/// the error for `name`'s pin on `side`, which did not read whole as `error` says. `whole` says
/// the side came whole from git rather than from the conflict lines.
fn unreadable_pin(name: &str, side: Side, whole: bool, error: &Error) -> Error {
    let mut unread = Error::new(
        Kind::ProjectInvalid,
        format!(
            "{name} on {} side of data.lock cannot be read whole",
            side.whose()
        ),
    )
    .detail(error.message.clone());
    for detail in &error.details {
        unread = unread.detail(detail.clone());
    }
    if !whole {
        unread = unread.detail(
            "this side comes from git's conflict lines, which can hold lines git merged from the other side; with git on PATH, fetchloom reads each side whole",
        );
    }
    unread.help(format!(
        "copy the whole [[dataset]] table of {name} from {} side's data.lock, and the files of data.lock.d it names, by hand, then run `{}`",
        side.whose(),
        crate::suggested::command("lock")
    ))
}

/// the version `take` keeps, of `ours` and `theirs`.
fn pick<'a, T>(take: Take, ours: Option<&'a T>, theirs: Option<&'a T>) -> Option<&'a T> {
    match take {
        Take::Same | Take::Side(Side::Ours) => ours,
        Take::Side(Side::Theirs) => theirs,
    }
}

/// what data.lock's merge base pins of `name`, as `find` finds it.
fn base_of<'a>(
    locks: &'a Locks,
    name: &str,
    find: fn(&'a [Parsed], &str) -> Option<&'a Parsed>,
) -> Base<'a, Parsed> {
    locks
        .base
        .as_ref()
        .map_or(Base::Unknown, |base| Base::Known(find(base, name)))
}

/// the entry of `records`, sorted by name as both readers give them, named `name`.
fn find<'a>(records: &'a [Parsed], name: &str) -> Option<&'a Parsed> {
    records
        .binary_search_by(|record| record.name().cmp(name))
        .ok()
        .map(|at| &records[at])
}

/// `name`'s entry in `records` when it pins a fetched dataset for `wanted`, the spec it has now,
/// downloaded or not.
fn answering<'a>(records: &'a [Parsed], name: &str, wanted: &str) -> Option<&'a Parsed> {
    find(records, name).filter(|record| match record {
        Parsed::Source(locked, _) | Parsed::Stated(super::Stated { pin: locked, .. }) => {
            locked.spec == wanted
        }
    })
}

/// what each side holds of `name`, which needs a choice, where data.toml tells the sides apart:
/// each key of its entry that one side lacks or holds otherwise, as git's whole side writes it
/// where it can, and `not in data.toml` for a side without it. none where data.toml has it alike.
fn entry_held(manifests: &Manifests<'_>, name: &str) -> Option<Held> {
    let entry = |side: Side| {
        let text = match manifests.whole(side) {
            Some(whole) => whole.tables.get(name),
            None => manifests.texts[usize::from(side == Side::Theirs)].get(name),
        };
        text.map(|text| keys(text, name))
    };
    let held = match (entry(Side::Ours), entry(Side::Theirs)) {
        (Some(ours), Some(theirs)) => Held {
            ours: differing(&ours, &theirs),
            theirs: differing(&theirs, &ours),
        },
        (None, None) => return None,
        (ours, theirs) => {
            let lines = |entry: Option<toml::Table>| {
                entry.map_or_else(
                    || vec!["not in data.toml".to_owned()],
                    |entry| differing(&entry, &toml::Table::new()),
                )
            };
            Held {
                ours: lines(ours),
                theirs: lines(theirs),
            }
        }
    };
    (!held.ours.is_empty() || !held.theirs.is_empty()).then_some(held)
}

/// the keys of `name`'s entry in `text`, its own text as [`manifest::tables`] gives it.
fn keys(text: &str, name: &str) -> toml::Table {
    let entry = toml::from_str::<toml::Table>(text)
        .ok()
        .and_then(|mut document| match document.remove("datasets")? {
            toml::Value::Table(mut datasets) => datasets.remove(name),
            _ => None,
        });
    match entry {
        Some(toml::Value::Table(entry)) => entry,
        _ => toml::Table::new(),
    }
}

/// each key `entry` holds otherwise than `other`, as data.toml writes it on one line, or as
/// `no <key>` where only `other` has it.
fn differing(entry: &toml::Table, other: &toml::Table) -> Vec<String> {
    let names: BTreeSet<&String> = entry.keys().chain(other.keys()).collect();
    names
        .into_iter()
        .filter(|key| entry.get(*key) != other.get(*key))
        .map(|key| match entry.get(key) {
            Some(value) => format!("{} = {}", written_key(key), masked(key, value)),
            None => format!("no {}", written_key(key)),
        })
        .collect()
}

/// `value` of `key` as [`written`] writes it, with what may hold a secret masked as data.toml's
/// citations leave it out: a `ref`'s login and query values, and each value of `env`.
fn masked(key: &str, value: &toml::Value) -> String {
    match (key, value) {
        ("ref", toml::Value::String(text)) => super::quoted(&crate::shown::mask_query(text)),
        ("env", toml::Value::Table(table)) if !table.is_empty() => {
            let names: Vec<String> = table
                .keys()
                .map(|name| format!("{} = ***", written_key(name)))
                .collect();
            format!("{{ {} }}", names.join(", "))
        }
        ("env", _) => "***".to_owned(),
        _ => written(value),
    }
}

/// `value` on one line, as data.toml writes it.
fn written(value: &toml::Value) -> String {
    match value {
        toml::Value::String(text) => super::quoted(text),
        toml::Value::Integer(number) => number.to_string(),
        toml::Value::Float(number) => number.to_string(),
        toml::Value::Boolean(yes) => yes.to_string(),
        toml::Value::Datetime(when) => when.to_string(),
        toml::Value::Array(values) => {
            let values: Vec<String> = values.iter().map(written).collect();
            format!("[{}]", values.join(", "))
        }
        toml::Value::Table(table) if table.is_empty() => "{}".to_owned(),
        toml::Value::Table(table) => {
            let pairs: Vec<String> = table
                .iter()
                .map(|(key, value)| format!("{} = {}", written_key(key), written(value)))
                .collect();
            format!("{{ {} }}", pairs.join(", "))
        }
    }
}

/// `key` as data.toml writes it: bare, or quoted when it holds more than letters, digits, `_`
/// and `-`.
fn written_key(key: &str) -> String {
    let bare = !key.is_empty()
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if bare {
        key.to_owned()
    } else {
        super::quoted(key)
    }
}

/// what `side`'s data.lock pins of `name`, as [`pinned`] says it.
fn pins_held(locks: Option<&Locks>, name: &str, side: Side) -> Vec<String> {
    locks.map_or_else(
        || vec!["not in data.lock".to_owned()],
        |locks| pinned(locks, name, side),
    )
}

/// the most files a side's pin of a dataset that needs a choice shows of those only it pins.
const SHOWN: usize = 3;

/// what each side pins for `name`, which needs a choice, as lines to compare, never saying which
/// to take: the files and bytes, and up to [`SHOWN`] files only that side pins.
fn pinned_on_each_side(locks: &Locks, name: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for side in [Side::Ours, Side::Theirs] {
        let mut pinned = pinned(locks, name, side).into_iter();
        if let Some(head) = pinned.next() {
            lines.push(format!("{name}, {}: {head}", side.word()));
        }
        lines.extend(pinned.map(|line| format!("  {line}")));
    }
    lines
}

/// what `side` pins for `name`, as [`pinned_on_each_side`] shows it, without the name: its files
/// and bytes first, then a line for each file only it pins, up to [`SHOWN`].
fn pinned(locks: &Locks, name: &str, side: Side) -> Vec<String> {
    let record = find(locks.of(side), name);
    let other = find(locks.of(side.other()), name);
    let (files, listed): (&[LockedFile], _) = match record {
        None => return vec!["not in data.lock".to_owned()],
        Some(Parsed::Source(locked, listed)) => (&locked.files, listed.as_ref()),
        Some(Parsed::Stated(stated)) => {
            return vec![format!(
                "{} \u{b7} {}, not downloaded yet",
                units::count(stated.files.len() as u64, "file", "files"),
                units::size(stated.bytes())
            )];
        }
    };
    let (count, bytes) = listed.map_or_else(
        || (files.len() as u64, files.iter().map(|file| file.size).sum()),
        |listed| (listed.count, listed.bytes),
    );
    let mut head = String::new();
    let _ = write!(
        head,
        "{} \u{b7} {}",
        units::count(count, "file", "files"),
        units::size(bytes)
    );
    // the other side wrote these names, so only fetchloom's own are shown
    match listed {
        Some(listed) if super::lists::named_as_written(name, listed) => {
            let _ = write!(head, ", listed in {}", listed.parts.join(", "));
        }
        Some(_) => head.push_str(", listed in files fetchloom does not write"),
        None => {}
    }
    let mut lines = vec![head];
    let others: &[LockedFile] = match other {
        Some(Parsed::Source(locked, None)) => &locked.files,
        _ => &[],
    };
    let only: Vec<&LockedFile> = files
        .iter()
        .filter(|file| {
            !others.iter().any(|other| {
                (&other.path, other.size, other.sha256) == (&file.path, file.size, file.sha256)
            })
        })
        .collect();
    for file in only.iter().take(SHOWN) {
        lines.push(format!(
            "{} \u{b7} {} \u{b7} sha256 {}",
            file.path,
            units::size(file.size),
            file.sha256.short()
        ));
    }
    if only.len() > SHOWN {
        lines.push(format!(
            "and {} only {} pins",
            units::count((only.len() - SHOWN) as u64, "more file", "more files"),
            side.word()
        ));
    }
    lines
}

/// whether two entries of data.lock pin the same thing, whenever each was pinned.
fn alike(a: &Parsed, b: &Parsed) -> bool {
    match (a, b) {
        (Parsed::Source(a, a_listed), Parsed::Source(b, b_listed)) => {
            a_listed == b_listed && untimed(a) == untimed(b)
        }
        (Parsed::Stated(a), Parsed::Stated(b)) => {
            a.files == b.files && untimed(&a.pin) == untimed(&b.pin)
        }
        _ => false,
    }
}

/// a fetched dataset's entry without when it was pinned. naming every field makes a new one
/// count here too.
fn untimed(locked: &Locked) -> impl PartialEq + '_ {
    let Locked {
        name,
        reference,
        spec,
        at,
        extract,
        decompress,
        web_page,
        resolved,
        title,
        license,
        doi,
        access,
        timestamp: _,
        files,
    } = locked;
    // tuples compare at most twelve fields, so the fields go in groups
    (
        (
            name, reference, spec, extract, decompress, web_page, resolved,
        ),
        (title, license, doi, access, files),
        at,
    )
}

/// the error for a side of `file` that does not read.
fn unreadable(file: &str, side: Side, error: &Error) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!(
            "{file} on {} side of the merge cannot be read",
            side.whose()
        ),
    )
    .detail(error.message.clone())
    .help(format!(
        "resolve the conflict in {file} by hand, then run `{}`",
        crate::suggested::command("lock")
    ))
}

/// whether `text` reads as `file`, data.toml or data.lock, as a merge base must to count.
pub(crate) fn reads(file: &str, text: &str) -> bool {
    if file == manifest::FILE {
        Manifest::parse(text).is_ok() && manifest::tables(text).is_ok()
    } else {
        parse(text).is_ok()
    }
}

/// the error naming each dataset in `conflicts` and why, with the merge so far, in the project at
/// `root`. `shown` says the conflict lines showed a base, so the help names no conflict style: one
/// is in use already.
fn undecided(
    conflicts: &BTreeMap<String, Why>,
    summary: &Summary,
    shown: bool,
    root: &Path,
) -> Error {
    let message = match conflicts.keys().collect::<Vec<_>>().as_slice() {
        [one] => format!("the merge needs a choice for {one}"),
        many => format!(
            "the merge needs a choice for {} datasets",
            units::grouped(many.len() as u64)
        ),
    };
    let mut error = Error::new(Kind::Unconfirmed, message);
    for (name, why) in conflicts {
        error = error.detail(match why {
            Why::BothChanged => format!("{name}: each side changed it its own way"),
            Why::ChangedAndRemoved => {
                format!("{name}: one side changed it and the other removed it")
            }
            Why::BothAdded => format!("{name}: each side added it its own way"),
            Why::NoBase => format!("{name}: it differs on each side"),
            Why::OneSided(side) => format!(
                "{name}: only {} has it, and fetchloom cannot tell whether {} added it or {} removed it",
                side.word(),
                side.word(),
                side.other().word()
            ),
        });
        if let Some(held) = summary.sides.get(name) {
            for (side, lines) in [(Side::Ours, &held.ours), (Side::Theirs, &held.theirs)] {
                if !lines.is_empty() {
                    let lines = lines.join("; ");
                    error = error.detail(format!("{name} on {}: {lines}", side.word()));
                }
            }
        }
    }
    if conflicts
        .values()
        .any(|why| matches!(why, Why::NoBase | Why::OneSided(_)))
    {
        error = error.detail(if shown {
            "no merge base fetchloom can read was found: git on PATH, before `git add` marks the files resolved, lets fetchloom see what each side changed".to_owned()
        } else {
            format!(
                "no merge base was found: git on PATH, before `git add` marks the files resolved, or `{}` before merging or rebasing, lets fetchloom see what each side changed",
                crate::suggested::git(root, "config merge.conflictStyle zdiff3")
            )
        });
    }
    let error = error.help(format!(
        "choose a side for each with `{}` or `{}`; both repeat",
        crate::suggested::command("lock --ours <name>"),
        crate::suggested::command("lock --theirs <name>")
    ));
    match serde_json::to_value(summary) {
        Ok(merge) => error.with_result(serde_json::json!({ "merge": merge })),
        Err(_) => error,
    }
}
