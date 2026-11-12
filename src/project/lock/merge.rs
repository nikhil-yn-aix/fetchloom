//! `fetchloom lock` after a git merge it could not finish: data.toml and data.lock merged one
//! dataset at a time, as pnpm merges a lockfile both of whose sides read. a dataset only one side
//! changed takes that side, one both sides have alike stays, and one each side changed its own way
//! waits for a person to choose a side, as does a removal on one side of a dataset whose pin the
//! other side changed, even when git merged data.toml by itself. without a merge base, every
//! dataset the sides disagree on needs a choice, since an addition on one side looks like a
//! removal on the other. a choice takes its side whole, entry and pin, as git holds that side when
//! it can tell. nothing is resolved or downloaded: data.lock keeps a pin only when it answers the
//! merged data.toml.

pub(crate) mod markers;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use serde::Serialize;

use self::markers::Sides;
use super::derived::Derived;
use super::{Lock, Locked, LockedFile, Parsed, parse, resolve, spec};
use crate::error::{Error, Kind};
use crate::project::manifest::{self, Entry, Manifest};
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
    /// fetched datasets in the merged data.toml that no data.lock entry of either side answers,
    /// so `fetchloom lock` pins them.
    pub(crate) unpinned: Vec<String>,
    /// those that need a choice, in an error's result only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) conflicts: Vec<String>,
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
    /// only one side has it and no merge base shows whether that side added it or the other
    /// removed it.
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

/// what a merge decided so far: the version kept of each dataset in data.toml and in data.lock,
/// the conflicts nothing settled, and the datasets a choice settled.
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
    /// each dataset's own text on each side and at the base, when the base reads.
    texts: [BTreeMap<String, String>; 2],
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

/// data.toml and data.lock merged from their sides, `lock` none when neither side has a
/// data.lock, with the lists of the datasets data.lock keeps read from below `root`. a dataset the
/// sides disagree on without a base to settle it stops the merge, unless `choices` names a side
/// for it; what each side pins of it is `told` first, as `-v` shows it.
///
/// # Errors
///
/// `unconfirmed` naming each dataset that needs a choice, `usage` for a choice no dataset needs,
/// and `project.invalid` for a side that does not read or text outside the datasets that differs
/// on both sides.
pub(crate) fn merge(
    manifest: &Sides,
    lock: Option<&Sides>,
    choices: &Choices,
    root: &Path,
    told: &dyn Fn(&str),
) -> Result<Merged, Error> {
    let manifests = read_manifests(manifest)?;
    let frame = frame(manifest)?;
    let mut decided = Decided::default();
    decide_entries(&manifests, frame, choices, &mut decided);
    let text = merged_text(&manifests, frame, &decided.entries)?;
    let merged = read_merged(&text)?;
    check_merged(&manifests, &merged, &decided.entries)?;
    let locks = lock.map(read_locks).transpose()?;
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
            let text = chosen_whole(&manifests, choices, &decided, text)?;
            let merged = read_merged(&text)?;
            check_chosen(&manifests, &merged, choices, &decided)?;
            if let (Some(locks), Some(records)) = (&locks, &mut records) {
                chosen_records(records, locks, &merged, choices, &decided);
            }
            (text, merged)
        }
        _ => (text, merged),
    };
    let summary = summarize(&decided, &merged, records.as_deref());
    if !decided.conflicts.is_empty() {
        if let Some(locks) = &locks {
            for name in decided.conflicts.keys() {
                for line in pinned_on_each_side(locks, name) {
                    told(&line);
                }
            }
        }
        let shown = manifest.shows_base || lock.is_some_and(|lock| lock.shows_base);
        return Err(undecided(&decided.conflicts, &summary, shown));
    }
    let repinned = match (&locks, &records) {
        (Some(locks), Some(records)) => repinned(&locks.ours, records),
        _ => Vec::new(),
    };
    let lock = match (records, &locks) {
        (Some(records), Some(locks)) => Some(kept(records, locks, &decided, root)?),
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
        .help("merge data.toml by hand, then run `fetchloom lock`")
    })
}

