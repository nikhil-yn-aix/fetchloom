//! lineage: what a dataset was made from. data.toml's `from` names the datasets, and data.lock
//! keeps, with the pin of the dataset made from them, each one's tree identity ([`crate::tree`])
//! as it was when an add pinned it. it is a record a person keeps: nothing here runs a pipeline
//! or checks that the files came from those inputs.
//!
//! only an add records, for the datasets it names: `--from` replaces what data.toml says, and an
//! add without it keeps that and records the inputs anew, even when the dataset's own files are
//! the same. every other change of a pin keeps the record as it was. data.toml refuses a dataset
//! made from itself, directly or by way of others.
//!
//! status, `lock --check` and sync judge each input data.toml names for a pinned dataset, by what
//! data.toml, data.lock and this machine have of it now, against its record:
//!
//! | data.toml has it | data.lock pins it as asked | its files here | the record | state | said |
//! |---|---|---|---|---|---|
//! | no | | | known, and one other dataset's files are those | renamed | unchecked |
//! | no | | | any other | missing | unchecked |
//! | yes | no, or not downloaded or listed yet | | | unlocked | unchecked |
//! | yes | yes | unknown: an archive never unpacked here | | unresolved | unchecked |
//! | yes | yes | known | none, and no record of another name has those files | unresolved | unchecked |
//! | yes | yes | known | none, but a record of another name has those files | current | nothing |
//! | yes | yes | known | unknown | unresolved | unchecked |
//! | yes | yes | known | the same | current | nothing |
//! | yes | yes | known | other files | changed | stale |
//!
//! staleness is direct only: an input that is itself made from a changed dataset raises nothing
//! until its own files change. a stale dataset is told once per run, naming each input that
//! changed; status and `lock --check` also tell each input they cannot check, which sync leaves
//! to them. either way the exit code stays as it was.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::{Map, Value};

use crate::cache::Cache;
use crate::output::{Output, Warning};
use crate::project::lock::{Lock, Recorded, UNKNOWN};
use crate::project::manifest::{Entry, Manifest};
use crate::tree::{self, Identity};
use crate::units;

/// the identity of `name`'s files as data.toml and `lock` have them now, or none when this machine
/// cannot tell: data.toml has no such dataset, `lock` does not pin it as data.toml asks, or one of
/// its archives was never unpacked here.
pub(crate) fn current(
    manifest: &Manifest,
    lock: &Lock,
    name: &str,
    cache: &Cache,
) -> Option<Identity> {
    let entry = manifest.datasets.get(name)?;
    let locked = lock
        .get(name)
        .filter(|locked| locked.drift(entry).is_none())?;
    tree::effective(entry, locked, cache)
}

/// records, in `lock`, each of `names` as made from the datasets data.toml's `from` gives it, with
/// their files as `lock` pins them now; the names whose record changed, with it.
pub(crate) fn record(
    lock: &mut Lock,
    manifest: &Manifest,
    names: &[String],
    cache: &Cache,
) -> Vec<(String, Vec<Recorded>)> {
    let mut changed = Vec::new();
    for name in names {
        let Some(entry) = manifest.datasets.get(name) else {
            continue;
        };
        let mut inputs: Vec<Recorded> = entry
            .from
            .iter()
            .map(|input| Recorded {
                name: input.clone(),
                tree: current(manifest, lock, input, cache),
            })
            .collect();
        inputs.sort_by(|a, b| a.name.cmp(&b.name));
        let Some(locked) = lock.datasets.iter_mut().find(|locked| locked.name == *name) else {
            continue;
        };
        if locked.from != inputs {
            locked.from.clone_from(&inputs);
            changed.push((name.clone(), inputs));
        }
    }
    changed
}

/// what an add says of `name` once it recorded `inputs`: what it was made from, and whether status
/// can tell when their files change.
pub(crate) fn told_record(name: &str, inputs: &[Recorded]) -> String {
    let names: Vec<&str> = inputs.iter().map(|input| input.name.as_str()).collect();
    let unknown: Vec<&str> = inputs
        .iter()
        .filter(|input| input.tree.is_none())
        .map(|input| input.name.as_str())
        .collect();
    let made = format!("{name} as made from {}", units::listed(&names, "and"));
    match unknown.as_slice() {
        [] if names.len() == 1 => format!("{made}; status says when {}'s files change", names[0]),
        [] => format!("{made}; status says when their files change"),
        [one] => format!(
            "{made}; this machine does not know {one}'s files, so status cannot tell when they change"
        ),
        some => format!(
            "{made}; this machine does not know the files of {}, so status cannot tell when they change",
            units::listed(some, "and")
        ),
    }
}

