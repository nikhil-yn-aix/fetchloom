//! `data.toml`: the datasets a project needs. people write it, and `add` and `remove` edit it in
//! place, keeping comments, order and spacing.

use std::collections::BTreeMap;
use std::ops::Range;

use toml_edit::{Array, Document, DocumentMut, Item, Key, Table, TableLike, TomlError, Value};

use crate::error::{Error, Kind};
use crate::glob::{self, Glob};
use crate::name;
use crate::recipe::{self, Slot, Template};

pub(crate) const FILE: &str = "data.toml";
/// the data folder of a project whose data.toml names none.
pub(crate) const DATA_DIR: &str = "data";
/// the `path` that puts a dataset's files straight in the data folder.
pub(crate) const DATA_FOLDER_ITSELF: &str = ".";
const TOP_KEYS: [&str; 2] = ["project", "datasets"];
const PROJECT_KEYS: [&str; 1] = ["data_dir"];
const DATASET_KEYS: [&str; 12] = [
    "ref", "select", "exclude", "extract", "members", "copy", "path", "place", "from", "run",
    "code", "env",
];
/// the keys of a dataset fetched from a source, beside `ref`.
const SOURCE_KEYS: [&str; 4] = ["select", "exclude", "extract", "members"];
/// the keys of the table that chooses the files of one archive.
const MEMBER_KEYS: [&str; 2] = ["select", "exclude"];
/// the keys of a dataset a recipe makes.
const RECIPE_KEYS: [&str; 4] = ["from", "run", "code", "env"];
/// how a person writes a recipe's command, for the help of a mistake in one.
const RUN_EXAMPLE: &str = "run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]";
/// what `code` is for, for the help of a path it refuses.
const OWN_FILES: &str = "name the project's own files, such as scripts and their settings";
/// the files fetchloom writes beside data.toml, which no recipe's key may read.
const WRITTEN: [&str; 2] = ["data.lock", "data.lock.d"];
/// words of a variable's name that mark a secret, which data.toml must not hold.
const SECRETS: [&str; 4] = ["SECRET", "PASSWORD", "PASSWD", "CREDENTIAL"];
/// pairs of words that name a key, such as `API_KEY`.
const KEYS: [&str; 3] = ["API", "ACCESS", "PRIVATE"];

#[derive(Clone, Debug)]
pub(crate) struct Manifest {
    /// where datasets land, relative to the project, with `/` between folders.
    pub(crate) data_dir: String,
    pub(crate) datasets: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    /// where the dataset comes from: a source it is fetched from, or a recipe that makes it.
    pub(crate) origin: Origin,
    pub(crate) copy: bool,
    /// where the dataset lands inside the data folder, when not under its name: a folder, or
    /// [`DATA_FOLDER_ITSELF`].
    pub(crate) path: Option<String>,
    /// whether its files are placed in the data folder. a dataset with `place = false` is
    /// pinned and cached for the recipes that read it, and has no folder.
    pub(crate) place: bool,
    /// each key's line in data.toml, as `data.toml:7   select = ["*.edf"]`, for messages.
    pub(crate) lines: BTreeMap<&'static str, String>,
}

/// where a dataset's files come from.
#[derive(Clone, Debug)]
pub(crate) enum Origin {
    Source(Source),
    Derived(Recipe),
}

/// a dataset fetched from a source.
#[derive(Clone, Debug)]
pub(crate) struct Source {
    pub(crate) reference: String,
    pub(crate) select: Vec<Glob>,
    pub(crate) exclude: Vec<Glob>,
    pub(crate) extract: bool,
    /// which files of each archive it unpacks the dataset keeps.
    pub(crate) members: Members,
}

/// what a dataset keeps of the archives it unpacks: a table for each archive under its path as
/// data.lock gives it, or under a glob key, as `"shard-*.tar"`, for every archive the key
/// matches.
#[derive(Clone, Debug, Default)]
pub(crate) struct Members {
    tables: BTreeMap<String, MemberFilter>,
    /// the keys with a wildcard, as patterns, in data.toml's order.
    globs: Vec<(String, Glob)>,
}

/// the keys whose tables an archive's path finds.
enum Found<'m> {
    None,
    One(&'m str),
    /// two glob keys, and no key of the archive's own.
    Two(&'m str, &'m str),
}

impl Members {
    pub(crate) fn new() -> Members {
        Members::default()
    }

    /// adds `filter` under `key`. a key with `*`, `?` or `[` that reads as a pattern is a glob;
    /// any other key names one archive.
    fn insert(&mut self, key: String, filter: MemberFilter) {
        if key.contains(['*', '?', '['])
            && let Ok(glob) = Glob::new(&key)
        {
            self.globs.push((key.clone(), glob));
        }
        self.tables.insert(key, filter);
    }

    /// every table with its key, in key order.
    pub(crate) fn iter(&self) -> std::collections::btree_map::Iter<'_, String, MemberFilter> {
        self.tables.iter()
    }

    fn found(&self, archive: &str) -> Found<'_> {
        if let Some((key, _)) = self.tables.get_key_value(archive) {
            return Found::One(key);
        }
        let mut globs = self
            .globs
            .iter()
            .filter(|(_, glob)| glob.matches(archive))
            .map(|(key, _)| key.as_str());
        match (globs.next(), globs.next()) {
            (None, _) => Found::None,
            (Some(key), None) => Found::One(key),
            (Some(first), Some(second)) => Found::Two(first, second),
        }
    }

    /// the table that chooses the files of the archive at `archive`, its path in the dataset
    /// `dataset` as data.lock gives it, with its key: the archive's own, which wins, or else the
    /// one glob key that matches the path. none when no key does.
    ///
    /// # Errors
    ///
    /// `project.invalid` naming both keys and the archive, when two glob keys match it and it
    /// has no table of its own.
    pub(crate) fn of(
        &self,
        dataset: &str,
        archive: &str,
    ) -> Result<Option<(&str, &MemberFilter)>, Error> {
        match self.found(archive) {
            Found::None => Ok(None),
            Found::One(key) => Ok(Some((key, &self.tables[key]))),
            Found::Two(first, second) => {
                let mut error = Error::new(
                    Kind::ProjectInvalid,
                    format!(
                        "members.\"{first}\" and members.\"{second}\" in datasets.{dataset} both match {archive}"
                    ),
                );
                for key in [first, second] {
                    if let Some(line) = self.tables[key].lines.get("members") {
                        error = error.detail(line.clone());
                    }
                }
                Err(error.help(format!(
                    "change one key so only one matches it, or give it a table of its own, as [datasets.{dataset}.members.\"{archive}\"], which wins over both"
                )))
            }
        }
    }

    /// the table [`Members::of`] finds for `archive`, for a reader that cannot refuse, as gc
    /// deciding what to keep: none when two glob keys match it, so every file of it counts.
    /// planning refuses that data.toml.
    pub(crate) fn get(&self, archive: &str) -> Option<&MemberFilter> {
        match self.found(archive) {
            Found::One(key) => Some(&self.tables[key]),
            Found::None | Found::Two(..) => None,
        }
    }

    /// the tables, in key order, whose keys neither name one of `archives` nor match one as a
    /// pattern.
    pub(crate) fn unmatched<'m>(
        &'m self,
        archives: &'m [&str],
    ) -> impl Iterator<Item = (&'m str, &'m MemberFilter)> {
        let matched = |key: &str| {
            archives.contains(&key)
                || self.globs.iter().any(|(glob_key, glob)| {
                    glob_key == key && archives.iter().any(|archive| glob.matches(archive))
                })
        };
        self.tables
            .iter()
            .filter(move |(key, _)| !matched(key))
            .map(|(key, filter)| (key.as_str(), filter))
    }
}

impl<'m> IntoIterator for &'m Members {
    type Item = (&'m String, &'m MemberFilter);
    type IntoIter = std::collections::btree_map::Iter<'m, String, MemberFilter>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// the files a dataset keeps of each archive its key covers, matched against the archive's own
/// paths, its top folder included, before placing strips anything. they change where files go,
/// not what data.lock pins, so they stay out of the dataset's spec.
#[derive(Clone, Debug)]
pub(crate) struct MemberFilter {
    pub(crate) select: glob::Set,
    pub(crate) exclude: glob::Set,
    /// the table's line in data.toml, as `members`, and each key's, for messages.
    pub(crate) lines: BTreeMap<&'static str, String>,
}

impl MemberFilter {
    /// whether the dataset keeps the file at `path` inside the archive.
    pub(crate) fn keeps(&self, path: &str) -> bool {
        (self.select.is_empty() || self.select.matches(path)) && !self.exclude.matches(path)
    }
}

/// a dataset made from other datasets by a command. only `fetchloom build` runs it.
#[derive(Clone, Debug)]
pub(crate) struct Recipe {
    /// the datasets it is made from, in data.toml's order.
    pub(crate) from: Vec<String>,
    /// the program and its arguments, never read by a shell.
    pub(crate) run: Vec<Template>,
    /// files and folders of the project whose bytes enter its key, with `/` between folders.
    pub(crate) code: Vec<String>,
    /// variables set for the command alone.
    pub(crate) env: BTreeMap<String, String>,
}

impl Entry {
    /// the source the dataset is fetched from, unless a recipe makes it.
    pub(crate) fn source(&self) -> Option<&Source> {
        match &self.origin {
            Origin::Source(source) => Some(source),
            Origin::Derived(_) => None,
        }
    }

    /// whether the dataset is an import: files from this machine, pinned by their bytes.
    pub(crate) fn is_import(&self) -> bool {
        self.source()
            .is_some_and(|source| crate::source::local::is_imported(&source.reference))
    }

    /// the recipe that makes the dataset, unless it is fetched from a source.
    pub(crate) fn recipe(&self) -> Option<&Recipe> {
        match &self.origin {
            Origin::Source(_) => None,
            Origin::Derived(recipe) => Some(recipe),
        }
    }
}

impl Manifest {
    pub(crate) fn parse(text: &str) -> Result<Manifest, Error> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let document = Document::parse(text).map_err(|err| not_toml(text, &err))?;
        let root = document.as_table();
        let reader = Reader {
            text,
            starts: std::cell::OnceCell::new(),
            private: Some(private(root)),
        };
        reader.known(root, &TOP_KEYS, "")?;
        let data_dir = match root.get("project") {
            None => DATA_DIR.to_owned(),
            Some(item) => {
                let project = item.as_table_like().ok_or_else(|| {
                    reader.error(
                        item.span(),
                        "[project] in data.toml must be a table".to_owned(),
                    )
                })?;
                reader.known(project, &PROJECT_KEYS, " in [project]")?;
                match project.get("data_dir") {
                    None => DATA_DIR.to_owned(),
                    Some(item) => reader.relative(item, "data_dir")?,
                }
            }
        };
        let mut datasets = BTreeMap::new();
        if let Some(item) = root.get("datasets") {
            let table = item.as_table_like().ok_or_else(|| {
                reader.error(
                    item.span(),
                    "datasets in data.toml must be a table".to_owned(),
                )
            })?;
            for (name, item) in table.iter() {
                let entry = reader.entry(table, name, item)?;
                datasets.insert(name.to_owned(), entry);
            }
        }
        recipes_read_known_datasets(&datasets)?;
        code_stays_out_of_the_data_folder(&data_dir, &datasets)?;
        Ok(Manifest { data_dir, datasets })
    }
}

/// where data.toml may hold a secret, sorted and merged: each dataset's ref, which can carry a
/// key in its query, and its env, which should not but may. a message cites such a line by its
/// number alone.
fn private(root: &Table) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let datasets = root.get("datasets").and_then(Item::as_table_like);
    for (_, entry) in datasets.into_iter().flat_map(TableLike::iter) {
        let Some(entry) = entry.as_table_like() else {
            continue;
        };
        spans.extend(entry.get("ref").and_then(Item::span));
        let Some(env) = entry.get("env") else {
            continue;
        };
        spans.extend(env.span());
        for (_, value) in env.as_table_like().into_iter().flat_map(TableLike::iter) {
            spans.extend(value.span());
        }
    }
    spans.sort_unstable_by_key(|span| span.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    merged
}

/// refuses code inside the data folder, or holding it, since fetchloom fills it from data.lock,
/// so its files would change the key they come from.
fn code_stays_out_of_the_data_folder(
    data_dir: &str,
    datasets: &BTreeMap<String, Entry>,
) -> Result<(), Error> {
    let data = name::fold(data_dir);
    for (name, entry) in datasets {
        for path in entry
            .recipe()
            .map(|recipe| recipe.code.as_slice())
            .unwrap_or_default()
        {
            let folded = name::fold(path);
            let reason = if folded == data || folded.starts_with(&format!("{data}/")) {
                "it is inside the data folder, which fetchloom fills"
            } else if data.starts_with(&format!("{folded}/")) {
                "it holds the data folder, which fetchloom fills"
            } else {
                continue;
            };
            let error = Error::new(
                Kind::ProjectInvalid,
                format!("code \"{path}\" in datasets.{name} is refused because {reason}"),
            )
            .help(OWN_FILES);
            return Err(match entry.lines.get("code") {
                Some(line) => error.detail(line.clone()),
                None => error,
            });
        }
    }
    Ok(())
}

/// refuses a recipe made from a dataset data.toml does not have, or recipes made from each
/// other, which no order of steps could make.
fn recipes_read_known_datasets(datasets: &BTreeMap<String, Entry>) -> Result<(), Error> {
    for (name, entry) in datasets {
        let Some(recipe) = entry.recipe() else {
            continue;
        };
        for input in &recipe.from {
            if datasets.contains_key(input) {
                continue;
            }
            let error = Error::new(
                Kind::ProjectInvalid,
                format!("datasets.{name} is made from {input}, which data.toml does not have"),
            );
            let error = match entry.lines.get("from") {
                Some(line) => error.detail(line.clone()),
                None => error,
            };
            return Err(
                match name::closest(input, datasets.keys().map(String::as_str)) {
                    Some(best) => error.help(format!("did you mean {best}?")),
                    None => error.help(format!(
                        "add [datasets.{input}] to data.toml, or remove {input} from the from of datasets.{name}"
                    )),
                },
            );
        }
    }
    let Err(cycle) = recipe::order(datasets) else {
        return Ok(());
    };
    let message = if let [only, _] = cycle.as_slice() {
        format!("datasets.{only} is made from itself")
    } else {
        let mut names: Vec<&str> = cycle[..cycle.len() - 1].to_vec();
        names.sort_unstable();
        let names = match names.split_last() {
            Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
            _ => names.join(""),
        };
        format!(
            "datasets {names} are made from each other: {}",
            cycle.join(" \u{2192} ")
        )
    };
    let error = Error::new(Kind::ProjectInvalid, message);
    let error = match datasets
        .get(cycle[0])
        .and_then(|entry| entry.lines.get("from"))
    {
        Some(line) => error.detail(line.clone()),
        None => error,
    };
    Err(error.help(
        "a recipe can read only datasets made before it; take one of them out of the other's from",
    ))
}