/// decides which version of each dataset in data.toml the merge keeps. the same values written
/// apart on each side, as with other comments, keep `frame`'s text.
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

/// how each dataset came out, from what `decided` kept of data.toml and of data.lock's `records`,
/// none when neither side has a data.lock.
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
    // only `fetchloom build` pins a recipe, so the next sync or lock pins fetched datasets alone
    summary.unpinned = merged
        .datasets
        .iter()
        .filter(|(name, entry)| {
            entry.source().is_some() && !pinned(name) && !decided.conflicts.contains_key(*name)
        })
        .map(|(name, _)| name.clone())
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
        .help("in data.toml, keep one side's lines of that part and delete the other side's and git's conflict lines by hand, then run `fetchloom lock`")),
    }
}

/// data.toml's merged text: the `frame` side's text, with each dataset the merge takes from the
/// other side copied in or taken out.
fn merged_text(
    manifests: &Manifests<'_>,
    frame: Side,
    takes: &BTreeMap<String, Take>,
) -> Result<String, Error> {
    let other = frame.other();
    let mut text = manifests.text(frame).to_owned();
    for (name, take) in takes {
        if *take != Take::Side(other) {
            continue;
        }
        text = if manifests.of(other).datasets.contains_key(name) {
            manifest::with_dataset_from(&text, manifests.text(other), name)?
        } else {
            manifest::without_dataset(&text, name)?
        };
    }
    Ok(text)
}