/// how one input of a pinned dataset stands against what the dataset was made from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// its files are those the dataset was pinned from.
    Current,
    /// its files changed since the dataset was pinned from them, so the dataset may be stale.
    Changed,
    /// data.toml has no dataset of its name.
    Missing,
    /// data.toml has no dataset of its name, and only the dataset `to` has the files it had.
    Renamed { to: String },
    /// data.toml has it, and data.lock does not pin its files as data.toml asks yet.
    Unlocked,
    /// this machine does not know its files, as when an archive of it was never unpacked here.
    UnknownHere,
    /// data.lock has no record of it from when the dataset was pinned.
    Unrecorded,
    /// the record says its files were not known when the dataset was pinned.
    UnknownThen,
}

impl State {
    /// the reason `--json` gives, beside the warning's kind.
    fn reason(&self) -> &'static str {
        match self {
            State::Current => "current",
            State::Changed => "changed",
            State::Missing => "missing",
            State::Renamed { .. } => "renamed",
            State::Unlocked => "unlocked",
            State::UnknownHere | State::Unrecorded | State::UnknownThen => "unresolved",
        }
    }
}

/// one input of a pinned dataset as judged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Judged {
    pub(crate) dataset: String,
    pub(crate) input: String,
    pub(crate) state: State,
    /// the record's tree as data.lock writes it, or none when it has no record of the input.
    recorded: Option<String>,
    /// the input's tree now, `unknown` when this machine cannot tell, or none when data.toml or
    /// data.lock has no files of it.
    current: Option<String>,
    /// the add that pins the dataset again and records its inputs anew, with `--from` naming them
    /// all when one of them was renamed.
    again: String,
}

/// each input data.toml names for a dataset `lock` pins, judged as the table above says.
pub(crate) fn judge(manifest: &Manifest, lock: &Lock, cache: &Cache) -> Vec<Judged> {
    let mut judged = Vec::new();
    // each dataset's files now, found once, and only when a judgment asks
    let mut trees: BTreeMap<&str, Option<Identity>> = BTreeMap::new();
    let mut tree = |name: &str| -> Option<Identity> {
        if let Some(known) = trees.get(name) {
            return *known;
        }
        let found = current(manifest, lock, name, cache);
        if let Some((name, _)) = manifest.datasets.get_key_value(name) {
            trees.insert(name.as_str(), found);
        }
        found
    };
    for (dataset, entry) in manifest
        .datasets
        .iter()
        .filter(|(_, entry)| !entry.from.is_empty())
    {
        let Some(locked) = lock.get(dataset) else {
            continue;
        };
        for input in &entry.from {
            let record = locked.from.iter().find(|record| record.name == *input);
            let (state, now) = if !manifest.datasets.contains_key(input) {
                let then = record.and_then(|record| record.tree);
                (moved(manifest, dataset, input, then, &mut tree), None)
            } else if !pinned(manifest, lock, input) {
                (State::Unlocked, None)
            } else {
                match tree(input) {
                    None => (State::UnknownHere, Some(UNKNOWN.to_owned())),
                    Some(now) => {
                        let state = match record.map(|record| record.tree) {
                            Some(Some(then)) if then == now => State::Current,
                            Some(Some(_)) => State::Changed,
                            Some(None) => State::UnknownThen,
                            // a dataset renamed in data.toml, its from with it, kept its record
                            None if locked.from.iter().any(|other| {
                                other.tree == Some(now) && !entry.from.contains(&other.name)
                            }) =>
                            {
                                State::Current
                            }
                            None => State::Unrecorded,
                        };
                        (state, Some(now.to_string()))
                    }
                }
            };
            let (current, again) = match &state {
                State::Renamed { to } => (
                    tree(to).map(|tree| tree.to_string()),
                    again(dataset, entry, Some((input, to))),
                ),
                _ => (now, again(dataset, entry, None)),
            };
            judged.push(Judged {
                dataset: dataset.clone(),
                input: input.clone(),
                state,
                recorded: record.map(Recorded::tree_text),
                current,
                again,
            });
        }
    }
    judged
}