/// reads data.toml's items into types, pointing at the line of any mistake.
struct Reader<'a> {
    text: &'a str,
    /// where each line begins, found the first time a line is cited.
    starts: std::cell::OnceCell<Vec<usize>>,
    /// the spans [`private`] gives; invalid TOML has no trustworthy bounds for hiding credentials.
    private: Option<Vec<Range<usize>>>,
}

impl Reader<'_> {
    fn entry(&self, datasets: &dyn TableLike, name: &str, item: &Item) -> Result<Entry, Error> {
        let key_span = datasets.key(name).and_then(Key::span);
        if let Some(problem) = name::dataset_name_problem(name) {
            let error = self
                .error(key_span, format!("\"{name}\" is not a valid dataset name"))
                .detail(problem);
            return Err(match name::dataset_name(name) {
                Some(suggested) => error.help(format!(
                    "rename it to \"{suggested}\" in data.toml, then run the same command again"
                )),
                None => error.help("rename it in data.toml with lowercase letters, digits, - and _, then run the same command again"),
            });
        }
        let table = item.as_table_like().ok_or_else(|| {
            self.error(
                item.span(),
                format!("datasets.{name} in data.toml must be a table with a ref"),
            )
        })?;
        self.known(table, &DATASET_KEYS, &format!(" in datasets.{name}"))?;
        let mut lines = BTreeMap::new();
        for key in DATASET_KEYS {
            if let Some(span) = table.key(key).and_then(Key::span) {
                lines.insert(key, self.cite(span.start));
            }
        }
        let origin = if RECIPE_KEYS.iter().any(|key| table.contains_key(key)) {
            Origin::Derived(self.recipe(table, name)?)
        } else if !table.contains_key("ref") && table.iter().all(|(key, _)| key == "members") {
            return Err(self.left_members(datasets, table, name, item.span().or(key_span)));
        } else {
            Origin::Source(self.source(table, name, item.span().or(key_span))?)
        };
        Ok(Entry {
            origin,
            copy: self.boolean(table.get("copy"), "copy", false)?,
            path: table
                .get("path")
                .map(|item| self.relative(item, "path"))
                .transpose()?,
            place: self.boolean(table.get("place"), "place", true)?,
            lines,
        })
    }

    /// the refusal of `table`, the dataset `name` in `datasets` with members tables alone, as
    /// when only the dataset's own table was renamed. the new name is told only when a single
    /// other dataset's ref ends in the file of an archive the tables name.
    fn left_members(
        &self,
        datasets: &dyn TableLike,
        table: &dyn TableLike,
        name: &str,
        span: Option<Range<usize>>,
    ) -> Error {
        let archives: Vec<&str> = table
            .get("members")
            .and_then(Item::as_table_like)
            .map(|members| members.iter().map(|(archive, _)| archive).collect())
            .unwrap_or_default();
        let example = archives.first().copied().unwrap_or("<archive>");
        let links_to = |reference: &str| {
            let bare = reference.split(['?', '#']).next().unwrap_or(reference);
            let file = bare.rsplit('/').next().unwrap_or(bare);
            archives
                .iter()
                .any(|archive| archive.rsplit('/').next() == Some(file))
        };
        let renamed: Vec<&str> = datasets
            .iter()
            .filter(|(other, item)| {
                *other != name
                    && item
                        .as_table_like()
                        .and_then(|entry| entry.get("ref"))
                        .and_then(Item::as_str)
                        .is_some_and(links_to)
            })
            .map(|(other, _)| other)
            .collect();
        let error = self.error(
            span,
            format!("datasets.{name} in data.toml has members tables but no ref"),
        );
        error.help(match renamed.as_slice() {
            [new] => format!(
                "if {name} is now {new}, rename its members tables too, as [datasets.{new}.members.\"{example}\"]"
            ),
            _ => format!(
                "if {name} was renamed, rename its members tables with it, as [datasets.<new name>.members.\"{example}\"], or remove them"
            ),
        })
    }

    /// the source of a dataset fetched from one, which `table` names with its ref.
    fn source(
        &self,
        table: &dyn TableLike,
        name: &str,
        span: Option<Range<usize>>,
    ) -> Result<Source, Error> {
        let Some(reference) = table.get("ref") else {
            return Err(self
                .error(span, format!("datasets.{name} in data.toml has no ref"))
                .help("add a line such as ref = \"zenodo:3608990\""));
        };
        let reference = self.string(reference, "ref")?.trim().to_owned();
        if reference.is_empty() {
            return Err(self.error(
                table.get("ref").and_then(Item::span),
                format!("datasets.{name} has an empty ref"),
            ));
        }
        // data.lock tells entries apart by their text, which a line break inside a ref could fake
        if reference.contains(char::is_control) {
            let place = table
                .get("ref")
                .and_then(Item::span)
                .map(|span| self.cite(span.start));
            let error = Error::new(
                Kind::ProjectInvalid,
                format!(
                    "the ref of datasets.{name} holds a line break or another control character"
                ),
            )
            .help("write the ref on one line, without tabs");
            // the line may hold a key, so only its place is shown
            return Err(
                match place
                    .as_deref()
                    .and_then(|line| line.split_whitespace().next())
                {
                    Some(place) => error.detail(place.to_owned()),
                    None => error,
                },
            );
        }
        let mut select = self.patterns(table.get("select"), "select")?;
        // a link to one file or folder means that file or folder, as `add` writes it, unless
        // select says otherwise
        if select.is_empty()
            && let Ok(parsed) = crate::source::Reference::parse(&reference)
        {
            select.extend(
                parsed
                    .select()
                    .iter()
                    .filter_map(|pattern| Glob::new(pattern).ok()),
            );
        }
        let extract = self.boolean(table.get("extract"), "extract", true)?;
        Ok(Source {
            reference,
            select,
            exclude: self.patterns(table.get("exclude"), "exclude")?,
            extract,
            members: self.members(table, name, extract)?,
        })
    }

    /// the tables under `members` that choose the files of each archive, by its path. they
    /// choose among files fetchloom unpacks, so a dataset that keeps its archives packed refuses
    /// them.
    fn members(&self, table: &dyn TableLike, name: &str, extract: bool) -> Result<Members, Error> {
        let Some(item) = table.get("members") else {
            return Ok(Members::new());
        };
        let span = table
            .key("members")
            .and_then(Key::span)
            .or_else(|| item.span());
        let archives = item.as_table_like().ok_or_else(|| {
            self.error(
                span.clone(),
                format!("members in datasets.{name} must be a table for each archive"),
            )
            .help(format!(
                "write it as [datasets.{name}.members.\"<archive>\"] with select or exclude"
            ))
        })?;
        let mut members = Members::new();
        for (archive, item) in archives.iter() {
            let span = archives
                .key(archive)
                .and_then(Key::span)
                .or_else(|| item.span())
                .or_else(|| span.clone());
            if !extract {
                return Err(self
                    .error(
                        span,
                        format!("members in datasets.{name} chooses files inside archives, but extract = false keeps them packed"),
                    )
                    .help("remove the members tables, or remove extract = false"));
            }
            let filter = item.as_table_like().ok_or_else(|| {
                self.error(
                    span.clone(),
                    format!(
                        "members.\"{archive}\" in datasets.{name} must be a table with select or exclude"
                    ),
                )
            })?;
            self.known(
                filter,
                &MEMBER_KEYS,
                &format!(" in datasets.{name}.members.\"{archive}\""),
            )?;
            let mut lines = BTreeMap::new();
            lines.extend(span.map(|span| ("members", self.cite(span.start))));
            for key in MEMBER_KEYS {
                if let Some(span) = filter.key(key).and_then(Key::span) {
                    lines.insert(key, self.cite(span.start));
                }
            }
            let select = self.patterns(filter.get("select"), "select")?;
            let exclude = self.patterns(filter.get("exclude"), "exclude")?;
            members.insert(
                archive.to_owned(),
                MemberFilter {
                    select: glob::Set::new(select),
                    exclude: glob::Set::new(exclude),
                    lines,
                },
            );
        }
        Ok(members)
    }

    /// the recipe of a dataset made from others, which `table` gives with from and run. the
    /// datasets it reads are checked once every dataset is read.
    fn recipe(&self, table: &dyn TableLike, name: &str) -> Result<Recipe, Error> {
        let span = |key: &str| table.key(key).and_then(Key::span);
        let first = RECIPE_KEYS
            .into_iter()
            .find(|key| table.contains_key(key))
            .unwrap_or("from");
        if table.contains_key("ref") {
            return Err(self
                .error(
                    span(first),
                    format!("datasets.{name} has both ref and {first}, but a dataset is fetched from a source or made by a recipe, not both"),
                )
                .help("keep ref to fetch it, or from and run to make it from other datasets"));
        }
        if let Some(key) = SOURCE_KEYS.into_iter().find(|key| table.contains_key(key)) {
            return Err(self
                .error(
                    span(key),
                    format!("{key} is for a dataset fetched from a source, and datasets.{name} is made by a recipe"),
                )
                .help(format!("remove {key}, and choose files inside the step instead")));
        }
        let (from, run) = match (table.get("from"), table.get("run")) {
            (Some(from), Some(run)) => (from, run),
            (Some(_), None) => {
                return Err(self
                    .error(
                        span("from"),
                        format!("datasets.{name} has from but no run, so nothing makes it"),
                    )
                    .help(format!(
                        "add run, the program and arguments that make it, as {RUN_EXAMPLE}"
                    )));
            }
            (None, _) => {
                return Err(self
                    .error(
                        span(first),
                        format!("datasets.{name} has {first} but no from, so it reads no dataset"),
                    )
                    .help("add from with the datasets run reads, as from = [\"raw\"]"));
            }
        };
        let from = self.inputs(from, name)?;
        let run = self.run(run, name, &from)?;
        let code = match table.get("code") {
            None => Vec::new(),
            Some(item) => Self::strings(item).ok_or_else(|| {
                self.error(
                    item.span(),
                    "code in data.toml must be a list of files and folders in the project"
                        .to_owned(),
                )
                .help("write it as code = [\"clean.py\"]")
            })?,
        };
        let code = self.code(table.get("code"), name, code)?;
        let env = match table.get("env") {
            None => BTreeMap::new(),
            Some(item) => self.variables(item, name)?,
        };
        Ok(Recipe {
            from,
            run,
            code,
            env,
        })
    }

    /// a recipe's code: paths inside the project, each once, out of what fetchloom writes and
    /// of repository metadata. the data folder is checked once it is known.
    fn code(
        &self,
        item: Option<&Item>,
        name: &str,
        paths: Vec<String>,
    ) -> Result<Vec<String>, Error> {
        let span = item.and_then(Item::span);
        let mut code: Vec<String> = Vec::with_capacity(paths.len());
        for path in paths {
            let path = path.trim().trim_end_matches('/').to_owned();
            let first = path.split('/').next().unwrap_or_default();
            let refused = |reason: &str, help: &str| {
                self.error(
                    span.clone(),
                    format!("code \"{path}\" in datasets.{name} is refused because {reason}"),
                )
                .help(help.to_owned())
            };
            if let Some(reason) = name::escapes(&path) {
                return Err(refused(
                    reason,
                    "name a file or folder inside the project, with / between folders",
                ));
            }
            if let Some(reason) = name::metadata(&path) {
                return Err(refused(reason, OWN_FILES));
            }
            if WRITTEN
                .iter()
                .any(|written| first.eq_ignore_ascii_case(written))
            {
                return Err(refused("fetchloom writes it", OWN_FILES));
            }
            if code
                .iter()
                .any(|known| name::fold(known) == name::fold(&path))
            {
                return Err(self
                    .error(
                        span,
                        format!("datasets.{name} names the code \"{path}\" twice"),
                    )
                    .help("name each file or folder once in code"));
            }
            code.push(path);
        }
        Ok(code)
    }

    /// a recipe's variables: a table of strings, named as every system takes them, none of them
    /// fetchloom's own or a secret.
    fn variables(&self, item: &Item, name: &str) -> Result<BTreeMap<String, String>, Error> {
        let table = item.as_table_like().ok_or_else(|| {
            self.error(
                item.span(),
                "env in data.toml must be a table of variables".to_owned(),
            )
            .help("write it as env = { SEED = \"1\" }")
        })?;
        let mut variables = BTreeMap::new();
        for (variable, written) in table.iter() {
            let span = written.span().or_else(|| item.span());
            let Some(value) = written.as_str() else {
                let value = written;
                return Err(self
                    .error(
                        span,
                        "every value in env must be a string in quotes".to_owned(),
                    )
                    .help(format!(
                        "write it as {variable} = \"{}\"",
                        value
                            .as_value()
                            .map(|value| value.to_string().trim().to_owned())
                            .unwrap_or_default()
                    )));
            };
            let refused =
                |message: String, help: String| self.error(span.clone(), message).help(help);
            let upper = variable.to_ascii_uppercase();
            if !portable(variable) {
                return Err(refused(
                    format!(
                        "env in datasets.{name} sets \"{variable}\", which is not a variable name every system takes"
                    ),
                    "name variables with letters, digits and _, not starting with a digit"
                        .to_owned(),
                ));
            }
            if upper.starts_with("FETCHLOOM_") {
                return Err(refused(
                    format!("env in datasets.{name} sets {variable}, one of fetchloom's own variables"),
                    "set fetchloom's variables in your shell or its settings file, never in data.toml".to_owned(),
                ));
            }
            if secret(&upper) {
                return Err(refused(
                    format!("env in datasets.{name} sets {variable}, which looks like a secret"),
                    format!(
                        "data.toml and data.lock are shared, and the step inherits your environment, so set {variable} there"
                    ),
                ));
            }
            if value.contains('\0') {
                return Err(refused(
                    format!(
                        "env in datasets.{name} gives {variable} a value with a NUL character, which no program can be given"
                    ),
                    "remove the NUL character from the value".to_owned(),
                ));
            }
            // git for windows checks data.toml out with CRLF, which a multi-line string keeps
            variables.insert(variable.to_owned(), value.replace("\r\n", "\n"));
        }
        Ok(variables)
    }

    /// the datasets a recipe reads: a list of names, each once.
    fn inputs(&self, item: &Item, name: &str) -> Result<Vec<String>, Error> {
        let names = Self::strings(item).ok_or_else(|| {
            self.error(
                item.span(),
                "from in data.toml must be a list of dataset names".to_owned(),
            )
            .help("write it as from = [\"raw\"]")
        })?;
        if names.is_empty() {
            return Err(self
                .error(
                    item.span(),
                    format!("datasets.{name} is made from no dataset"),
                )
                .help("name the datasets run reads in from, as from = [\"raw\"]"));
        }
        for (index, input) in names.iter().enumerate() {
            if input == recipe::OUT {
                return Err(self
                    .error(
                        item.span(),
                        format!("datasets.{name} is made from out, the name run keeps for the result's folder"),
                    )
                    .help("rename the dataset out in data.toml"));
            }
            if names[..index].contains(input) {
                return Err(self
                    .error(
                        item.span(),
                        format!("datasets.{name} is made from {input} twice"),
                    )
                    .help("name each dataset once in from"));
            }
        }
        Ok(names)
    }

    /// a recipe's command: one word after another, each read by [`Template::parse`], naming the
    /// folder of every dataset in `from` and of the result, and no other.
    fn run(&self, item: &Item, name: &str, from: &[String]) -> Result<Vec<Template>, Error> {
        let words = Self::strings(item).ok_or_else(|| {
            self.error(
                item.span(),
                "run in data.toml must be a list of words, since no shell reads it".to_owned(),
            )
            .help(format!(
                "write the program and each argument as a word of their own, as {RUN_EXAMPLE}"
            ))
        })?;
        if words.is_empty() {
            return Err(self
                .error(
                    item.span(),
                    format!("run in datasets.{name} is empty, so there is no program to start"),
                )
                .help(format!(
                    "write the program and each argument as a word of their own, as {RUN_EXAMPLE}"
                )));
        }
        let mut run = Vec::with_capacity(words.len());
        for word in &words {
            let template = Template::parse(word).map_err(|reason| {
                self.error(
                    item.span(),
                    format!("a word of run in datasets.{name} {reason}"),
                )
                .help("write {{ and }} for a brace of its own, and {name} for the folder of a dataset it reads")
            })?;
            run.push(template);
        }
        let refused = |message: String, help: String| self.error(item.span(), message).help(help);
        if run[0].text().is_empty() {
            return Err(refused(
                format!("the program in run of datasets.{name} is empty"),
                format!("make the first word the program to start, as {RUN_EXAMPLE}"),
            ));
        }
        let slots: Vec<Slot> = run.iter().flat_map(Template::slots).collect();
        for slot in &slots {
            if let Slot::Input(input) = slot
                && !from.iter().any(|known| known == input)
            {
                let names = from.iter().map(String::as_str).chain([recipe::OUT]);
                let help = match name::closest(input, names) {
                    Some(best) => {
                        format!("did you mean {{{best}}}? a name in braces is one of from, or out")
                    }
                    None => "a name in braces is one of from, or out".to_owned(),
                };
                return Err(refused(
                    format!(
                        "run in datasets.{name} names {{{input}}}, which is not a dataset it is made from"
                    ),
                    help,
                ));
            }
        }
        if !slots.contains(&Slot::Out) {
            return Err(refused(
                format!(
                    "run in datasets.{name} never names {{out}}, so the step has nowhere to write"
                ),
                "put {out} where the step takes the folder to write the result in".to_owned(),
            ));
        }
        if let Some(input) = from
            .iter()
            .find(|input| !slots.contains(&Slot::Input(input)))
        {
            return Err(refused(
                format!(
                    "run in datasets.{name} never names {{{input}}}, so the step cannot find {input}"
                ),
                format!("put {{{input}}} where the step takes the folder holding {input}'s files"),
            ));
        }
        Ok(run)
    }

    /// the strings of an array, or none when `item` is not an array of strings.
    fn strings(item: &Item) -> Option<Vec<String>> {
        item.as_array()?
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect()
    }

    /// refuses any key not in `known`, suggesting the closest one.
    fn known(&self, table: &dyn TableLike, known: &[&str], within: &str) -> Result<(), Error> {
        for (key, _) in table.iter() {
            if !known.contains(&key) {
                let error = self.error(
                    table.key(key).and_then(Key::span),
                    format!("data.toml has a key fetchloom does not know: \"{key}\"{within}"),
                );
                return Err(match name::closest(key, known.iter().copied()) {
                    Some(best) => error.help(format!("did you mean `{best}`?")),
                    None => error.help(format!("the keys here are {}", known.join(", "))),
                });
            }
        }
        Ok(())
    }

    fn string<'b>(&self, item: &'b Item, key: &str) -> Result<&'b str, Error> {
        item.as_str().ok_or_else(|| {
            self.error(
                item.span(),
                format!("{key} in data.toml must be a string in quotes"),
            )
        })
    }

    fn boolean(&self, item: Option<&Item>, key: &str, default: bool) -> Result<bool, Error> {
        item.map_or(Ok(default), |item| {
            item.as_bool().ok_or_else(|| {
                self.error(
                    item.span(),
                    format!("{key} in data.toml must be true or false"),
                )
            })
        })
    }

    /// a path inside the project, which may not climb out of it. a dataset's `path` may also be
    /// [`DATA_FOLDER_ITSELF`], the data folder.
    fn relative(&self, item: &Item, key: &str) -> Result<String, Error> {
        let text = self.string(item, key)?.trim().trim_end_matches('/');
        let refused = |reason: &str| {
            self.error(
                item.span(),
                format!("{key} = \"{text}\" in data.toml is refused because {reason}"),
            )
        };
        if let Some(reason) = name::metadata(text) {
            return Err(
                refused(reason).help(format!("choose a {key} outside the repository's metadata"))
            );
        }
        if shorthand(text) {
            let inside = if key == "data_dir" {
                "the project"
            } else {
                "the data folder"
            };
            return Err(refused(&format!("fetchloom does not expand ~ or $NAME, and the folder is always inside {inside}"))
                .help("to keep data on a scratch disk, set FETCHLOOM_CACHE to a folder there, or move the whole project there. see https://nikhil-yn-aix.github.io/fetchloom/user/hpc.html"));
        }
        if text == DATA_FOLDER_ITSELF {
            return match key {
                "path" => Ok(text.to_owned()),
                _ => Err(refused("it would mix data with the project's own files")
                    .help("use a folder inside the project, such as data")),
            };
        }
        match name::escapes(text) {
            None => Ok(text.to_owned()),
            Some(reason) => Err(refused(reason).help(match key {
                "path" if text.is_empty() => "use \".\" for the data folder itself",
                "path" => {
                    "use a folder inside the data folder, with / between folders, or \".\" for the data folder itself"
                }
                _ => "use a folder inside the project, with / between folders",
            })),
        }
    }

    /// one pattern or a list of them.
    fn patterns(&self, item: Option<&Item>, key: &str) -> Result<Vec<Glob>, Error> {
        let Some(item) = item else {
            return Ok(Vec::new());
        };
        let texts: Vec<(&str, Option<Range<usize>>)> = if let Some(text) = item.as_str() {
            vec![(text, item.span())]
        } else if let Some(array) = item.as_array() {
            array
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .map(|text| (text, value.span()))
                        .ok_or_else(|| {
                            self.error(
                                value.span(),
                                format!("every pattern in {key} must be a string in quotes"),
                            )
                        })
                })
                .collect::<Result<_, _>>()?
        } else {
            return Err(self.error(
                item.span(),
                format!("{key} in data.toml must be a pattern or a list of patterns"),
            ));
        };
        texts
            .into_iter()
            .map(|(text, span)| {
                Glob::new(text)
                    .map_err(|reason| self.error(span, format!("the {key} pattern {reason}")))
            })
            .collect()
    }

    fn error(&self, span: Option<Range<usize>>, message: String) -> Error {
        let error = Error::new(Kind::ProjectInvalid, message);
        match span {
            Some(span) => error.detail(self.cite(span.start)),
            None => error,
        }
    }

    /// the line holding `offset`, as `data.toml:7   select = ["*.edf"]`.
    fn cite(&self, offset: usize) -> String {
        let offset = offset.min(self.text.len());
        // the lines are found once, since a large data.toml cites every key of every dataset
        let starts = self.starts.get_or_init(|| {
            std::iter::once(0)
                .chain(self.text.match_indices('\n').map(|(at, _)| at + 1))
                .collect()
        });
        let line = starts.partition_point(|&start| start <= offset);
        let start = starts[line - 1];
        let end = self.text[start..]
            .find('\n')
            .map_or(self.text.len(), |at| start + at);
        let private = self.private.as_ref().is_none_or(|spans| {
            let after = spans.partition_point(|span| span.start < end);
            after > 0 && spans[after - 1].end > start
        });
        if private {
            format!("{FILE}:{line}")
        } else {
            format!("{FILE}:{line}   {}", self.text[start..end].trim())
        }
    }
}