/// what decides a dataset's files and where they go, without the lines that cite them.
fn values(entry: &Entry) -> impl PartialEq {
    let patterns = |globs: &[crate::glob::Glob]| {
        globs
            .iter()
            .map(|glob| glob.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    let source = entry.source().map(|source| {
        (
            source.reference.clone(),
            patterns(&source.select),
            patterns(&source.exclude),
            source.extract,
        )
    });
    let recipe = entry.recipe().map(|recipe| {
        let run: Vec<String> = recipe
            .run
            .iter()
            .map(|word| word.text().to_owned())
            .collect();
        (
            recipe.from.clone(),
            run,
            recipe.code.clone(),
            recipe.env.clone(),
        )
    });
    (source, recipe, entry.copy, entry.path.clone())
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
    .help("merge data.toml by hand, then run `fetchloom lock`")
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
/// both, else as the conflict lines give them.
fn read_locks(sides: &Sides) -> Result<Locks, Error> {
    let side =
        |text: &str, side: Side| parse(text).map_err(|error| unreadable(super::FILE, side, &error));
    let [ours, theirs] = sides
        .whole
        .as_ref()
        .map_or([&sides.ours, &sides.theirs], |[ours, theirs]| {
            [ours, theirs]
        });
    Ok(Locks {
        ours: side(ours, Side::Ours)?,
        theirs: side(theirs, Side::Theirs)?,
        base: sides.base.as_deref().and_then(|base| parse(base).ok()),
        whole: sides.whole.is_some(),
    })
}

/// the data.lock entries a merge keeps for `merged`, the merged data.toml, sorted by name: for a
/// fetched dataset, an entry that answers its spec, the chosen side's when a person chose one for
/// its entry in data.toml; for a recipe's result, the merge of both sides' entries.
fn pick_records(
    locks: &Locks,
    merged: &Manifest,
    choices: &Choices,
    decided: &mut Decided,
) -> Vec<Parsed> {
    let mut kept = Vec::new();
    for (name, entry) in &merged.datasets {
        let (ours, theirs, base) = match entry.source() {
            Some(source) => {
                let wanted = spec(source);
                (
                    answering(&locks.ours, name, &wanted),
                    answering(&locks.theirs, name, &wanted),
                    base_of(locks, name, find),
                )
            }
            None => (
                result(&locks.ours, name),
                result(&locks.theirs, name),
                base_of(locks, name, result),
            ),
        };
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

/// decides each dataset data.lock pins on a side that the merged data.toml no longer has. it goes
/// with its entry, unless its pin changed or came on one side since the base while the other
/// side removed it: that removal would drop the change unseen, so it waits for a person, as it
/// does without a base when only one side pins it. a choice of the side that pins it takes back
/// that side's entry whole, which needs git to give each side whole.
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

/// the error for a choice of `side` for `name`, whose entry neither the conflict lines nor git
/// can give back.
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
        "copy [datasets.{name}] from {} side's data.toml into data.toml by hand, then run `fetchloom lock`",
        side.whose()
    ))
}

/// `text`, the merged data.toml, with each dataset a person chose a side for as that side's
/// branch writes it, whole: a choice keeps one side of a dataset, not lines git merged into it
/// from the other, nor its removal by the other.
fn chosen_whole(
    manifests: &Manifests<'_>,
    choices: &Choices,
    decided: &Decided,
    mut text: String,
) -> Result<String, Error> {
    let now = manifest::tables(&text)?;
    for name in &decided.chosen {
        let Some(whole) = choices.get(name).and_then(|side| manifests.whole(*side)) else {
            continue;
        };
        text = match (now.get(name), whole.tables.get(name)) {
            (now, Some(wanted)) if now != Some(wanted) => {
                manifest::with_dataset_from(&text, whole.text, name)?
            }
            (Some(_), None) => manifest::without_dataset(&text, name)?,
            _ => continue,
        };
    }
    Ok(text)
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

/// `records` with the pin of each dataset a person chose a side for as that side has it, when
/// it answers the dataset's entry in `merged`.
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
            .and_then(|entry| match entry.source() {
                Some(source) => answering(pins, name, &spec(source)),
                None => result(pins, name),
            });
        records.retain(|record| record.name() != name);
        records.extend(pin.cloned());
    }
    records.sort_by(|a, b| a.name().cmp(b.name()));
}

/// the merged data.lock of `records`, the pins a merge keeps, with each list read below `root`.
/// a pin that does not read stops the merge with how to put back the side it came from, never
/// with advice to pin anew.
fn kept(
    records: Vec<Parsed>,
    locks: &Locks,
    decided: &Decided,
    root: &Path,
) -> Result<Lock, Error> {
    let mut lock = Lock::default();
    for record in records {
        let name = record.name().to_owned();
        let side = match decided.records.get(&name) {
            Some(Take::Side(side)) => *side,
            _ => Side::Ours,
        };
        let read = resolve(vec![record], Some(root)).map_err(|error| {
            let broken = [super::BROKEN, super::lists::BROKEN];
            if error
                .help
                .as_deref()
                .is_some_and(|help| broken.contains(&help))
            {
                unreadable_pin(&name, side, locks.whole, &error)
            } else {
                error
            }
        })?;
        lock.datasets.extend(read.datasets);
        lock.derived.extend(read.derived);
    }
    Ok(lock)
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
        "copy the whole [[dataset]] table of {name} from {} side's data.lock, and the files of data.lock.d it names, by hand, then run `fetchloom lock`",
        side.whose()
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

/// the entry of `records` that pins the fetched dataset `name` for `wanted`, the spec its entry in
/// data.toml has now.
fn answering<'a>(records: &'a [Parsed], name: &str, wanted: &str) -> Option<&'a Parsed> {
    find(records, name)
        .filter(|record| matches!(record, Parsed::Source(locked, _) if locked.spec == wanted))
}

/// the entry of `records` for `name`, a recipe's result.
fn result<'a>(records: &'a [Parsed], name: &str) -> Option<&'a Parsed> {
    find(records, name).filter(|record| matches!(record, Parsed::Derived(..)))
}

/// the most files a side's pin of a dataset that needs a choice shows of those only it pins.
const SHOWN: usize = 3;