/// what the record says now of `input`, which data.toml no longer has: renamed when exactly one
/// other dataset than `dataset` has the files `then` records, else missing.
fn moved(
    manifest: &Manifest,
    dataset: &str,
    input: &str,
    then: Option<Identity>,
    tree: &mut dyn FnMut(&str) -> Option<Identity>,
) -> State {
    let Some(then) = then else {
        return State::Missing;
    };
    let mut holders = manifest
        .datasets
        .keys()
        .filter(|name| *name != dataset && *name != input)
        .filter(|name| tree(name) == Some(then));
    match (holders.next(), holders.next()) {
        (Some(to), None) => State::Renamed { to: to.clone() },
        _ => State::Missing,
    }
}

/// whether `lock` pins `name`'s files as data.toml asks.
fn pinned(manifest: &Manifest, lock: &Lock, name: &str) -> bool {
    let entry = &manifest.datasets[name];
    lock.get(name)
        .is_some_and(|locked| locked.drift(entry).is_none())
}

/// tells what `judged` found: for each dataset, once, the inputs whose files changed, and with
/// `unchecked`, each input it cannot check. the exit code stays as it was.
pub(crate) fn tell(output: &Output, judged: &[Judged], unchecked: bool) {
    let mut changed: BTreeMap<&str, Vec<&Judged>> = BTreeMap::new();
    for one in judged.iter().filter(|one| one.state == State::Changed) {
        changed.entry(one.dataset.as_str()).or_default().push(one);
    }
    for (dataset, inputs) in changed {
        let names: Vec<&str> = inputs.iter().map(|one| one.input.as_str()).collect();
        let facts = inputs.iter().map(|one| facts(one)).collect();
        output.warn_facts(
            Warning::DerivedStale,
            &stale(dataset, &names),
            &format!("rerun your pipeline, then `{}`", inputs[0].again),
            &[],
            facts,
        );
    }
    if !unchecked {
        return;
    }
    for one in judged {
        let (dataset, input) = (one.dataset.as_str(), one.input.as_str());
        let (why, help, details) = match &one.state {
            State::Current | State::Changed => continue,
            State::Missing => (
                format!("data.toml has no dataset {input}"),
                format!("add {input} back, or take it out of {dataset}'s from in data.toml"),
                Vec::new(),
            ),
            State::Renamed { to } => (
                format!("data.toml has no dataset {input}, though {to} has the files {input} had"),
                format!("record the new name with `{}`", one.again),
                vec![to.clone()],
            ),
            State::Unlocked => (
                format!("data.lock does not pin {input}'s files yet"),
                format!(
                    "`{}` pins and places them",
                    crate::suggested::command("sync")
                ),
                Vec::new(),
            ),
            State::UnknownHere => (
                format!("this machine does not know {input}'s files"),
                format!(
                    "`{}` brings them",
                    crate::suggested::command(&format!("sync {input}"))
                ),
                Vec::new(),
            ),
            State::Unrecorded => (
                format!(
                    "data.lock has no record of {input}'s files from when {dataset} was pinned"
                ),
                anew(one),
                Vec::new(),
            ),
            State::UnknownThen => (
                format!("{input}'s files were not known on the machine that pinned {dataset}"),
                anew(one),
                Vec::new(),
            ),
        };
        output.warn_facts(
            Warning::DerivedUnchecked,
            &format!("cannot tell whether {dataset} is stale: {why}"),
            &help,
            &details,
            vec![facts(one)],
        );
    }
}

/// what a stale `dataset` says of the inputs `names`, whose files changed.
fn stale(dataset: &str, names: &[&str]) -> String {
    let owned: Vec<String> = names.iter().map(|name| format!("{name}'s")).collect();
    format!(
        "{dataset} may be stale: {} files changed since {dataset} was pinned from them",
        units::listed(&owned, "and")
    )
}

/// the help that names the add that records `one`'s input as it is now.
fn anew(one: &Judged) -> String {
    format!(
        "if {} is made from {}'s files as they are now, `{}` records them",
        one.dataset, one.input, one.again
    )
}