/// whether every system takes `variable` as a name: letters, digits and `_`, not starting with a
/// digit, as POSIX names them.
fn portable(variable: &str) -> bool {
    variable
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && variable
            .bytes()
            .next()
            .is_some_and(|first| !first.is_ascii_digit())
}

/// whether an uppercase variable name looks like it holds a secret: a word of it ends in TOKEN,
/// holds a word of [`SECRETS`], or two words name a key, as `API_KEY`. `MAX_TOKENS` and
/// `TOKENIZERS_PARALLELISM` count tokens of text, so they stay usable.
fn secret(upper: &str) -> bool {
    let words: Vec<&str> = upper.split('_').collect();
    words.iter().any(|word| {
        word.ends_with("TOKEN")
            || SECRETS.iter().any(|secret| word.contains(secret))
            || KEYS.iter().any(|key| word.ends_with(&format!("{key}KEY")))
    }) || words
        .windows(2)
        .any(|pair| pair[1] == "KEY" && KEYS.contains(&pair[0]))
}

/// whether a folder in data.toml is shell shorthand a person may expect to be expanded: it
/// starts with `~` or `$`, or names a variable such as `$SCRATCH` or `${SCRATCH}`.
fn shorthand(folder: &str) -> bool {
    folder.starts_with(['~', '$'])
        || folder
            .split('$')
            .skip(1)
            .any(|after| after.starts_with(|c: char| c.is_ascii_alphabetic() || "_{".contains(c)))
}

/// data.toml's text with one more dataset, or a new file holding just that dataset. it goes
/// before the dataset whose name follows its own, so two people adding different datasets rarely
/// change the same lines.
pub(crate) fn with_dataset(
    text: &str,
    name: &str,
    reference: &str,
    select: &[String],
    exclude: &[String],
    extract: bool,
) -> Result<String, Error> {
    let mut document = editable(text)?;
    let mut table = Table::new();
    table.insert("ref", toml_edit::value(reference));
    for (key, patterns) in [("select", select), ("exclude", exclude)] {
        if !patterns.is_empty() {
            table.insert(key, toml_edit::value(patterns.iter().collect::<Array>()));
        }
    }
    if !extract {
        table.insert("extract", toml_edit::value(false));
    }
    if !put(&mut document, name, table) {
        let reference = crate::net::mask_query(reference);
        return Err(Error::new(
            Kind::ProjectInvalid,
            "data.toml writes its datasets inline, so fetchloom cannot add to them",
        )
        .help(format!(
            "add [datasets.{name}] with ref = \"{reference}\" by hand"
        )));
    }
    Ok(document.to_string())
}

/// puts `table` among the datasets as `name`, before the dataset whose name follows it, or after
/// them all, with the comments touching its header. false when the datasets are written inline,
/// where no table can go.
fn put(document: &mut DocumentMut, name: &str, mut table: Table) -> bool {
    let blank_line_first = !document.as_table().is_empty();
    let datasets = document.entry("datasets").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    let Some(datasets) = datasets.as_table_mut() else {
        return false;
    };
    let touching = split_note(prefix(&table)).1.to_owned();
    let next = successor(datasets, name);
    let above = if blank_line_first { "\n" } else { "" };
    table.decor_mut().set_prefix(format!("{above}{touching}"));
    table.set_position(None);
    datasets.insert(name, Item::Table(table));
    if let Some(next) = next {
        insert_before(document, name, &next, &touching);
    }
    true
}

/// the dataset table whose name follows `name` most closely. none when `name` comes last or the
/// others are written inline.
fn successor(datasets: &Table, name: &str) -> Option<String> {
    datasets
        .iter()
        .filter(|(other, item)| {
            *other > name
                && item
                    .as_table()
                    .is_some_and(|table| !table.is_dotted() && table.position().is_some())
        })
        .map(|(other, _)| other)
        .min()
        .map(str::to_owned)
}