/// what each side of a merge pins for `name`, a dataset that needs a choice, as lines a person
/// compares: a recipe result's key, how many files and bytes, and up to [`SHOWN`] of the files
/// only that side pins, with their sizes and hashes. it says nothing of which side to take.
fn pinned_on_each_side(locks: &Locks, name: &str) -> Vec<String> {
    let [ours, theirs] = [Side::Ours, Side::Theirs].map(|side| find(locks.of(side), name));
    let mut lines = Vec::new();
    for (side, record, other) in [(Side::Ours, ours, theirs), (Side::Theirs, theirs, ours)] {
        let side = side.word();
        let (key, files, listed): (_, &[LockedFile], _) = match record {
            None => {
                lines.push(format!("{name}, {side}: not in data.lock"));
                continue;
            }
            Some(Parsed::Source(locked, listed)) => (None, &locked.files, listed.as_ref()),
            Some(Parsed::Derived(derived, listed)) => (Some(derived.key), &[], Some(listed)),
        };
        let (count, bytes) = listed.map_or_else(
            || (files.len() as u64, files.iter().map(|file| file.size).sum()),
            |listed| (listed.count, listed.bytes),
        );
        let mut head = format!("{name}, {side}: ");
        if let Some(key) = key {
            let _ = write!(head, "key {}, ", key.short());
        }
        let _ = write!(
            head,
            "{} \u{b7} {}",
            units::count(count, "file", "files"),
            units::size(bytes)
        );
        if let Some(listed) = listed {
            let _ = write!(head, ", listed in {}", listed.parts.join(", "));
        }
        lines.push(head);
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
                "  {} \u{b7} {} \u{b7} sha256 {}",
                file.path,
                units::size(file.size),
                file.sha256.short()
            ));
        }
        if only.len() > SHOWN {
            lines.push(format!(
                "  and {} only {side} pins",
                units::count((only.len() - SHOWN) as u64, "more file", "more files")
            ));
        }
    }
    lines
}

/// whether two entries of data.lock pin the same thing, whenever each was pinned.
fn alike(a: &Parsed, b: &Parsed) -> bool {
    match (a, b) {
        (Parsed::Source(a, a_listed), Parsed::Source(b, b_listed)) => {
            a_listed == b_listed && untimed(a) == untimed(b)
        }
        (Parsed::Derived(a, a_listed), Parsed::Derived(b, b_listed)) => {
            a_listed == b_listed && untimed_result(a) == untimed_result(b)
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
        extract,
        resolved,
        title,
        license,
        doi,
        access,
        timestamp: _,
        files,
    } = locked;
    (
        name, reference, spec, extract, resolved, title, license, doi, access, files,
    )
}

/// a recipe result's entry without when it was made.
fn untimed_result(derived: &Derived) -> impl PartialEq + '_ {
    let Derived {
        name,
        from,
        run,
        code,
        env,
        key,
        timestamp: _,
        tree,
        files,
    } = derived;
    (name, from, run, code, env, key, tree, files)
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
        "resolve the conflict in {file} by hand, then run `fetchloom lock`"
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

/// the error that names each dataset in `conflicts` and why it needs a choice, with the merge as
/// far as it got. `shown` says git's conflict lines showed a base, so the conflict style that
/// shows one is in use already and the help names none.
fn undecided(conflicts: &BTreeMap<String, Why>, summary: &Summary, shown: bool) -> Error {
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
    }
    if conflicts
        .values()
        .any(|why| matches!(why, Why::NoBase | Why::OneSided(_)))
    {
        error = error.detail(if shown {
            "no merge base fetchloom can read was found: git on PATH, before `git add` marks the files resolved, lets fetchloom see what each side changed"
        } else {
            "no merge base was found: git on PATH, before `git add` marks the files resolved, or `git config merge.conflictStyle zdiff3` before merging or rebasing, lets fetchloom see what each side changed"
        });
    }
    let error = error.help(
        "choose a side for each with `fetchloom lock --ours <name>` or `fetchloom lock --theirs <name>`; both repeat",
    );
    match serde_json::to_value(summary) {
        Ok(merge) => error.with_result(serde_json::json!({ "merge": merge })),
        Err(_) => error,
    }
}