/// the add that pins `dataset` again and records its inputs anew, with `--from` naming them all,
/// `renamed` the one of them that data.toml now has under another name.
fn again(dataset: &str, entry: &Entry, renamed: Option<(&str, &str)>) -> String {
    let what = if entry.is_import() {
        "<folder>"
    } else {
        "<link>"
    };
    let mut command = format!("add {what} --as {dataset}");
    if let Some((old, new)) = renamed {
        for input in &entry.from {
            let input = if input == old { new } else { input.as_str() };
            let _ = write!(command, " --from {input}");
        }
    }
    crate::suggested::command(&command)
}

/// the fields `--json` gives a warning about `one`, beside its kind.
fn facts(one: &Judged) -> Map<String, Value> {
    let text = |value: &Option<String>| value.clone().map_or(Value::Null, Value::String);
    let mut facts = Map::new();
    facts.insert("dataset".to_owned(), Value::String(one.dataset.clone()));
    facts.insert("input".to_owned(), Value::String(one.input.clone()));
    facts.insert("recorded".to_owned(), text(&one.recorded));
    facts.insert("current".to_owned(), text(&one.current));
    facts.insert(
        "reason".to_owned(),
        Value::String(one.state.reason().to_owned()),
    );
    facts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest;
    use crate::project::lock::{self, Locked, LockedFile};
    use crate::source::Access;

    /// data.toml with each dataset of `datasets`, a link and the `from` given, written as is.
    fn manifest(datasets: &[(&str, &str)]) -> Manifest {
        let mut text = String::new();
        for (name, from) in datasets {
            let _ = writeln!(
                text,
                "[datasets.{name}]\nref = \"https://example.org/{name}.zip\"\n{from}"
            );
        }
        Manifest::parse(&text).unwrap()
    }

    /// `name` pinned as `manifest` asks, to one file of `bytes`, or to an archive of them when
    /// `packed`, with `from` recorded.
    fn pin(
        manifest: &Manifest,
        name: &str,
        bytes: &[u8],
        packed: bool,
        from: Vec<Recorded>,
    ) -> Locked {
        let source = &manifest.datasets[name].source;
        Locked {
            name: name.to_owned(),
            reference: source.reference.clone(),
            spec: lock::spec(source),
            extract: source.extract,
            decompress: source.decompress,
            web_page: false,
            resolved: source.reference.clone(),
            title: None,
            license: None,
            doi: None,
            access: Access::Unknown,
            timestamp: "2026-11-17T00:00:00Z".to_owned(),
            from,
            files: vec![LockedFile {
                path: format!("{name}.csv"),
                size: bytes.len() as u64,
                sha256: digest::sha256(bytes),
                md5: None,
                url: String::new(),
                unpacks_to: packed.then(|| digest::sha256(b"never unpacked here")),
            }],
        }
    }

    fn lock(mut datasets: Vec<Locked>) -> Lock {
        datasets.sort_by(|a, b| a.name.cmp(&b.name));
        Lock {
            datasets,
            ..Lock::default()
        }
    }

    /// the identity of a dataset pinned to one file of `bytes`, by [`pin`].
    fn tree_of(name: &str, bytes: &[u8]) -> Identity {
        Identity::of([(
            format!("{name}.csv").as_str(),
            bytes.len() as u64,
            digest::sha256(bytes),
        )])
    }

    fn record(name: &str, tree: Option<Identity>) -> Recorded {
        Recorded {
            name: name.to_owned(),
            tree,
        }
    }

    /// the state `judge` gives the one input of `processed`, made from `iris`.
    fn state(manifest: &Manifest, lock: &Lock) -> Vec<State> {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        judge(manifest, lock, &cache)
            .into_iter()
            .map(|judged| judged.state)
            .collect()
    }

    #[test]
    fn each_input_is_judged_as_the_table_says() {
        let made = manifest(&[("processed", "from = [\"iris\"]"), ("iris", "")]);
        let then = Some(tree_of("iris", b"setosa"));
        let processed = |from| pin(&made, "processed", b"counts", false, from);
        let iris = |bytes: &[u8], packed| pin(&made, "iris", bytes, packed, Vec::new());
        let cases = [
            (
                "as pinned",
                vec![record("iris", then)],
                Some(iris(b"setosa", false)),
                State::Current,
            ),
            (
                "changed",
                vec![record("iris", then)],
                Some(iris(b"virginica", false)),
                State::Changed,
            ),
            (
                "not pinned",
                vec![record("iris", then)],
                None,
                State::Unlocked,
            ),
            (
                "never unpacked",
                vec![record("iris", then)],
                Some(iris(b"setosa", true)),
                State::UnknownHere,
            ),
            (
                "no record",
                Vec::new(),
                Some(iris(b"setosa", false)),
                State::Unrecorded,
            ),
            (
                "unknown then",
                vec![record("iris", None)],
                Some(iris(b"setosa", false)),
                State::UnknownThen,
            ),
            (
                "renamed with its from",
                vec![record("flowers", then)],
                Some(iris(b"setosa", false)),
                State::Current,
            ),
        ];
        for (case, from, iris, expected) in cases {
            let pins: Vec<Locked> = [Some(processed(from)), iris]
                .into_iter()
                .flatten()
                .collect();
            assert_eq!(state(&made, &lock(pins)), [expected], "{case}");
        }
        // a pin data.toml has moved past is no pin of what it asks
        let mut moved = iris(b"setosa", false);
        moved.spec = "sha256:another".to_owned();
        let pins = lock(vec![processed(vec![record("iris", then)]), moved]);
        assert_eq!(state(&made, &pins), [State::Unlocked]);
    }

    #[test]
    fn an_input_data_toml_no_longer_has_is_renamed_only_when_one_dataset_has_its_files() {
        let then = Some(tree_of("iris", b"setosa"));
        let made = manifest(&[("processed", "from = [\"iris\"]"), ("flowers", "")]);
        let flowers = |bytes: &[u8]| {
            let mut pinned = pin(&made, "flowers", bytes, false, Vec::new());
            pinned.files[0].path = "iris.csv".to_owned();
            pinned
        };
        let processed = |from| pin(&made, "processed", b"counts", false, from);
        let renamed = lock(vec![
            processed(vec![record("iris", then)]),
            flowers(b"setosa"),
        ]);
        assert_eq!(
            state(&made, &renamed),
            [State::Renamed {
                to: "flowers".to_owned()
            }]
        );
        let other = lock(vec![
            processed(vec![record("iris", then)]),
            flowers(b"virginica"),
        ]);
        assert_eq!(state(&made, &other), [State::Missing]);
        let unknown = lock(vec![
            processed(vec![record("iris", None)]),
            flowers(b"setosa"),
        ]);
        assert_eq!(state(&made, &unknown), [State::Missing]);
        let unrecorded = lock(vec![processed(Vec::new()), flowers(b"setosa")]);
        assert_eq!(state(&made, &unrecorded), [State::Missing]);
        let twice = manifest(&[
            ("processed", "from = [\"iris\"]"),
            ("flowers", ""),
            ("petals", ""),
        ]);
        let mut petals = pin(&twice, "petals", b"setosa", false, Vec::new());
        petals.files[0].path = "iris.csv".to_owned();
        let mut flowers = pin(&twice, "flowers", b"setosa", false, Vec::new());
        flowers.files[0].path = "iris.csv".to_owned();
        let both = lock(vec![
            pin(
                &twice,
                "processed",
                b"counts",
                false,
                vec![record("iris", then)],
            ),
            flowers,
            petals,
        ]);
        assert_eq!(state(&twice, &both), [State::Missing], "two may hold them");
    }

    #[test]
    fn only_a_pinned_dataset_with_a_from_is_judged() {
        let made = manifest(&[("processed", "from = [\"iris\"]"), ("iris", "")]);
        let iris = pin(&made, "iris", b"setosa", false, Vec::new());
        assert_eq!(state(&made, &lock(vec![iris])), []);
        let plain = manifest(&[("processed", ""), ("iris", "")]);
        let recorded = pin(
            &plain,
            "processed",
            b"counts",
            false,
            vec![record("iris", None)],
        );
        let iris = pin(&plain, "iris", b"virginica", false, Vec::new());
        assert_eq!(
            state(&plain, &lock(vec![recorded, iris])),
            [],
            "a record data.toml no longer names is kept, and judges nothing"
        );
    }
}