/// moves the dataset `name`, just put last, to just before the dataset `next`, with `touching`,
/// its own comments, above its header. a note a blank line above `next`, such as one at the top
/// of the file, then stays above both.
fn insert_before(document: &mut DocumentMut, name: &str, next: &str, touching: &str) {
    let Some(following) = document
        .get("datasets")
        .and_then(|datasets| datasets.get(next))
        .and_then(Item::as_table)
    else {
        return;
    };
    let Some(position) = following.position() else {
        return;
    };
    let (note, theirs) = split_note(prefix(following));
    let first = document.iter().all(|(_, item)| !item.is_value())
        && first_position(document.as_table()).is_none_or(|first| first >= position);
    let above = if note.is_empty() && !first {
        "\n"
    } else {
        note
    };
    let added = format!("{above}{touching}");
    let kept = format!("\n{theirs}");
    shift_positions(document.as_table_mut(), position);
    let Some(datasets) = document.get_mut("datasets").and_then(Item::as_table_mut) else {
        return;
    };
    if let Some(following) = datasets.get_mut(next).and_then(Item::as_table_mut) {
        following.decor_mut().set_prefix(kept);
    }
    if let Some(table) = datasets.get_mut(name).and_then(Item::as_table_mut) {
        table.set_position(Some(position));
        table.decor_mut().set_prefix(added);
    }
}

/// the comments and blank lines above a table's header.
fn prefix(table: &Table) -> &str {
    table
        .decor()
        .prefix()
        .and_then(|prefix| prefix.as_str())
        .unwrap_or_default()
}

/// `prefix` split after its last blank line: a note that stands apart, such as one at the top of
/// the file, and the comments touching the header below it.
fn split_note(prefix: &str) -> (&str, &str) {
    let mut end = 0;
    let mut at = 0;
    for line in prefix.split_inclusive('\n') {
        at += line.len();
        if line.trim().is_empty() {
            end = at;
        }
    }
    prefix.split_at(end)
}

/// the earliest place of any table written with a header below `table`.
fn first_position(table: &Table) -> Option<isize> {
    let mut first: Option<isize> = None;
    for (_, item) in table {
        let tables: Vec<&Table> = match item {
            Item::Table(table) => vec![table],
            Item::ArrayOfTables(array) => array.iter().collect(),
            _ => Vec::new(),
        };
        for table in tables {
            let own = table
                .position()
                .filter(|_| !table.is_implicit() && !table.is_dotted());
            for position in own.into_iter().chain(first_position(table)) {
                first = Some(first.map_or(position, |first| first.min(position)));
            }
        }
    }
    first
}

/// moves every table at `from` or after it one place later, making room for one more there.
fn shift_positions(table: &mut Table, from: isize) {
    let mut shift = |table: &mut Table| {
        if let Some(position) = table.position().filter(|position| *position >= from) {
            table.set_position(Some(position + 1));
        }
        shift_positions(table, from);
    };
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(table) => shift(table),
            Item::ArrayOfTables(array) => array.iter_mut().for_each(&mut shift),
            _ => {}
        }
    }
}

/// data.toml's text without one dataset. comments touching its header go with it, and comments
/// a blank line above stay, such as a note at the top of the file.
pub(crate) fn without_dataset(text: &str, name: &str) -> Result<String, Error> {
    let mut document = editable(text)?;
    remove(&mut document, name);
    Ok(document.to_string().trim_start_matches('\n').to_owned())
}

/// data.toml's text with `patterns` in the `exclude` of the dataset `name`, or of its members
/// table for `archive`, each once, keeping comments, order and spacing. a table for an archive
/// gets a header of its own under the dataset's, as `[datasets.pets.members."pets.zip"]`.
///
/// # Errors
///
/// `project.invalid` for a dataset or a members table written inline, which gets no new keys,
/// as `add` refuses to add to inline datasets.
pub(crate) fn with_exclusions(
    text: &str,
    name: &str,
    archive: Option<&str>,
    patterns: &[String],
) -> Result<String, Error> {
    let mut document = editable(text)?;
    let dataset = document
        .get_mut("datasets")
        .and_then(Item::as_table_mut)
        .and_then(|datasets| datasets.get_mut(name))
        .and_then(Item::as_table_mut)
        .ok_or_else(|| inline(&format!("datasets.{name}")))?;
    let table = match archive {
        None => dataset,
        Some(archive) => {
            let dotted = dataset.is_dotted();
            let members = dataset
                .entry("members")
                .or_insert_with(|| {
                    let mut members = Table::new();
                    members.set_implicit(true);
                    members.set_dotted(dotted);
                    Item::Table(members)
                })
                .as_table_mut()
                .ok_or_else(|| inline(&format!("datasets.{name}.members")))?;
            let dotted = members.is_dotted();
            members
                .entry(archive)
                .or_insert_with(|| {
                    let mut table = Table::new();
                    table.set_dotted(dotted);
                    Item::Table(table)
                })
                .as_table_mut()
                .ok_or_else(|| inline(&format!("datasets.{name}.members.\"{archive}\"")))?
        }
    };
    exclude_once(table, patterns);
    Ok(document.to_string())
}

/// the refusal to add to `what`, a part of data.toml written inline, which gets no new keys, as
/// `add` refuses to add to inline datasets.
fn inline(what: &str) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!("data.toml writes {what} inline, so fetchloom cannot add to it"),
    )
    .help(format!(
        "write it as a [{what}] table, then run the same command again"
    ))
}

/// data.toml's text with the members table of the dataset `name` under the glob key `from`
/// copied under `archive`, its select and exclude as written, without the comments around them.
/// a table under an archive's own path wins over a glob, so the one drop writes for an archive a
/// glob covers starts as a copy, and the archive keeps what the glob chose.
///
/// # Errors
///
/// `project.invalid` for a dataset or a members table written inline, as [`with_exclusions`].
pub(crate) fn with_members_copied(
    text: &str,
    name: &str,
    from: &str,
    archive: &str,
) -> Result<String, Error> {
    let mut document = editable(text)?;
    let members = document
        .get_mut("datasets")
        .and_then(Item::as_table_mut)
        .and_then(|datasets| datasets.get_mut(name))
        .and_then(Item::as_table_mut)
        .ok_or_else(|| inline(&format!("datasets.{name}")))?
        .get_mut("members")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| inline(&format!("datasets.{name}.members")))?;
    let source = members
        .get(from)
        .and_then(Item::as_table)
        .ok_or_else(|| inline(&format!("datasets.{name}.members.\"{from}\"")))?;
    let mut table = Table::new();
    table.set_dotted(members.is_dotted());
    for key in MEMBER_KEYS {
        if let Some(value) = source.get(key).and_then(Item::as_value) {
            let mut value = value.clone();
            value.decor_mut().clear();
            table.insert(key, Item::Value(value));
        }
    }
    members.insert(archive, Item::Table(table));
    Ok(document.to_string())
}

/// adds each of `patterns` that `exclude` in `table` lacks, a list of more than one written a
/// pattern per line.
fn exclude_once(table: &mut Table, patterns: &[String]) {
    let mut list = match table.get("exclude").and_then(Item::as_value) {
        Some(Value::Array(list)) => list.clone(),
        Some(Value::String(one)) => {
            let mut list: Array = std::iter::once(one.value().clone()).collect();
            *list.decor_mut() = one.decor().clone();
            list
        }
        _ => Array::new(),
    };
    let mut seen: std::collections::HashSet<String> = list
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    for pattern in patterns {
        if seen.insert(pattern.clone()) {
            list.push(pattern.as_str());
        }
    }
    if list.len() > 1 {
        one_per_line(&mut list);
    }
    match table.get_mut("exclude") {
        Some(item) => *item = toml_edit::value(list),
        None => {
            table.insert("exclude", toml_edit::value(list));
        }
    }
}

/// writes each value of `list` on a line of its own, keeping the lines and comments of one
/// written that way already and indenting new values as its last.
fn one_per_line(list: &mut Array) {
    let raw = |value: &Value| {
        value
            .decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .map(str::to_owned)
    };
    let last_line = list
        .iter()
        .filter_map(raw)
        .filter_map(|prefix| prefix.rfind('\n').map(|at| prefix[at..].to_owned()))
        .last();
    let open = last_line.is_some();
    let indent = last_line.unwrap_or_else(|| "\n  ".to_owned());
    for value in list.iter_mut() {
        if !open || value.decor().prefix().is_none() {
            value.decor_mut().set_prefix(indent.clone());
            value.decor_mut().set_suffix("");
        }
    }
    list.set_trailing_comma(true);
    if !list
        .trailing()
        .as_str()
        .is_some_and(|trailing| trailing.contains('\n'))
    {
        list.set_trailing("\n");
    }
}

/// data.toml's text without the members table of the dataset `name` for `archive`, as when the
/// whole archive is left out, and without `members` once it is empty.
pub(crate) fn without_members(text: &str, name: &str, archive: &str) -> Result<String, Error> {
    let mut document = editable(text)?;
    if let Some(dataset) = document
        .get_mut("datasets")
        .and_then(Item::as_table_like_mut)
        .and_then(|datasets| datasets.get_mut(name))
        .and_then(Item::as_table_like_mut)
    {
        let emptied = dataset
            .get_mut("members")
            .and_then(Item::as_table_like_mut)
            .is_some_and(|members| {
                members.remove(archive);
                members.is_empty()
            });
        if emptied {
            dataset.remove("members");
        }
    }
    Ok(document.to_string())
}

/// takes the dataset `name` out of `document`, as [`without_dataset`] says.
fn remove(document: &mut DocumentMut, name: &str) {
    let Some(datasets) = document
        .get_mut("datasets")
        .and_then(Item::as_table_like_mut)
    else {
        return;
    };
    let removed = datasets.get(name).and_then(Item::as_table).map(|table| {
        let prefix = table
            .decor()
            .prefix()
            .and_then(|prefix| prefix.as_str())
            .unwrap_or_default();
        let kept = prefix
            .rfind("\n\n")
            .map_or("", |at| &prefix[..at + 2])
            .to_owned();
        (table.position(), kept)
    });
    datasets.remove(name);
    if datasets.is_empty() {
        document.remove("datasets");
    }
    if let Some((Some(position), kept)) = removed
        && !kept.trim().is_empty()
    {
        if let Some(table) = next_table(document, position) {
            let prefix = table
                .decor()
                .prefix()
                .and_then(|prefix| prefix.as_str())
                .unwrap_or_default();
            let joined = format!("{kept}{}", prefix.trim_start_matches('\n'));
            table.decor_mut().set_prefix(joined);
        } else {
            let trailing = document.trailing().as_str().unwrap_or_default().to_owned();
            document.set_trailing(format!("{}\n{trailing}", kept.trim_end()));
        }
    }
}

/// data.toml's text with the dataset `name` as `other`, the text of another data.toml, writes it,
/// comments touching its header included: in place of the one `text` has, under any note above
/// that one, or else before the dataset whose name follows it.
pub(crate) fn with_dataset_from(text: &str, other: &str, name: &str) -> Result<String, Error> {
    let mut document = editable(text)?;
    let copied = editable(other)?
        .get("datasets")
        .and_then(Item::as_table_like)
        .and_then(|datasets| datasets.get(name))
        .cloned();
    let mut table = match copied {
        Some(Item::Table(table)) => table,
        Some(Item::Value(Value::InlineTable(inline))) => inline.into_table(),
        _ => {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!("the other data.toml has no table for datasets.{name}"),
            ));
        }
    };
    table.set_dotted(false);
    table.set_implicit(false);
    let touching = split_note(prefix(&table)).1.to_owned();
    let place = document
        .get_mut("datasets")
        .and_then(Item::as_table_mut)
        .and_then(|datasets| datasets.get_mut(name))
        .and_then(Item::as_table_mut)
        .filter(|table| !table.is_dotted());
    if let Some(found) = place
        && let Some(position) = found.position()
    {
        let above = split_note(prefix(found)).0;
        table.decor_mut().set_prefix(format!("{above}{touching}"));
        table.set_position(Some(position));
        *found = table;
        return Ok(document.to_string());
    }
    remove(&mut document, name);
    if !put(&mut document, name, table) {
        return Err(Error::new(
            Kind::ProjectInvalid,
            "data.toml writes its datasets inline, so fetchloom cannot merge them",
        )
        .help("merge data.toml by hand, then run `fetchloom lock`"));
    }
    Ok(document.to_string())
}

/// each dataset's own text: its table as it would read alone in a data.toml, with the comments
/// touching its header, so two files can be compared a dataset at a time.
pub(crate) fn tables(text: &str) -> Result<BTreeMap<String, String>, Error> {
    let document = editable(text)?;
    let Some(datasets) = document.get("datasets").and_then(Item::as_table_like) else {
        return Ok(BTreeMap::new());
    };
    Ok(datasets
        .iter()
        .map(|(name, item)| {
            let mut item = item.clone();
            if let Item::Table(table) = &mut item {
                let touching = split_note(prefix(table)).1.to_owned();
                table.decor_mut().set_prefix(touching);
                table.set_position(None);
            }
            let mut alone = Table::new();
            alone.set_implicit(true);
            alone.insert(name, item);
            let mut document = DocumentMut::new();
            document.insert("datasets", Item::Table(alone));
            (name.to_owned(), document.to_string())
        })
        .collect())
}

