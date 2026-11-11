//! `fetchloom lock` after a git merge it could not finish: data.toml and data.lock merged one
//! dataset at a time, as pnpm merges a lockfile both of whose sides read. a dataset only one side
//! changed takes that side, one both sides have alike stays, and one each side changed its own way
//! waits for a person to choose a side. without a merge base, every dataset the sides disagree on
//! needs a choice, since an addition on one side looks like a removal on the other. nothing is
//! resolved or downloaded: data.lock keeps a pin only when it answers the merged data.toml.

pub(crate) mod git;
pub(crate) mod markers;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use self::markers::Sides;
use super::derived::Derived;
use super::{Lock, Locked, Parsed, parse, resolve, spec};
use crate::error::{Error, Kind};
use crate::project::manifest::{self, Entry, Manifest};

/// one side of a merge, as git's `--ours` and `--theirs` name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    /// the branch checked out, before `=======` in a conflict.
    Ours,
    /// the branch merged in, after `=======`.
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
    /// in the merged data.toml, but pinned by no data.lock entry of either side that answers
    /// it, so `fetchloom lock` pins it.
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
}

/// data.toml and data.lock merged from their sides, `lock` none when neither side has a
/// data.lock, with the lists of the datasets data.lock keeps read from below `root`. a dataset the
/// sides disagree on without a base to settle it stops the merge, unless `choices` names a side
/// for it.
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
) -> Result<Merged, Error> {
    let manifests = read_manifests(manifest)?;
    let frame = frame(manifest)?;
    let mut decided = Decided::default();
    decide_entries(&manifests, frame, choices, &mut decided);
    let text = merged_text(&manifests, frame, &decided.entries)?;
    let merged = Manifest::parse(&text).map_err(|_| unmergeable())?;
    check_merged(&manifests, &merged, &decided.entries)?;
    let locks = lock.map(read_locks).transpose()?;
    let records = locks
        .as_ref()
        .map(|locks| pick_records(locks, &merged, choices, &mut decided));
    check_choices(choices, &decided, &manifests)?;
    let summary = summarize(&decided, &merged, records.as_deref());
    if !decided.conflicts.is_empty() {
        return Err(undecided(&decided.conflicts, &summary));
    }
    let repinned = match (&locks, &records) {
        (Some(locks), Some(records)) => repinned(&locks.ours, records),
        _ => Vec::new(),
    };
    let lock = records
        .map(|records| resolve(records, Some(root)))
        .transpose()?;
    Ok(Merged {
        manifest: text,
        lock,
        summary,
        repinned,
        warnings: decided.warnings,
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
fn read_manifests(sides: &Sides) -> Result<Manifests<'_>, Error> {
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
    Ok(Manifests {
        sides,
        ours,
        theirs,
        texts: [ours_texts, theirs_texts],
        base,
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
        .help("make that part of data.toml the same on both sides by hand, then run `fetchloom lock`")),
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
}

fn read_locks(sides: &Sides) -> Result<Locks, Error> {
    let side =
        |text: &str, side: Side| parse(text).map_err(|error| unreadable(super::FILE, side, &error));
    Ok(Locks {
        ours: side(&sides.ours, Side::Ours)?,
        theirs: side(&sides.theirs, Side::Theirs)?,
        base: sides.base.as_deref().and_then(|base| parse(base).ok()),
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

/// the error that names each dataset in `conflicts` and why it needs a choice, with the merge as
/// far as it got.
fn undecided(conflicts: &BTreeMap<String, Why>, summary: &Summary) -> Error {
    let message = match conflicts.keys().collect::<Vec<_>>().as_slice() {
        [one] => format!("the merge needs a choice for {one}"),
        many => format!("the merge needs a choice for {} datasets", many.len()),
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
        error = error.detail(
            "no merge base was found: git 2.45 or later, or `git config merge.conflictStyle zdiff3` before merging, lets fetchloom see what each side changed",
        );
    }
    let error = error.help(
        "choose a side for each with `fetchloom lock --ours <name>` or `fetchloom lock --theirs <name>`; both repeat",
    );
    match serde_json::to_value(summary) {
        Ok(merge) => error.with_result(serde_json::json!({ "merge": merge })),
        Err(_) => error,
    }
}