/// what data.toml holds outside its datasets, such as `[project]`, notes and comments, without
/// blank lines.
pub(crate) fn outside(text: &str) -> Result<String, Error> {
    let mut document = editable(text)?;
    let names: Vec<String> = document
        .get("datasets")
        .and_then(Item::as_table_like)
        .map(|datasets| datasets.iter().map(|(name, _)| name.to_owned()).collect())
        .unwrap_or_default();
    for name in &names {
        remove(&mut document, name);
    }
    Ok(document
        .to_string()
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// the first table written after `position`.
fn next_table(document: &mut DocumentMut, position: isize) -> Option<&mut Table> {
    let mut tables: Vec<&mut Table> = Vec::new();
    for (_, item) in document.iter_mut() {
        if let Some(table) = item.as_table_mut() {
            if table.is_implicit() {
                tables.extend(table.iter_mut().filter_map(|(_, item)| item.as_table_mut()));
            } else {
                tables.push(table);
            }
        }
    }
    tables
        .into_iter()
        .filter(|table| table.position().is_some_and(|at| at > position))
        .min_by_key(|table| table.position())
}

fn editable(text: &str) -> Result<DocumentMut, Error> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    text.parse::<DocumentMut>()
        .map_err(|err| not_toml(text, &err))
}

/// data.toml that is not TOML at all, pointing at the line where reading it stopped.
fn not_toml(text: &str, err: &TomlError) -> Error {
    let message = err.message().lines().next().unwrap_or_default().trim();
    Reader {
        text,
        starts: std::cell::OnceCell::new(),
        private: None,
    }
    .error(
        err.span(),
        format!("data.toml is not valid TOML: {message}"),
    )
    .help("fix that line, then run the command again")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Manifest {
        Manifest::parse(text).unwrap()
    }

    fn fails(text: &str) -> Error {
        Manifest::parse(text).unwrap_err()
    }

    #[test]
    fn a_full_manifest_reads_with_defaults() {
        let manifest = parse(
            "\u{feff}[project]\ndata_dir = \"inputs/\"\n\n[datasets.eeg]\nref = \"osf:abc12\"\nselect = [\"**/*.edf\", \"participants.tsv\"]\nexclude = \"**/derivatives/**\"\n\n[datasets.weights]\nref = \" https://example.org/w.bin \"\nextract = false\ncopy = true\npath = \"models/w\"\n",
        );
        assert_eq!(manifest.data_dir, "inputs");
        let eeg = &manifest.datasets["eeg"];
        let source = eeg.source().unwrap();
        assert_eq!(source.reference, "osf:abc12");
        assert_eq!(
            source.select.iter().map(Glob::as_str).collect::<Vec<_>>(),
            ["**/*.edf", "participants.tsv"]
        );
        assert_eq!(source.exclude.len(), 1);
        assert!(source.extract && !eeg.copy && eeg.path.is_none());
        assert_eq!(
            eeg.lines["select"],
            "data.toml:6   select = [\"**/*.edf\", \"participants.tsv\"]"
        );
        let weights = &manifest.datasets["weights"];
        assert_eq!(
            weights.source().unwrap().reference,
            "https://example.org/w.bin"
        );
        assert!(!weights.source().unwrap().extract && weights.copy);
        assert_eq!(weights.path.as_deref(), Some("models/w"));
        assert_eq!(parse("").data_dir, "data");
        assert!(parse("").datasets.is_empty());
        let inline = parse("datasets.iris = { ref = \"zenodo:3608990\" }\n");
        assert_eq!(inline.datasets.len(), 1, "inline tables work too");
        assert!(
            !inline.datasets["iris"].lines.is_empty(),
            "an entry from data.toml always cites its lines"
        );
    }

    const PETS: &str = "[datasets.pets]\nref = \"https://example.org/pets.zip\"\n";

    /// a members table keeps or leaves out the files of one archive by their paths inside it,
    /// top folder and all, whichever way the table is written.
    #[test]
    fn members_tables_choose_files_inside_each_archive() {
        let manifest = parse(&format!(
            "{PETS}\n[datasets.pets.members.\"pets.zip\"]\nexclude = [\"pets-v1/images/dog_010.png\", \"pets-v1/**/*.tmp\"]\n\n[datasets.pets.members.\"raw/b.tar.gz\"]\nselect = \"**/*.csv\"\nexclude = \"old/**\"\n"
        ));
        let members = &manifest.datasets["pets"].source().unwrap().members;
        assert_eq!(
            members.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            ["pets.zip", "raw/b.tar.gz"]
        );
        let zip = members.get("pets.zip").unwrap();
        assert!(zip.keeps("pets-v1/images/cat_001.png"));
        assert!(!zip.keeps("pets-v1/images/dog_010.png"));
        assert!(!zip.keeps("pets-v1/a/b.tmp"));
        assert!(
            zip.keeps("images/dog_010.png"),
            "the top folder is part of it"
        );
        let tar = members.get("raw/b.tar.gz").unwrap();
        assert!(tar.keeps("x/a.csv") && !tar.keeps("x/a.txt") && !tar.keeps("old/a.csv"));
        assert_eq!(
            zip.lines["exclude"],
            "data.toml:5   exclude = [\"pets-v1/images/dog_010.png\", \"pets-v1/**/*.tmp\"]"
        );
        assert_eq!(
            zip.lines["members"],
            "data.toml:4   [datasets.pets.members.\"pets.zip\"]"
        );
        for written in [
            format!("{PETS}members = {{ \"pets.zip\" = {{ exclude = [\"a/x.png\"] }} }}\n"),
            format!("{PETS}members.\"pets.zip\".exclude = \"a/x.png\"\n"),
        ] {
            let manifest = parse(&written);
            let members = &manifest.datasets["pets"].source().unwrap().members;
            let zip = members.get("pets.zip").unwrap();
            assert!(!zip.keeps("a/x.png") && zip.keeps("a/y.png"), "{written}");
        }
        assert!(
            parse(PETS).datasets["pets"]
                .source()
                .unwrap()
                .members
                .iter()
                .next()
                .is_none()
        );
    }

    /// member filters change where files go, not what data.lock pins, so they stay out of a
    /// dataset's spec: editing them needs no network and `sync --locked` still passes.
    #[test]
    fn member_filters_stay_out_of_the_spec() {
        let plain = parse(PETS);
        let filtered = parse(&format!(
            "{PETS}[datasets.pets.members.\"pets.zip\"]\nexclude = [\"a/x.png\"]\n"
        ));
        let spec = |manifest: &Manifest| {
            crate::project::lock::spec(manifest.datasets["pets"].source().unwrap())
        };
        assert_eq!(spec(&plain), spec(&filtered));
    }

    #[test]
    fn members_tables_refuse_what_cannot_apply() {
        for (text, message, line) in [
            (
                format!(
                    "{PETS}extract = false\n[datasets.pets.members.\"pets.zip\"]\nexclude = [\"a/x.png\"]\n"
                ),
                "members in datasets.pets chooses files inside archives, but extract = false keeps them packed",
                "data.toml:4   [datasets.pets.members.\"pets.zip\"]",
            ),
            (
                format!("{PETS}members = [\"pets.zip\"]\n"),
                "members in datasets.pets must be a table for each archive",
                "data.toml:3   members = [\"pets.zip\"]",
            ),
            (
                format!("{PETS}members.\"pets.zip\" = \"a/x.png\"\n"),
                "members.\"pets.zip\" in datasets.pets must be a table with select or exclude",
                "data.toml:3   members.\"pets.zip\" = \"a/x.png\"",
            ),
            (
                format!("{PETS}[datasets.pets.members.\"pets.zip\"]\nexclude = 7\n"),
                "exclude in data.toml must be a pattern or a list of patterns",
                "data.toml:4   exclude = 7",
            ),
            (
                format!("{PETS}[datasets.pets.members.\"pets.zip\"]\nexclude = [\"a\\\\x.png\"]\n"),
                "the exclude pattern \"a\\x.png\" has a \\. patterns use / between folders",
                "data.toml:4   exclude = [\"a\\\\x.png\"]",
            ),
        ] {
            let error = fails(&text);
            assert_eq!(error.kind, Kind::ProjectInvalid, "{text}");
            assert_eq!(error.message, message, "{text}");
            assert_eq!(error.details, [line], "{text}");
        }
        let made = fails(&format!(
            "{PETS}[datasets.b]\nfrom = [\"pets\"]\nrun = [\"t\", \"{{pets}}\", \"{{out}}\"]\n[datasets.b.members.\"x.zip\"]\nexclude = \"a\"\n"
        ));
        assert_eq!(
            made.message,
            "members is for a dataset fetched from a source, and datasets.b is made by a recipe"
        );
    }

    #[test]
    fn mistakes_point_at_their_line_and_suggest_a_fix() {
        let typo = fails("[datasets.eeg]\nref = \"osf:abc12\"\nselct = [\"*.edf\"]\n");
        assert_eq!(typo.kind, Kind::ProjectInvalid);
        assert_eq!(typo.details, ["data.toml:3   selct = [\"*.edf\"]"]);
        assert_eq!(typo.help.as_deref(), Some("did you mean `select`?"));
        let no_ref = fails("[datasets.eeg]\nselect = \"*\"\n");
        assert!(no_ref.message.contains("has no ref"), "{}", no_ref.message);
        let name = fails("[datasets.EEG]\nref = \"osf:abc12\"\n");
        assert!(
            name.help
                .as_deref()
                .is_some_and(|help| help.contains("\"eeg\""))
        );
        for bad in [
            "[datasets.x]\nref = 3\n",
            "[datasets.x]\nref = \"a\"\ncopy = \"yes\"\n",
            "[datasets.x]\nref = \"a\"\nselect = [\"[abc\"]\n",
            "[datasets.x]\nref = \"a\"\npath = \"../outside\"\n",
            "[project]\ndata_dir = \"/abs\"\n",
            "[datasets.x\nref = \"a\"\n",
            "[datasets.x]\nref = \"\"\n",
        ] {
            assert_eq!(fails(bad).kind, Kind::ProjectInvalid, "{bad}");
        }
    }

    const METADATA_COMPONENTS: &[&str] = &[
        ".git",
        ".GIT",
        ".hg",
        ".HG",
        ".svn",
        ".SVN",
        ".fetchloom",
        ".FETCHLOOM",
        ".git.",
        ".git ",
        ".git. ",
        ".hg.",
        ".hg ",
        ".svn.",
        ".svn ",
        ".fetchloom.",
        ".fetchloom ",
        "git~1",
        "GIT~1",
        "hg~1",
        "svn~1",
        "fetchl~1",
        ".git:stream",
        ".git::$INDEX_ALLOCATION",
        ".hg:stream",
        ".svn:stream",
        ".fetchloom:stream",
        ".GiT. :stream",
        "git~1:stream",
    ];

    fn refuses_metadata(key: &str, table: &str) {
        for path in [".github", ".gitignore", "data/git", "data/.hgdata"] {
            let text = format!("{table}\n{key} = \"{path}\"\n");
            assert!(
                Manifest::parse(&text).is_ok(),
                "refused data folder: {path}"
            );
        }
        for component in METADATA_COMPONENTS {
            for path in [(*component).to_owned(), format!("nested/{component}/files")] {
                let text = format!("{table}\n{key} = \"{path}\"\n");
                let result = Manifest::parse(&text);
                assert!(result.is_err(), "accepted metadata {key}: {path:?}");
                let error = result.unwrap_err();
                assert_eq!(error.kind, Kind::ProjectInvalid);
                assert!(error.message.contains(path.trim()), "{}", error.message);
                assert!(error.message.contains("metadata"), "{}", error.message);
                assert!(error.help.is_some());
            }
        }
    }

    #[test]
    fn metadata_data_dirs_are_refused() {
        refuses_metadata("data_dir", "[project]");
    }

    #[test]
    fn metadata_dataset_paths_are_refused() {
        refuses_metadata("path", "[datasets.x]\nref = \"https://example.org/x\"");
    }

    /// data.toml comes with a cloned repository, so fetchloom never expands `~` or a variable in
    /// it, and a folder that only makes sense expanded is refused rather than made under its
    /// literal name.
    #[test]
    fn folders_are_taken_as_written_so_shell_shorthand_is_refused() {
        for (key, table, inside) in [
            ("data_dir", "[project]", "the project"),
            (
                "path",
                "[datasets.x]\nref = \"https://example.org/x\"",
                "the data folder",
            ),
        ] {
            for folder in ["data~old", "price$", "a$1", "notes/~drafts"] {
                let text = format!("{table}\n{key} = \"{folder}\"\n");
                assert!(Manifest::parse(&text).is_ok(), "{key} = {folder}");
            }
            for folder in [
                "~/scratch/data",
                "~",
                "~ada/data",
                "$SCRATCH/data",
                "${SCRATCH}/data",
                "$1",
                "inputs/$USER",
                "inputs/${RUN}/raw",
                "runs/$_last",
            ] {
                let error = fails(&format!("{table}\n{key} = \"{folder}\"\n"));
                assert_eq!(error.kind, Kind::ProjectInvalid, "{key} = {folder}");
                assert_eq!(
                    error.message,
                    format!(
                        "{key} = \"{folder}\" in data.toml is refused because fetchloom does not expand ~ or $NAME, and the folder is always inside {inside}"
                    )
                );
                assert_eq!(
                    error.details,
                    [format!(
                        "data.toml:{}   {key} = \"{folder}\"",
                        table.lines().count() + 1
                    )]
                );
                assert_eq!(
                    error.help.as_deref(),
                    Some(
                        "to keep data on a scratch disk, set FETCHLOOM_CACHE to a folder there, or move the whole project there. see https://nikhil-yn-aix.github.io/fetchloom/user/hpc.html"
                    )
                );
            }
        }
    }

    #[test]
    fn a_link_to_one_file_selects_that_file_unless_select_is_given() {
        let manifest = Manifest::parse(
            "[datasets.one]\nref = \"https://huggingface.co/datasets/acme/data/blob/main/a[1].csv\"\n\n[datasets.chosen]\nref = \"https://huggingface.co/datasets/acme/data/blob/main/a.csv\"\nselect = \"*.json\"\n",
        )
        .unwrap();
        let select = |name: &str| {
            manifest.datasets[name]
                .source()
                .unwrap()
                .select
                .iter()
                .map(|glob| glob.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(select("one"), ["a[[]1].csv"]);
        assert_eq!(select("chosen"), ["*.json"]);
    }

    #[test]
    fn a_link_to_one_folder_selects_that_folder_unless_select_is_given() {
        let manifest = Manifest::parse(
            "[datasets.folder]\nref = \"https://huggingface.co/datasets/acme/data/tree/main/sample/10BT\"\n\n[datasets.chosen]\nref = \"https://huggingface.co/datasets/acme/data/tree/main/sample\"\nselect = \"*.json\"\n\n[datasets.whole]\nref = \"https://huggingface.co/datasets/acme/data/tree/main\"\n",
        )
        .unwrap();
        let select = |name: &str| {
            manifest.datasets[name]
                .source()
                .unwrap()
                .select
                .iter()
                .map(|glob| glob.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(select("folder"), ["sample/10BT/**"]);
        assert_eq!(select("chosen"), ["*.json"]);
        assert_eq!(select("whole"), Vec::<String>::new());
    }

    #[test]
    fn a_ref_with_a_line_break_is_refused_without_showing_it() {
        let error = Manifest::parse(
            "[datasets.data]\nref = \"https://example.org/data?token=abc\\nselect=*\"\n",
        )
        .unwrap_err();
        assert!(error.message.contains("control character"), "{error:?}");
        assert_eq!(error.details, ["data.toml:2"]);
        assert!(!format!("{error:?}").contains("abc"));
    }

    #[test]
    fn reference_help_masks_query_secrets() {
        let error = with_dataset(
            "datasets = {}\n",
            "data",
            "https://example.org/data?token=abc",
            &[],
            &[],
            true,
        )
        .unwrap_err();
        let help = error.help.unwrap();
        assert!(
            help.contains("token=***"),
            "manifest help exposed its query: {help}"
        );
        assert!(!help.contains("abc"), "{help}");
    }

    #[test]
    fn adding_keeps_comments_and_order() {
        let text = "# my data\n[project]\ndata_dir = \"data\" # here\n\n[datasets.b]\nref = \"zenodo:2\"\n";
        let added = with_dataset(text, "a", "zenodo:1", &["*.csv".to_owned()], &[], true).unwrap();
        assert_eq!(
            added,
            "# my data\n[project]\ndata_dir = \"data\" # here\n\n[datasets.a]\nref = \"zenodo:1\"\nselect = [\"*.csv\"]\n\n[datasets.b]\nref = \"zenodo:2\"\n"
        );
        assert_eq!(parse(&added).datasets.len(), 2);
        let fresh = with_dataset("", "iris", "zenodo:3608990", &[], &[], true).unwrap();
        assert_eq!(fresh, "[datasets.iris]\nref = \"zenodo:3608990\"\n");
    }

    /// a new dataset goes before the one whose name follows it, so two people adding different
    /// datasets rarely change the same lines of data.toml.
    #[test]
    fn add_inserts_in_sorted_order() {
        let add = |text: &str, name: &str| {
            with_dataset(text, name, &format!("zenodo:{name}"), &[], &[], true).unwrap()
        };
        let three = "[datasets.alpha]\nref = \"zenodo:alpha\"\n\n[datasets.core]\nref = \"zenodo:core\"\n\n[datasets.delta]\nref = \"zenodo:delta\"\n";
        assert_eq!(
            add(three, "beta"),
            "[datasets.alpha]\nref = \"zenodo:alpha\"\n\n[datasets.beta]\nref = \"zenodo:beta\"\n\n[datasets.core]\nref = \"zenodo:core\"\n\n[datasets.delta]\nref = \"zenodo:delta\"\n"
        );
        assert_eq!(
            add(three, "echo"),
            format!("{three}\n[datasets.echo]\nref = \"zenodo:echo\"\n"),
            "the last name still goes last"
        );
        assert_eq!(
            add("[datasets.core]\nref = \"zenodo:core\"\n", "alpha"),
            "[datasets.alpha]\nref = \"zenodo:alpha\"\n\n[datasets.core]\nref = \"zenodo:core\"\n"
        );
        // a note at the top stays there, and a comment on a dataset stays on it
        assert_eq!(
            add(
                "# inputs\n\n# the core set\n[datasets.core]\nref = \"zenodo:core\"\n",
                "alpha"
            ),
            "# inputs\n\n[datasets.alpha]\nref = \"zenodo:alpha\"\n\n# the core set\n[datasets.core]\nref = \"zenodo:core\"\n"
        );
        let after_project =
            "[datasets.core]\nref = \"zenodo:core\"\n\n[project]\ndata_dir = \"in\"\n";
        assert_eq!(
            add(after_project, "delta"),
            "[datasets.core]\nref = \"zenodo:core\"\n\n[datasets.delta]\nref = \"zenodo:delta\"\n\n[project]\ndata_dir = \"in\"\n"
        );
    }

    /// a merge takes a dataset from the other side with the comments touching its header: in
    /// place of the one this side has, or in name order.
    #[test]
    fn a_dataset_from_the_other_side_keeps_its_comments_and_its_place() {
        let ours = "# inputs\n\n[datasets.alpha]\nref = \"zenodo:1\"\n\n[datasets.core]\nref = \"zenodo:3\"\n";
        let theirs = "[datasets.beta]\nref = \"zenodo:2\" # new\n\n# upgraded\n[datasets.core]\nref = \"zenodo:4\"\n";
        let with_beta = with_dataset_from(ours, theirs, "beta").unwrap();
        assert_eq!(
            with_beta,
            "# inputs\n\n[datasets.alpha]\nref = \"zenodo:1\"\n\n[datasets.beta]\nref = \"zenodo:2\" # new\n\n[datasets.core]\nref = \"zenodo:3\"\n"
        );
        assert_eq!(
            with_dataset_from(&with_beta, theirs, "core").unwrap(),
            "# inputs\n\n[datasets.alpha]\nref = \"zenodo:1\"\n\n[datasets.beta]\nref = \"zenodo:2\" # new\n\n# upgraded\n[datasets.core]\nref = \"zenodo:4\"\n"
        );
        assert_eq!(
            with_dataset_from("", "datasets = { x = { ref = \"zenodo:9\" } }\n", "x").unwrap(),
            "[datasets.x]\nref = \"zenodo:9\"\n"
        );
        assert!(with_dataset_from(ours, theirs, "missing").is_err());
    }

    #[test]
    fn each_dataset_reads_alone_and_the_rest_without_them() {
        let text = "# inputs\n\n[project]\ndata_dir = \"in\"\n\n# about alpha\n[datasets.alpha]\nref = \"zenodo:1\"\n\n[datasets.beta]\nref = \"zenodo:2\"\n";
        let tables = tables(text).unwrap();
        assert_eq!(
            tables.into_iter().collect::<Vec<_>>(),
            [
                (
                    "alpha".to_owned(),
                    "# about alpha\n[datasets.alpha]\nref = \"zenodo:1\"\n".to_owned()
                ),
                (
                    "beta".to_owned(),
                    "[datasets.beta]\nref = \"zenodo:2\"\n".to_owned()
                ),
            ]
        );
        assert_eq!(
            outside(text).unwrap(),
            "# inputs\n[project]\ndata_dir = \"in\""
        );
        assert_eq!(
            outside(&text.replace("\n\n[datasets.beta]", "\n[datasets.beta]")).unwrap(),
            outside(text).unwrap(),
            "blank lines do not count"
        );
    }

    /// drop adds each pattern once, under the dataset's own header or one of its own for the
    /// archive, and leaves every other line as written.
    #[test]
    fn an_exclusion_is_added_once_keeping_comments() {
        let text = "# pets for the study\n[datasets.pets]\nref = \"https://example.org/pets.zip\" # v1\n\n[datasets.zoo]\nref = \"zenodo:1\"\nexclude = \"*.tmp\" # scratch\n";
        let dog = "pets-v1/images/dog_010.png".to_owned();
        let blurry = "pets-v1/images/blurry_011.png".to_owned();
        let once =
            with_exclusions(text, "pets", Some("pets.zip"), std::slice::from_ref(&dog)).unwrap();
        assert_eq!(
            once,
            "# pets for the study\n[datasets.pets]\nref = \"https://example.org/pets.zip\" # v1\n\n[datasets.pets.members.\"pets.zip\"]\nexclude = [\"pets-v1/images/dog_010.png\"]\n\n[datasets.zoo]\nref = \"zenodo:1\"\nexclude = \"*.tmp\" # scratch\n"
        );
        let twice =
            with_exclusions(&once, "pets", Some("pets.zip"), &[blurry.clone(), dog]).unwrap();
        assert_eq!(
            twice,
            "# pets for the study\n[datasets.pets]\nref = \"https://example.org/pets.zip\" # v1\n\n[datasets.pets.members.\"pets.zip\"]\nexclude = [\n  \"pets-v1/images/dog_010.png\",\n  \"pets-v1/images/blurry_011.png\",\n]\n\n[datasets.zoo]\nref = \"zenodo:1\"\nexclude = \"*.tmp\" # scratch\n"
        );
        let read = parse(&twice);
        let members = &read.datasets["pets"].source().unwrap().members;
        let zip = members.get("pets.zip").unwrap();
        assert!(!zip.keeps(&blurry) && zip.keeps("pets-v1/images/cat.png"));
        let listed = with_exclusions(text, "zoo", None, &["b[[]1].csv".to_owned()]).unwrap();
        assert!(
            listed.ends_with(
                "[datasets.zoo]\nref = \"zenodo:1\"\nexclude = [\n  \"*.tmp\",\n  \"b[[]1].csv\",\n] # scratch\n"
            ),
            "{listed}"
        );
        assert_eq!(without_members(&twice, "pets", "pets.zip").unwrap(), text);
        for inline in [
            "datasets.pets = { ref = \"x\" }\n",
            "[datasets.pets]\nref = \"x\"\nmembers = { \"pets.zip\" = { exclude = \"a\" } }\n",
        ] {
            let error = with_exclusions(
                inline,
                "pets",
                Some("pets.zip"),
                std::slice::from_ref(&blurry),
            )
            .unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid, "{inline}");
            assert!(error.message.contains("inline"), "{}", error.message);
        }
    }

    #[test]
    fn removing_keeps_everything_else() {
        let text = "# my thesis data\n\n# the classic one\n[datasets.a]\nref = \"zenodo:1\" # first\n\n[datasets.b]\nref = \"zenodo:2\"\n";
        assert_eq!(
            without_dataset(text, "a").unwrap(),
            "# my thesis data\n\n[datasets.b]\nref = \"zenodo:2\"\n"
        );
        assert_eq!(
            without_dataset(text, "b").unwrap(),
            "# my thesis data\n\n# the classic one\n[datasets.a]\nref = \"zenodo:1\" # first\n"
        );
        assert_eq!(
            without_dataset("[datasets.a]\nref = \"x\"\n", "a").unwrap(),
            ""
        );
        assert_eq!(
            without_dataset("# only\n\n[datasets.a]\nref = \"x\"\n", "a").unwrap(),
            "# only\n"
        );
    }

    /// what data.toml states: the data folder, and each dataset's values without their lines.
    type Stated = (String, BTreeMap<String, Values>);
    type Values = (Option<Fetched>, Option<Made>, bool, Option<String>);
    /// a source's ref, select, exclude, extract and member filters.
    type Fetched = (String, Vec<String>, Vec<String>, bool, Vec<Filtered>);
    /// an archive's path, and the select and exclude patterns of its members table.
    type Filtered = (String, Vec<String>, Vec<String>);
    /// a recipe's from, run, code and env.
    type Made = (
        Vec<String>,
        Vec<String>,
        Vec<String>,
        BTreeMap<String, String>,
    );

    fn patterns(globs: &[Glob]) -> Vec<String> {
        globs.iter().map(|glob| glob.as_str().to_owned()).collect()
    }

    fn stated(manifest: &Manifest) -> Stated {
        let datasets = manifest.datasets.iter().map(|(dataset, entry)| {
            let fetched = entry.source().map(|source| {
                (
                    source.reference.clone(),
                    patterns(&source.select),
                    patterns(&source.exclude),
                    source.extract,
                    source
                        .members
                        .iter()
                        .map(|(archive, filter)| {
                            (
                                archive.clone(),
                                patterns(filter.select.globs()),
                                patterns(filter.exclude.globs()),
                            )
                        })
                        .collect(),
                )
            });
            let made = entry.recipe().map(|recipe| {
                (
                    recipe.from.clone(),
                    recipe
                        .run
                        .iter()
                        .map(|word| word.text().to_owned())
                        .collect(),
                    recipe.code.clone(),
                    recipe.env.clone(),
                )
            });
            (
                dataset.clone(),
                (fetched, made, entry.copy, entry.path.clone()),
            )
        });
        (manifest.data_dir.clone(), datasets.collect())
    }

    /// any text: what reads is valid, and removing or adding a dataset changes only that
    /// dataset. each dataset is added again under another name with its own values, so the
    /// writer is given every string the reader takes.
    #[test]
    fn fuzz_manifest() {
        bolero::check!().for_each(|bytes| {
            let Ok(text) = std::str::from_utf8(bytes) else {
                return;
            };
            let Ok(manifest) = Manifest::parse(text) else {
                return;
            };
            assert_eq!(name::escapes(&manifest.data_dir), None);
            assert_eq!(name::metadata(&manifest.data_dir), None);
            let before = stated(&manifest);
            for (dataset, entry) in &manifest.datasets {
                assert!(name::is_dataset_name(dataset), "{dataset:?}");
                if let Some(source) = entry.source() {
                    assert!(!source.reference.is_empty(), "{dataset:?}");
                    assert_eq!(source.reference.trim(), source.reference);
                }
                if let Some(path) = entry
                    .path
                    .as_deref()
                    .filter(|path| *path != DATA_FOLDER_ITSELF)
                {
                    assert_eq!(name::escapes(path), None, "{path:?}");
                    assert_eq!(name::metadata(path), None, "{path:?}");
                }

                let removed = without_dataset(text, dataset).unwrap();
                let mut expected = before.clone();
                expected.1.remove(dataset);
                let after = Manifest::parse(&removed).map(|manifest| stated(&manifest));
                // a recipe cannot read a dataset data.toml no longer has
                let read = manifest.datasets.values().any(|entry| {
                    entry
                        .recipe()
                        .is_some_and(|recipe| recipe.from.contains(dataset))
                });
                if read {
                    assert!(after.is_err(), "{removed:?}");
                } else {
                    assert_eq!(after.ok(), Some(expected), "{removed:?}");
                }

                let copy = format!("{dataset}-2");
                let Some(source) = entry.source() else {
                    continue;
                };
                if !name::is_dataset_name(&copy) || manifest.datasets.contains_key(&copy) {
                    continue;
                }
                let added = match with_dataset(
                    text,
                    &copy,
                    &source.reference,
                    &patterns(&source.select),
                    &patterns(&source.exclude),
                    source.extract,
                ) {
                    Ok(added) => added,
                    Err(err) => {
                        assert!(err.message.contains("inline"), "{}", err.message);
                        continue;
                    }
                };
                let mut expected = before.clone();
                let (fetched, _, _, _) = before.1[dataset].clone();
                // a dataset is added without member filters, which only a person writes
                let fetched = fetched.map(|(reference, select, exclude, extract, _)| {
                    (reference, select, exclude, extract, Vec::new())
                });
                expected.1.insert(copy, (fetched, None, false, None));
                let after = Manifest::parse(&added).map(|manifest| stated(&manifest));
                assert_eq!(after.ok(), Some(expected), "{added:?}");
            }
        });
    }

    const RAW: &str = "[datasets.raw]\nref = \"zenodo:1\"\n\n";
    const CLEAN: &str = "[datasets.clean]\nfrom = [\"raw\"]\nrun = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]\n";

    /// a recipe makes a dataset from others with a command, so it needs both, and a dataset is
    /// fetched from a source or made by a recipe, never both.
    #[test]
    fn a_recipe_needs_from_and_run_and_no_ref() {
        let manifest = parse(&format!(
            "{RAW}{CLEAN}code = [\"clean.py\", \"lib/\"]\nenv = {{ SEED = \"1\" }}\ncopy = true\n"
        ));
        let entry = &manifest.datasets["clean"];
        let recipe = entry.recipe().unwrap();
        assert_eq!(recipe.from, ["raw"]);
        let words: Vec<&str> = recipe.run.iter().map(Template::text).collect();
        assert_eq!(words, ["python3", "clean.py", "{raw}", "{out}"]);
        assert_eq!(recipe.code, ["clean.py", "lib"]);
        assert_eq!(
            recipe.env,
            BTreeMap::from([("SEED".to_owned(), "1".to_owned())])
        );
        assert!(entry.copy && entry.source().is_none());
        assert!(manifest.datasets["raw"].recipe().is_none());
        let run = "run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]\n";
        for (table, message, help) in [
            (
                "from = [\"raw\"]\n".to_owned(),
                "datasets.clean has from but no run, so nothing makes it",
                "add run, the program and arguments that make it, as run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]",
            ),
            (
                run.to_owned(),
                "datasets.clean has run but no from, so it reads no dataset",
                "add from with the datasets run reads, as from = [\"raw\"]",
            ),
            (
                format!("ref = \"zenodo:2\"\nfrom = [\"raw\"]\n{run}"),
                "datasets.clean has both ref and from, but a dataset is fetched from a source or made by a recipe, not both",
                "keep ref to fetch it, or from and run to make it from other datasets",
            ),
            (
                "ref = \"zenodo:2\"\nenv = { SEED = \"1\" }\n".to_owned(),
                "datasets.clean has both ref and env, but a dataset is fetched from a source or made by a recipe, not both",
                "keep ref to fetch it, or from and run to make it from other datasets",
            ),
            (
                format!("from = [\"raw\"]\n{run}select = \"*.csv\"\n"),
                "select is for a dataset fetched from a source, and datasets.clean is made by a recipe",
                "remove select, and choose files inside the step instead",
            ),
            (
                format!("from = [\"raw\"]\n{run}extract = false\n"),
                "extract is for a dataset fetched from a source, and datasets.clean is made by a recipe",
                "remove extract, and choose files inside the step instead",
            ),
            (
                format!("from = []\n{run}"),
                "datasets.clean is made from no dataset",
                "name the datasets run reads in from, as from = [\"raw\"]",
            ),
            (
                format!("from = \"raw\"\n{run}"),
                "from in data.toml must be a list of dataset names",
                "write it as from = [\"raw\"]",
            ),
            (
                format!("from = [\"raw\", \"raw\"]\n{run}"),
                "datasets.clean is made from raw twice",
                "name each dataset once in from",
            ),
            (
                "from = [\"raw\"]\nrun = \"python3 clean.py {raw} {out}\"\n".to_owned(),
                "run in data.toml must be a list of words, since no shell reads it",
                "write the program and each argument as a word of their own, as run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]",
            ),
            (
                "from = [\"raw\"]\nrun = []\n".to_owned(),
                "run in datasets.clean is empty, so there is no program to start",
                "write the program and each argument as a word of their own, as run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]",
            ),
            (
                format!("from = [\"raw\"]\n{run}code = \"clean.py\"\n"),
                "code in data.toml must be a list of files and folders in the project",
                "write it as code = [\"clean.py\"]",
            ),
            (
                format!("from = [\"raw\"]\n{run}env = {{ SEED = 1 }}\n"),
                "every value in env must be a string in quotes",
                "write it as SEED = \"1\"",
            ),
        ] {
            let error = fails(&format!("{RAW}[datasets.clean]\n{table}"));
            assert_eq!(error.kind, Kind::ProjectInvalid, "{table}");
            assert_eq!(error.message, message, "{table}");
            assert_eq!(error.help.as_deref(), Some(help), "{table}");
            assert_eq!(error.details.len(), 1, "{table}: {:?}", error.details);
            assert!(error.details[0].starts_with("data.toml:"), "{table}");
        }
    }

    /// `place = false` keeps a dataset pinned and cached but out of the data folder, whether it
    /// is fetched or made, and only true or false is read.
    #[test]
    fn place_reads_on_both_kinds() {
        let manifest = parse(&format!(
            "{RAW}place = false\n\n{CLEAN}place = false\n\n[datasets.kept]\nref = \"zenodo:2\"\n"
        ));
        assert!(!manifest.datasets["raw"].place);
        assert!(!manifest.datasets["clean"].place);
        assert!(
            manifest.datasets["kept"].place,
            "placed unless data.toml says not"
        );
        assert_eq!(
            manifest.datasets["raw"].lines["place"],
            "data.toml:4   place = false"
        );
        let error = fails(&format!("{RAW}place = \"no\"\n"));
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(error.message, "place in data.toml must be true or false");
        assert_eq!(error.details, ["data.toml:4   place = \"no\""]);
    }

    /// each word of run is text with `{name}` for the folder of a dataset it reads and `{out}`
    /// for the result's, so a word naming anything else, or a step that cannot find an input
    /// or write its result, is refused before anything runs.
    #[test]
    fn templates_name_inputs_and_out() {
        let recipe = |run: &str| format!("{RAW}[datasets.clean]\nfrom = [\"raw\"]\nrun = {run}\n");
        let manifest = parse(&recipe(
            "[\"tool\", \"--in={raw}/x\", \"{{literal}}\", \"{out}\"]",
        ));
        assert_eq!(manifest.datasets["clean"].recipe().unwrap().run.len(), 4);
        for (run, message, help) in [
            (
                "[\"python3\", \"clean.py\", \"{raw}\"]",
                "run in datasets.clean never names {out}, so the step has nowhere to write",
                "put {out} where the step takes the folder to write the result in",
            ),
            (
                "[\"python3\", \"clean.py\", \"{out}\"]",
                "run in datasets.clean never names {raw}, so the step cannot find raw",
                "put {raw} where the step takes the folder holding raw's files",
            ),
            (
                "[\"python3\", \"{rwa}\", \"{raw}\", \"{out}\"]",
                "run in datasets.clean names {rwa}, which is not a dataset it is made from",
                "did you mean {raw}? a name in braces is one of from, or out",
            ),
            (
                "[\"python3\", \"{Raw}\", \"{raw}\", \"{out}\"]",
                "run in datasets.clean names {Raw}, which is not a dataset it is made from",
                "did you mean {raw}? a name in braces is one of from, or out",
            ),
            (
                "[\"python3\", \"{raw\", \"{out}\"]",
                "a word of run in datasets.clean has a { with no } after it",
                "write {{ and }} for a brace of its own, and {name} for the folder of a dataset it reads",
            ),
            (
                "[\"\", \"{raw}\", \"{out}\"]",
                "the program in run of datasets.clean is empty",
                "make the first word the program to start, as run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]",
            ),
            (
                "[\"python3\\n\", \"{raw}\", \"{out}\"]",
                "a word of run in datasets.clean holds a control character",
                "write {{ and }} for a brace of its own, and {name} for the folder of a dataset it reads",
            ),
        ] {
            let error = fails(&recipe(run));
            assert_eq!(error.kind, Kind::ProjectInvalid, "{run}");
            assert_eq!(error.message, message, "{run}");
            assert_eq!(error.help.as_deref(), Some(help), "{run}");
            assert_eq!(error.details.len(), 1, "{run}");
            assert!(error.details[0].starts_with("data.toml:6"), "{run}");
        }
        let out = fails(
            "[datasets.out]\nref = \"zenodo:1\"\n\n[datasets.clean]\nfrom = [\"out\"]\nrun = [\"tool\", \"{out}\"]\n",
        );
        assert_eq!(
            out.message,
            "datasets.clean is made from out, the name run keeps for the result's folder"
        );
        assert_eq!(
            out.help.as_deref(),
            Some("rename the dataset out in data.toml")
        );
    }

    /// a recipe reads datasets data.toml has, and recipes cannot be made from each other, since
    /// no order of steps could make them. a cycle names its path.
    #[test]
    fn from_names_exist_and_form_no_cycle() {
        let recipe = |name: &str, from: &[&str]| {
            let names: Vec<String> = from.iter().map(|input| format!("\"{input}\"")).collect();
            let folders: Vec<String> = from
                .iter()
                .map(|input| format!("\"{{{input}}}\""))
                .collect();
            format!(
                "\n[datasets.{name}]\nfrom = [{}]\nrun = [\"tool\", {}, \"{{out}}\"]\n",
                names.join(", "),
                folders.join(", ")
            )
        };
        let chain = format!("{RAW}{}{}", recipe("b", &["a"]), recipe("a", &["raw"]));
        assert_eq!(parse(&chain).datasets.len(), 3);
        let typo = fails(&format!("{RAW}{}", recipe("clean", &["rwa"])));
        assert_eq!(typo.kind, Kind::ProjectInvalid);
        assert_eq!(
            typo.message,
            "datasets.clean is made from rwa, which data.toml does not have"
        );
        assert_eq!(typo.help.as_deref(), Some("did you mean raw?"));
        assert_eq!(typo.details, ["data.toml:6   from = [\"rwa\"]"]);
        for (text, message) in [
            (
                format!("{RAW}{}{}", recipe("a", &["b"]), recipe("b", &["a"])),
                "datasets a and b are made from each other: a \u{2192} b \u{2192} a",
            ),
            (
                format!(
                    "{RAW}{}{}{}",
                    recipe("c", &["raw", "a"]),
                    recipe("b", &["c"]),
                    recipe("a", &["b"])
                ),
                "datasets a, b and c are made from each other: a \u{2192} b \u{2192} c \u{2192} a",
            ),
            (
                format!("{RAW}{}", recipe("a", &["a"])),
                "datasets.a is made from itself",
            ),
        ] {
            let error = fails(&text);
            assert_eq!(error.kind, Kind::ProjectInvalid, "{text}");
            assert_eq!(error.message, message, "{text}");
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "a recipe can read only datasets made before it; take one of them out of the other's from"
                ),
            );
            assert_eq!(error.details.len(), 1, "{text}");
        }
    }

    /// code names files and folders the step reads, whose bytes enter the key, so each stays
    /// inside the project and out of what fetchloom writes: the data folder, data.lock and
    /// data.lock.d, and repository metadata.
    #[test]
    fn code_stays_in_the_project_and_out_of_the_data_folder() {
        let recipe = |data_dir: &str, code: &str| {
            format!("[project]\ndata_dir = \"{data_dir}\"\n\n{RAW}{CLEAN}code = [{code}]\n")
        };
        let manifest = parse(&recipe(
            "data",
            "\"clean.py\", \"lib/\", \"data.toml\", \"database\"",
        ));
        assert_eq!(
            manifest.datasets["clean"].recipe().unwrap().code,
            ["clean.py", "lib", "data.toml", "database"]
        );
        for (data_dir, code, message, help) in [
            (
                "data",
                "\"../shared/clean.py\"",
                "code \"../shared/clean.py\" in datasets.clean is refused because it climbs out of its folder",
                "name a file or folder inside the project, with / between folders",
            ),
            (
                "data",
                "\"/etc/passwd\"",
                "code \"/etc/passwd\" in datasets.clean is refused because it starts at the root of the disk",
                "name a file or folder inside the project, with / between folders",
            ),
            (
                "data",
                "\"lib\\\\clean.py\"",
                "code \"lib\\clean.py\" in datasets.clean is refused because it has a \\, which windows reads as a folder separator",
                "name a file or folder inside the project, with / between folders",
            ),
            (
                "data",
                "\".\"",
                "code \".\" in datasets.clean is refused because it has . as a folder name",
                "name a file or folder inside the project, with / between folders",
            ),
            (
                "data",
                "\".git/hooks\"",
                "code \".git/hooks\" in datasets.clean is refused because it names repository metadata",
                "name the project's own files, such as scripts and their settings",
            ),
            (
                "data",
                "\"Data.Lock\"",
                "code \"Data.Lock\" in datasets.clean is refused because fetchloom writes it",
                "name the project's own files, such as scripts and their settings",
            ),
            (
                "data",
                "\"data.lock.d/clean.tsv.zst\"",
                "code \"data.lock.d/clean.tsv.zst\" in datasets.clean is refused because fetchloom writes it",
                "name the project's own files, such as scripts and their settings",
            ),
            (
                "data",
                "\"data/raw/x.csv\"",
                "code \"data/raw/x.csv\" in datasets.clean is refused because it is inside the data folder, which fetchloom fills",
                "name the project's own files, such as scripts and their settings",
            ),
            (
                "work/data",
                "\"WORK\"",
                "code \"WORK\" in datasets.clean is refused because it holds the data folder, which fetchloom fills",
                "name the project's own files, such as scripts and their settings",
            ),
            (
                "data",
                "\"clean.py\", \"clean.py/\"",
                "datasets.clean names the code \"clean.py\" twice",
                "name each file or folder once in code",
            ),
        ] {
            let error = fails(&recipe(data_dir, code));
            assert_eq!(error.kind, Kind::ProjectInvalid, "{code}");
            assert_eq!(error.message, message, "{code}");
            assert_eq!(error.help.as_deref(), Some(help), "{code}");
            assert_eq!(error.details.len(), 1, "{code}");
            assert!(error.details[0].starts_with("data.toml:10"), "{code}");
        }
    }

    /// data.toml and data.lock are shared, and the step inherits the environment of whoever
    /// builds, so env sets plain settings: never fetchloom's own variables or a secret.
    #[test]
    fn env_refuses_fetchloom_and_secret_names() {
        let recipe = |env: &str| format!("{RAW}{CLEAN}env = {{ {env} }}\n");
        let manifest = parse(&recipe(
            "SEED = \"1\", OMP_NUM_THREADS = \"4\", http_proxy = \"\", TOKENIZERS_PARALLELISM = \"false\", MAX_TOKENS = \"512\"",
        ));
        assert_eq!(manifest.datasets["clean"].recipe().unwrap().env.len(), 5);
        for (env, message, help) in [
            (
                "FETCHLOOM_CACHE = \"/tmp/c\"",
                "env in datasets.clean sets FETCHLOOM_CACHE, one of fetchloom's own variables",
                "set fetchloom's variables in your shell or its settings file, never in data.toml",
            ),
            (
                "fetchloom_jobs = \"1\"",
                "env in datasets.clean sets fetchloom_jobs, one of fetchloom's own variables",
                "set fetchloom's variables in your shell or its settings file, never in data.toml",
            ),
            (
                "HF_TOKEN = \"hf_abc\"",
                "env in datasets.clean sets HF_TOKEN, which looks like a secret",
                "data.toml and data.lock are shared, and the step inherits your environment, so set HF_TOKEN there",
            ),
            (
                "aws_secret_access_key = \"x\"",
                "env in datasets.clean sets aws_secret_access_key, which looks like a secret",
                "data.toml and data.lock are shared, and the step inherits your environment, so set aws_secret_access_key there",
            ),
            (
                "DB_PASSWD = \"x\"",
                "env in datasets.clean sets DB_PASSWD, which looks like a secret",
                "data.toml and data.lock are shared, and the step inherits your environment, so set DB_PASSWD there",
            ),
            (
                "OPENAI_API_KEY = \"x\"",
                "env in datasets.clean sets OPENAI_API_KEY, which looks like a secret",
                "data.toml and data.lock are shared, and the step inherits your environment, so set OPENAI_API_KEY there",
            ),
            (
                "GITHUBTOKEN = \"x\"",
                "env in datasets.clean sets GITHUBTOKEN, which looks like a secret",
                "data.toml and data.lock are shared, and the step inherits your environment, so set GITHUBTOKEN there",
            ),
            (
                "\"MY VAR\" = \"1\"",
                "env in datasets.clean sets \"MY VAR\", which is not a variable name every system takes",
                "name variables with letters, digits and _, not starting with a digit",
            ),
            (
                "\"1SEED\" = \"1\"",
                "env in datasets.clean sets \"1SEED\", which is not a variable name every system takes",
                "name variables with letters, digits and _, not starting with a digit",
            ),
            (
                "SEED = \"a\\u0000b\"",
                "env in datasets.clean gives SEED a value with a NUL character, which no program can be given",
                "remove the NUL character from the value",
            ),
        ] {
            let error = fails(&recipe(env));
            assert_eq!(error.kind, Kind::ProjectInvalid, "{env}");
            assert_eq!(error.message, message, "{env}");
            assert_eq!(error.help.as_deref(), Some(help), "{env}");
            assert_eq!(error.details.len(), 1, "{env}");
            assert!(error.details[0].starts_with("data.toml:7"), "{env}");
            assert!(!error.details[0].contains("hf_abc"), "{env}");
        }
    }

    /// git for windows checks data.toml out with CRLF line endings, which a multi-line string
    /// keeps, so a variable reads with LF on every system and keys alike.
    #[test]
    fn a_multi_line_variable_reads_alike_in_a_crlf_checkout() {
        let text =
            format!("{RAW}{CLEAN}env = {{ HEADER = \"\"\"id\nname\"\"\", TAB = \"a\\tb\" }}\n");
        for written in [text.clone(), text.replace('\n', "\r\n")] {
            let manifest = parse(&written);
            let env = &manifest.datasets["clean"].recipe().unwrap().env;
            assert_eq!(env["HEADER"], "id\nname", "{written:?}");
            assert_eq!(env["TAB"], "a\tb");
        }
    }

    #[test]
    fn unknown_keys_are_rejected_at_each_table_level() {
        for (text, citation, help) in [
            (
                "datsets = {}\n",
                "data.toml:1   datsets = {}",
                "did you mean `datasets`?",
            ),
            (
                "[project]\ndata_dr = 'inputs'\n",
                "data.toml:2   data_dr = 'inputs'",
                "did you mean `data_dir`?",
            ),
            (
                "[datasets.x]\nref = 'zenodo:1'\nunknown_setting = true\n",
                "data.toml:3   unknown_setting = true",
                "the keys here are ref, select, exclude, extract, members, copy, path, place, from, run, code, env",
            ),
            (
                "[datasets.x]\nref = 'zenodo:1'\n[datasets.x.members.'x.zip']\nexculde = ['a/b.csv']\n",
                "data.toml:4   exculde = ['a/b.csv']",
                "did you mean `exclude`?",
            ),
        ] {
            let error = fails(text);
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert_eq!(error.details, [citation]);
            assert_eq!(error.help.as_deref(), Some(help));
        }
    }

    #[test]
    fn wrong_table_and_pattern_types_point_to_the_bad_value() {
        for (text, message, line) in [
            ("project = 7\n", "must be a table", "data.toml:1"),
            ("datasets = []\n", "must be a table", "data.toml:1"),
            (
                "[datasets]\nx = false\n",
                "must be a table with a ref",
                "data.toml:2",
            ),
            (
                "[datasets.x]\nref = 'zenodo:1'\nselect = 1\n",
                "must be a pattern or a list",
                "data.toml:3",
            ),
            (
                "[datasets.x]\nref = 'zenodo:1'\nexclude = ['*.tmp', 7]\n",
                "every pattern in exclude must be a string",
                "data.toml:3",
            ),
            (
                "[datasets.x]\nref = 'zenodo:1'\nextract = 'yes'\n",
                "must be true or false",
                "data.toml:3",
            ),
            (
                "[project]\ndata_dir = false\n",
                "must be a string",
                "data.toml:2",
            ),
        ] {
            let error = fails(text);
            assert_eq!(error.kind, Kind::ProjectInvalid, "{text}");
            assert!(error.message.contains(message), "{}", error.message);
            assert_eq!(error.details.len(), 1);
            assert!(error.details[0].starts_with(line), "{:?}", error.details);
        }
    }

    #[test]
    fn duplicate_datasets_and_duplicate_refs_are_not_silently_overwritten() {
        for text in [
            "[datasets.x]\nref = 'zenodo:1'\n[datasets.x]\nref = 'zenodo:2'\n",
            "[datasets.x]\nref = 'zenodo:1'\nref = 'zenodo:2'\n",
            "datasets = { x = { ref = 'zenodo:1' }, x = { ref = 'zenodo:2' } }\n",
        ] {
            let error = fails(text);
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert!(error.message.contains("not valid TOML"));
            assert_eq!(error.details.len(), 1);
            assert!(error.help.unwrap().contains("fix that line"));
        }
    }

    #[test]
    fn path_dot_means_the_data_folder_and_empty_is_refused() {
        let table = "[datasets.x]\nref = \"https://example.org/x\"\n";
        for written in [".", "./", " . "] {
            let manifest = parse(&format!("{table}path = \"{written}\"\n"));
            assert_eq!(
                manifest.datasets["x"].path.as_deref(),
                Some("."),
                "{written:?}"
            );
        }
        let empty = fails(&format!("{table}path = \"\"\n"));
        assert_eq!(
            empty.message,
            "path = \"\" in data.toml is refused because it is empty"
        );
        assert_eq!(
            empty.help.as_deref(),
            Some("use \".\" for the data folder itself")
        );
        for (written, reason) in [
            ("..", "it climbs out of its folder"),
            ("raw/../x", "it climbs out of its folder"),
            ("./raw", "it has . as a folder name"),
        ] {
            let error = fails(&format!("{table}path = \"{written}\"\n"));
            assert_eq!(
                error.message,
                format!("path = \"{written}\" in data.toml is refused because {reason}")
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "use a folder inside the data folder, with / between folders, or \".\" for the data folder itself"
                )
            );
        }
    }

    #[test]
    fn data_directories_must_stay_inside_the_project() {
        for directory in [
            "",
            " ",
            ".",
            "./",
            "../outside",
            "data/../outside",
            "/absolute",
            "C:/outside",
            r"data\files",
            "data//files",
        ] {
            let text = format!("[project]\ndata_dir = '{directory}'\n");
            let error = fails(&text);
            assert_eq!(error.kind, Kind::ProjectInvalid, "{directory}");
            assert!(error.message.contains("data_dir"));
            assert_eq!(
                error.details,
                [format!("data.toml:2   data_dir = '{directory}'")]
            );
            assert!(error.help.unwrap().contains("inside the project"));
        }
        assert_eq!(
            fails("[project]\ndata_dir = '.'\n").message,
            "data_dir = \".\" in data.toml is refused because it would mix data with the project's own files"
        );
        assert_eq!(parse("[project]\n").data_dir, "data");
        assert_eq!(
            parse("[project]\ndata_dir = '  inputs/raw///  '\n").data_dir,
            "inputs/raw"
        );
    }

    #[test]
    fn an_unusable_dataset_name_still_explains_how_to_rename_it() {
        let error = fails("[datasets.'!!!']\nref = 'zenodo:1'\n");
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert!(error.message.contains("not a valid dataset name"));
        assert!(error.help.unwrap().contains("lowercase letters"));
    }

    #[test]
    fn a_windows_device_name_is_refused_with_a_usable_one() {
        for device in ["con", "aux", "nul", "com1", "lpt1"] {
            let error = fails(&format!("[datasets.{device}]\nref = 'zenodo:1'\n"));
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert_eq!(
                error.message,
                format!("\"{device}\" is not a valid dataset name")
            );
            assert_eq!(
                error.details,
                [
                    format!("data.toml:1   [datasets.{device}]"),
                    "windows keeps it for a device, so it cannot be a folder there".to_owned()
                ]
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    format!(
                        "rename it to \"{device}-data\" in data.toml, then run the same command again"
                    )
                    .as_str()
                )
            );
        }
    }

    /// a members table left behind when its dataset's table was renamed is told as a rename to
    /// finish, naming the new table only when data.toml shows which one it is.
    #[test]
    fn members_left_by_a_rename_say_to_rename_them_too() {
        let members = "\n[datasets.glob.members.\"glob-000000.tar\"]\nexclude = ['a1.wav']\n";
        for (sounds, help) in [
            (
                "ref = 'https://example.org/v/glob-000000.tar?download=1'",
                "if glob is now sounds, rename its members tables too, as [datasets.sounds.members.\"glob-000000.tar\"]",
            ),
            (
                "ref = 'zenodo:1'",
                "if glob was renamed, rename its members tables with it, as [datasets.<new name>.members.\"glob-000000.tar\"], or remove them",
            ),
        ] {
            let error = fails(&format!("[datasets.sounds]\n{sounds}\n{members}"));
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert_eq!(
                error.message,
                "datasets.glob in data.toml has members tables but no ref"
            );
            assert_eq!(
                error.details,
                ["data.toml:4   [datasets.glob.members.\"glob-000000.tar\"]"]
            );
            assert_eq!(error.help.as_deref(), Some(help));
        }
    }

    #[test]
    fn removing_and_readding_preserves_other_datasets_and_all_requested_options() {
        let text = "# research inputs\n[project]\ndata_dir = 'inputs'\n\n[datasets.keep]\nref = 'zenodo:2' # leave this\ncopy = true\npath = 'raw/keep'\n\n[datasets.replace]\nref = 'zenodo:1'\n";
        let removed = without_dataset(text, "replace").unwrap();
        let mut expected = stated(&parse(text));
        expected.1.remove("replace");
        assert_eq!(stated(&parse(&removed)), expected);
        assert!(removed.contains("ref = 'zenodo:2' # leave this"));
        let added = with_dataset(
            &removed,
            "replace",
            "zenodo:3",
            &["**/*.csv".to_owned()],
            &["**/old/**".to_owned()],
            false,
        )
        .unwrap();
        let manifest = parse(&added);
        let replacement = manifest.datasets["replace"].source().unwrap();
        assert_eq!(manifest.data_dir, "inputs");
        assert_eq!(manifest.datasets.len(), 2);
        assert_eq!(replacement.reference, "zenodo:3");
        assert_eq!(patterns(&replacement.select), ["**/*.csv"]);
        assert_eq!(patterns(&replacement.exclude), ["**/old/**"]);
        assert!(!replacement.extract);
        assert_eq!(
            stated(&parse(&without_dataset(&added, "replace").unwrap())),
            expected
        );
        assert_eq!(without_dataset(&added, "missing").unwrap(), added);
        assert_eq!(
            without_dataset("# no datasets\n[project]\n", "missing").unwrap(),
            "# no datasets\n[project]\n"
        );
    }

    #[test]
    fn removing_an_inline_entry_and_editing_bom_text_round_trip() {
        let text = "\u{feff}datasets = { a = { ref = 'zenodo:1' }, b = { ref = 'zenodo:2' } }\n";
        let removed = without_dataset(text, "a").unwrap();
        let manifest = parse(&removed);
        assert_eq!(manifest.datasets.len(), 1);
        assert_eq!(
            manifest.datasets["b"].source().unwrap().reference,
            "zenodo:2"
        );
        let removed = without_dataset(&removed, "b").unwrap();
        assert_eq!(parse(&removed).datasets.len(), 0);
        let added = with_dataset("\u{feff}# notes\n", "a", "zenodo:1", &[], &[], true).unwrap();
        assert!(!added.starts_with('\u{feff}'));
        assert!(added.contains("# notes"));
        assert_eq!(
            parse(&added).datasets["a"].source().unwrap().reference,
            "zenodo:1"
        );
    }
}
