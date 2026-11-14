//! `data.toml`: the datasets a project needs. people write it, and `add` and `remove` edit it in
//! place, keeping comments, order and spacing.

use std::collections::BTreeMap;
use std::ops::{Bound, Range};

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
const DATASET_KEYS: [&str; 13] = [
    "ref", "select", "exclude", "extract", "members", "web_page", "copy", "path", "place", "from",
    "run", "code", "env",
];
/// the keys of a dataset fetched from a source, beside `ref`.
const SOURCE_KEYS: [&str; 5] = ["select", "exclude", "extract", "members", "web_page"];
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
    /// whether a web page that answers where a file was expected is the data a person asked for,
    /// rather than a folder listing or a sign-in page to refuse.
    pub(crate) web_page: bool,
}

/// what a dataset keeps of the archives it unpacks: a table per archive, keyed by its path as
/// data.lock gives it or by a glob, as `"shard-*.tar"`, for every archive that matches.
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

    /// adds `filter` under `key`, a glob when it reads as a pattern, else one archive's path.
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

    /// the table for `archive`, its path in `dataset` as data.lock gives it, with its key: the
    /// archive's own, which wins, or else the one glob key that matches. `project.invalid` when
    /// two glob keys match and it has no table of its own.
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

    /// [`Members::of`] for a reader that cannot refuse, as gc deciding what to keep: none when two
    /// glob keys match, so every file counts. planning refuses that data.toml.
    pub(crate) fn get(&self, archive: &str) -> Option<&MemberFilter> {
        match self.found(archive) {
            Found::One(key) => Some(&self.tables[key]),
            Found::None | Found::Two(..) => None,
        }
    }

    /// the tables, in key order, whose keys neither name nor match any of `archives`.
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

/// the files kept of each archive the key covers, by their paths in it, top folder included.
/// they change where files go, not what data.lock pins, so they stay out of the spec.
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
        #[cfg(test)]
        tests::reading();
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

/// data.toml as a settled sync reads it, when it holds only what `add` writes for a dataset of
/// one ref: `[datasets.<name>]` tables of a `ref` and maybe `place`, blank lines and comments. it
/// reads them without a TOML parser, and only where [`Manifest::parse`] would read the same; none
/// for anything else. an entry keeps no select its link implies and cites no line, so a sync that
/// finds anything to do parses data.toml whole.
pub(crate) fn read_plain(text: &str) -> Option<Manifest> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut datasets = BTreeMap::new();
    let mut open: Option<(String, Option<String>, Option<bool>)> = None;
    for line in text.split_inclusive('\n') {
        // a carriage return ends a line only before a line feed
        let line = line
            .strip_suffix('\n')
            .map_or(line, |line| line.strip_suffix('\r').unwrap_or(line));
        let line = line.trim_matches([' ', '\t']);
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            if line.contains(|c: char| c.is_control() && c != '\t') {
                return None;
            }
            continue;
        }
        if let Some(name) = line
            .strip_prefix("[datasets.")
            .and_then(|rest| rest.strip_suffix(']'))
        {
            put_plain(&mut datasets, open.take())?;
            // a bare key of valid name characters, so no quote, dot or space can nest a table
            let bare = name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
            if !bare || name::dataset_name_problem(name).is_some() || datasets.contains_key(name) {
                return None;
            }
            open = Some((name.to_owned(), None, None));
            continue;
        }
        let (_, reference, place) = open.as_mut()?;
        if let Some(value) = line
            .strip_prefix("ref = \"")
            .and_then(|rest| rest.strip_suffix('"'))
        {
            // no escape or quote, so the string is its text
            if reference.is_some()
                || value.contains(['"', '\\'])
                || value.contains(char::is_control)
            {
                return None;
            }
            let value = value.trim();
            if value.is_empty() {
                return None;
            }
            *reference = Some(value.to_owned());
            continue;
        }
        let value = line.strip_prefix("place = ")?;
        if place.is_some() {
            return None;
        }
        *place = Some(match value {
            "true" => true,
            "false" => false,
            _ => return None,
        });
    }
    put_plain(&mut datasets, open)?;
    Some(Manifest {
        data_dir: DATA_DIR.to_owned(),
        datasets,
    })
}

/// adds a dataset [`read_plain`] read, its name, ref and place, to `datasets`; none when it has
/// no ref, which only the parser's error answers.
fn put_plain(
    datasets: &mut BTreeMap<String, Entry>,
    read: Option<(String, Option<String>, Option<bool>)>,
) -> Option<()> {
    let Some((name, reference, place)) = read else {
        return Some(());
    };
    let origin = Origin::Source(Source {
        reference: reference?,
        select: Vec::new(),
        exclude: Vec::new(),
        extract: true,
        members: Members::new(),
        web_page: false,
    });
    datasets.insert(
        name,
        Entry {
            origin,
            copy: false,
            path: None,
            place: place.unwrap_or(true),
            lines: BTreeMap::new(),
        },
    );
    Some(())
}

/// where data.toml may hold a secret, sorted and merged: each ref, whose query can carry a key,
/// and each env, which should not but may. a message cites such a line by number alone.
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
        let names = crate::units::listed(&names, "and");
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

    /// the refusal of `name`, a dataset with members tables alone, as when only its own table was
    /// renamed; it names the new one when a single other ref ends in a file those tables name.
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
            web_page: self.boolean(table.get("web_page"), "web_page", false)?,
        })
    }

    /// the tables under `members`, by archive path. they choose among files fetchloom unpacks,
    /// so a dataset that keeps its archives packed refuses them.
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

    /// the recipe `table` gives; the datasets it reads are checked once every dataset is read.
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

    /// a recipe's env: strings under portable names, none fetchloom's own or a secret.
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
                .help("to keep data on a scratch disk, set FETCHLOOM_CACHE to a folder there, or move the whole project there. see https://nikhil-yn-aix.github.io/fetchloom/user/guides/clusters/#keep-the-project-and-the-cache-on-one-drive"));
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

/// whether every system takes `variable` as a name, as POSIX names them.
fn portable(variable: &str) -> bool {
    variable
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && variable
            .bytes()
            .next()
            .is_some_and(|first| !first.is_ascii_digit())
}

/// whether an uppercase variable name looks like it holds a secret. a word must end in TOKEN, so
/// `MAX_TOKENS` and `TOKENIZERS_PARALLELISM`, which count tokens of text, stay usable.
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

/// whether a folder is shell shorthand a person may expect expanded, as `~` or `${SCRATCH}`.
fn shorthand(folder: &str) -> bool {
    folder.starts_with(['~', '$'])
        || folder
            .split('$')
            .skip(1)
            .any(|after| after.starts_with(|c: char| c.is_ascii_alphabetic() || "_{".contains(c)))
}

/// a dataset `add` writes into data.toml.
#[derive(Clone, Copy)]
pub(crate) struct NewDataset<'a> {
    pub(crate) name: &'a str,
    pub(crate) reference: &'a str,
    pub(crate) select: &'a [String],
    pub(crate) exclude: &'a [String],
    pub(crate) extract: bool,
    pub(crate) web_page: bool,
}

impl NewDataset<'_> {
    /// the dataset's table, as data.toml writes it.
    fn table(&self) -> Table {
        let mut table = Table::new();
        table.insert("ref", toml_edit::value(self.reference));
        for (key, patterns) in [("select", self.select), ("exclude", self.exclude)] {
            if !patterns.is_empty() {
                table.insert(key, toml_edit::value(patterns.iter().collect::<Array>()));
            }
        }
        if !self.extract {
            table.insert("extract", toml_edit::value(false));
        }
        if self.web_page {
            table.insert("web_page", toml_edit::value(true));
        }
        table
    }

    /// the refusal of a data.toml whose datasets are written inline, where no table can go.
    fn refused_inline(&self) -> Error {
        let reference = crate::net::mask_query(self.reference);
        Error::new(
            Kind::ProjectInvalid,
            "data.toml writes its datasets inline, so fetchloom cannot add to them",
        )
        .help(format!(
            "add [datasets.{}] with ref = \"{reference}\" by hand",
            self.name
        ))
    }
}

/// data.toml's text with one more dataset, or a new file of just it. it goes before the dataset
/// whose name follows, so two people adding different datasets rarely change the same lines.
pub(crate) fn with_dataset(
    text: &str,
    name: &str,
    reference: &str,
    select: &[String],
    exclude: &[String],
    extract: bool,
) -> Result<String, Error> {
    let added = NewDataset {
        name,
        reference,
        select,
        exclude,
        extract,
        web_page: false,
    };
    let mut document = editable(text)?;
    if !put(&mut document, name, added.table()) {
        return Err(added.refused_inline());
    }
    Ok(document.to_string())
}

/// data.toml's text with the dataset `name` marked `web_page = true`, as `add --web-page` writes
/// it once the dataset is in.
pub(crate) fn with_web_page(text: &str, name: &str) -> Result<String, Error> {
    let mut document = editable(text)?;
    if let Some(table) = document
        .get_mut("datasets")
        .and_then(|datasets| datasets.get_mut(name))
        .and_then(Item::as_table_mut)
    {
        table.insert("web_page", toml_edit::value(true));
    }
    Ok(document.to_string())
}

/// data.toml's text with each of `added`, placed as [`with_dataset`] places one, and read and
/// written once however many there are. datasets that go before the same one go in name order.
pub(crate) fn with_datasets(text: &str, added: &[NewDataset<'_>]) -> Result<String, Error> {
    let Some(first) = added.first() else {
        return Ok(text.to_owned());
    };
    let mut document = editable(text)?;
    let blank_line_first = !document.as_table().is_empty();
    let datasets = match document.get("datasets") {
        None => None,
        Some(item) => Some(item.as_table().ok_or_else(|| first.refused_inline())?),
    };
    // the datasets written with a header, which a new one can go before, and where each is
    let placed: BTreeMap<String, isize> = datasets
        .into_iter()
        .flat_map(Table::iter)
        .filter_map(|(name, item)| {
            let table = item.as_table().filter(|table| !table.is_dotted())?;
            Some((name.to_owned(), table.position()?))
        })
        .collect();
    let mut sorted: Vec<&NewDataset<'_>> = added.iter().collect();
    sorted.sort_by_key(|added| added.name);
    let mut last = Vec::new();
    let mut before: BTreeMap<isize, (&str, Vec<&NewDataset<'_>>)> = BTreeMap::new();
    for added in sorted {
        let next = placed
            .range::<str, _>((Bound::Excluded(added.name), Bound::Unbounded))
            .next();
        match next {
            Some((next, position)) => {
                before
                    .entry(*position)
                    .or_insert((next.as_str(), Vec::new()))
                    .1
                    .push(added);
            }
            None => last.push(added),
        }
    }
    // each new dataset's blank line or note above it, and the comments of the one it goes
    // before, as `insert_before` gives them
    let has_values = document.iter().any(|(_, item)| item.is_value());
    let first_table = first_position(document.as_table());
    let notes: Vec<(String, String)> = before
        .iter()
        .map(|(position, (next, _))| {
            let following = datasets
                .and_then(|datasets| datasets.get(next))
                .and_then(Item::as_table)
                .map_or("", prefix);
            let (note, theirs) = split_note(following);
            let is_first = !has_values && first_table.is_none_or(|first| first >= *position);
            let above = if note.is_empty() && !is_first {
                "\n"
            } else {
                note
            };
            (above.to_owned(), format!("\n{theirs}"))
        })
        .collect();
    let mut room = Vec::with_capacity(before.len());
    let mut going = 0;
    for (position, (_, tables)) in &before {
        going += tables.len();
        room.push((*position, going));
    }
    make_room(document.as_table_mut(), &room);
    let datasets = document.entry("datasets").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    let Some(datasets) = datasets.as_table_mut() else {
        return Err(first.refused_inline());
    };
    // the last go first, so each takes the place after the datasets there are, as a table
    // without a place of its own follows the one before it
    for (index, added) in last.into_iter().enumerate() {
        let mut table = added.table();
        let above = if index > 0 || blank_line_first {
            "\n"
        } else {
            ""
        };
        table.decor_mut().set_prefix(above);
        datasets.insert(added.name, Item::Table(table));
    }
    let mut moved = 0;
    for ((position, (next, tables)), (above, theirs)) in before.into_iter().zip(notes) {
        let first = position.saturating_add_unsigned(moved);
        for (at, (index, added)) in (first..).zip(tables.iter().enumerate()) {
            let mut table = added.table();
            table.set_position(Some(at));
            table
                .decor_mut()
                .set_prefix(if index == 0 { above.as_str() } else { "\n" });
            datasets.insert(added.name, Item::Table(table));
        }
        moved += tables.len();
        if let Some(following) = datasets.get_mut(next).and_then(Item::as_table_mut) {
            following.decor_mut().set_prefix(theirs);
        }
    }
    Ok(document.to_string())
}

/// moves every table later by as many places as tables go before it, `room` holding each place
/// tables go before, in order, with how many go there and before it.
fn make_room(table: &mut Table, room: &[(isize, usize)]) {
    let later = |position: isize| {
        let at = room.partition_point(|(place, _)| *place <= position);
        at.checked_sub(1).map_or(0, |at| room[at].1)
    };
    let mut shift = |table: &mut Table| {
        if let Some(position) = table.position() {
            table.set_position(Some(position.saturating_add_unsigned(later(position))));
        }
        make_room(table, room);
    };
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(table) => shift(table),
            Item::ArrayOfTables(array) => array.iter_mut().for_each(&mut shift),
            _ => {}
        }
    }
}

/// puts `table` as the dataset `name` before the one whose name follows, or last, with the
/// comments touching its header; false when the datasets are written inline.
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

/// the dataset table whose name follows `name` most closely; none if last or inline.
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

/// moves the dataset `name`, just put last, before `next`, with `touching`, its own comments,
/// above its header; a note standing apart above `next`, as at the file's top, stays above both.
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
/// table for `archive`, under a header of its own as `[datasets.pets.members."pets.zip"]`, each
/// once. `project.invalid` for a dataset or members table written inline, as `add` refuses.
pub(crate) fn with_exclusions(
    text: &str,
    name: &str,
    archive: Option<&str>,
    patterns: &[String],
) -> Result<String, Error> {
    let mut document = editable(text)?;
    exclusions(&mut document, name, archive, patterns)?;
    Ok(document.to_string())
}

/// the edit [`with_exclusions`] makes, on `document`.
fn exclusions(
    document: &mut DocumentMut,
    name: &str,
    archive: Option<&str>,
    patterns: &[String],
) -> Result<(), Error> {
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
    Ok(())
}

/// the refusal to add keys to `what`, a part of data.toml written inline.
fn inline(what: &str) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!("data.toml writes {what} inline, so fetchloom cannot add to it"),
    )
    .help(format!(
        "write it as a [{what}] table, then run the same command again"
    ))
}

/// data.toml's text with `name`'s members table under the glob key `from` copied under `archive`
/// without its comments: an archive's own table wins over a glob, so the archive keeps what the
/// glob chose. select patterns in `unmatched` match nothing there, which an own table refuses, so
/// they are left out. fails as [`with_exclusions`] does on a part written inline.
pub(crate) fn with_members_copied(
    text: &str,
    name: &str,
    from: &str,
    archive: &str,
    unmatched: &[String],
) -> Result<String, Error> {
    let mut document = editable(text)?;
    members_copied(&mut document, name, from, archive, unmatched)?;
    Ok(document.to_string())
}

/// the edit [`with_members_copied`] makes, on `document`.
fn members_copied(
    document: &mut DocumentMut,
    name: &str,
    from: &str,
    archive: &str,
    unmatched: &[String],
) -> Result<(), Error> {
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
            if key == "select" && !unmatched.is_empty() {
                let patterns: Vec<&str> = match &value {
                    Value::String(one) => vec![one.value().as_str()],
                    Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
                    _ => Vec::new(),
                };
                let kept: Array = patterns
                    .into_iter()
                    .filter(|pattern| !unmatched.iter().any(|left| left == pattern))
                    .collect();
                // a select left empty would keep every file, so the copy keeps it whole
                if !kept.is_empty() {
                    value = Value::Array(kept);
                }
            }
            table.insert(key, Item::Value(value));
        }
    }
    members.insert(archive, Item::Table(table));
    Ok(())
}

/// adds the `patterns` the exclude in `table` lacks, writing more than one a pattern per line.
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
    members_out(&mut document, name, archive);
    Ok(document.to_string())
}

/// the edit [`without_members`] makes, on `document`.
fn members_out(document: &mut DocumentMut, name: &str, archive: &str) {
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
}

/// one change drop makes to data.toml, as the function it names makes it.
#[derive(Debug)]
pub(crate) enum Narrowing<'a> {
    /// [`with_members_copied`].
    MembersCopied {
        name: &'a str,
        from: &'a str,
        archive: &'a str,
        unmatched: &'a [String],
    },
    /// [`with_exclusions`].
    Exclusions {
        name: &'a str,
        archive: Option<&'a str>,
        patterns: Vec<String>,
    },
    /// [`without_members`].
    WithoutMembers { name: &'a str, archive: String },
}

/// data.toml's text with each change made in turn, read and written once however many there
/// are. it fails as the first change meeting a part written inline does, making none after it.
pub(crate) fn narrowed(text: &str, changes: &[Narrowing<'_>]) -> Result<String, Error> {
    if changes.is_empty() {
        return Ok(text.to_owned());
    }
    let mut document = editable(text)?;
    if !plain(&document) {
        return narrowed_one_at_a_time(text, changes);
    }
    for change in changes {
        match change {
            Narrowing::MembersCopied {
                name,
                from,
                archive,
                unmatched,
            } => members_copied(&mut document, name, from, archive, unmatched)?,
            Narrowing::Exclusions {
                name,
                archive,
                patterns,
            } => exclusions(&mut document, name, *archive, patterns)?,
            Narrowing::WithoutMembers { name, archive } => {
                members_out(&mut document, name, archive);
            }
        }
    }
    Ok(document.to_string())
}

/// [`narrowed`]'s text, reading and writing data.toml again for each change.
fn narrowed_one_at_a_time(text: &str, changes: &[Narrowing<'_>]) -> Result<String, Error> {
    let mut text = text.to_owned();
    for change in changes {
        text = match change {
            Narrowing::MembersCopied {
                name,
                from,
                archive,
                unmatched,
            } => with_members_copied(&text, name, from, archive, unmatched)?,
            Narrowing::Exclusions {
                name,
                archive,
                patterns,
            } => with_exclusions(&text, name, *archive, patterns)?,
            Narrowing::WithoutMembers { name, archive } => without_members(&text, name, archive)?,
        };
    }
    Ok(text)
}

/// the edit [`without_dataset`] makes, on `document`.
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

/// data.toml's text with the dataset `name` as the data.toml `other` writes it, with the comments
/// touching its header: in place of `text`'s, under any note above it, or before its successor.
pub(crate) fn with_dataset_from(text: &str, other: &str, name: &str) -> Result<String, Error> {
    let mut document = editable(text)?;
    let table = copied(&editable(other)?, name)?;
    if !copy_in(&mut document, name, table) {
        return Err(Error::new(
            Kind::ProjectInvalid,
            "data.toml writes its datasets inline, so fetchloom cannot merge them",
        )
        .help("merge data.toml by hand, then run `fetchloom lock`"));
    }
    Ok(document.to_string())
}

/// the table of the dataset `name` in `other`, to copy into another data.toml.
fn copied(other: &DocumentMut, name: &str) -> Result<Table, Error> {
    let copied = other
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
    Ok(table)
}

/// puts `table` in `document` as the dataset `name`, as [`with_dataset_from`] says. false when
/// `document` writes its datasets inline.
fn copy_in(document: &mut DocumentMut, name: &str, mut table: Table) -> bool {
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
        return true;
    }
    remove(document, name);
    put(document, name, table)
}

/// one dataset's edit for [`with_datasets_from`]: its table as the source at this index writes
/// it, or its removal.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Edit {
    From(usize),
    Out,
}

/// data.toml's text with each edit made in turn, as [`with_dataset_from`] copies a dataset from
/// one of `sources` and [`without_dataset`] takes one out.
pub(crate) fn with_datasets_from(
    text: &str,
    sources: &[&str],
    edits: &[(&str, Edit)],
) -> Result<String, Error> {
    if let Some(edited) = edited_at_once(text, sources, edits) {
        return Ok(edited);
    }
    one_at_a_time(text, sources, edits)
}

/// [`with_datasets_from`]'s text, reading and writing data.toml again for each edit.
fn one_at_a_time(text: &str, sources: &[&str], edits: &[(&str, Edit)]) -> Result<String, Error> {
    let mut text = text.to_owned();
    for (name, edit) in edits {
        text = match edit {
            Edit::From(source) => with_dataset_from(&text, sources[*source], name)?,
            Edit::Out => without_dataset(&text, name)?,
        };
    }
    Ok(text)
}

/// [`with_datasets_from`]'s text from one reading of `text` and each source used; none for an
/// unusual layout or a copy that is not a plain table, left to [`one_at_a_time`], errors and all.
fn edited_at_once(text: &str, sources: &[&str], edits: &[(&str, Edit)]) -> Option<String> {
    if edits.is_empty() {
        return None;
    }
    let mut document = editable(text).ok()?;
    if !plain(&document) {
        return None;
    }
    let mut read: Vec<Option<DocumentMut>> = sources.iter().map(|_| None).collect();
    for (name, edit) in edits {
        match *edit {
            Edit::From(source) => {
                if read[source].is_none() {
                    read[source] = Some(editable(sources[source]).ok()?);
                }
                let table = copied(read[source].as_ref()?, name).ok()?;
                if !table.iter().all(|(_, item)| item.is_value())
                    || !copy_in(&mut document, name, table)
                {
                    return None;
                }
                place_last(&mut document, name);
            }
            Edit::Out => {
                remove(&mut document, name);
                trim_top(&mut document);
            }
        }
    }
    Some(document.to_string())
}

/// whether edits made to `document` at once write what they would with data.toml read again
/// after each: only tables, none dotted or in an array, datasets headed, all in key order.
fn plain(document: &DocumentMut) -> bool {
    if !document.iter().all(|(_, item)| item.is_table()) {
        return false;
    }
    if let Some(datasets) = document.get("datasets") {
        let Some(datasets) = datasets.as_table() else {
            return false;
        };
        let headed = |item: &Item| {
            item.as_table()
                .is_some_and(|table| !table.is_implicit() && table.position().is_some())
        };
        if !datasets.is_implicit() || !datasets.iter().all(|(_, item)| headed(item)) {
            return false;
        }
    }
    in_order(document.as_table(), &mut None)
}

/// whether every table below `table`, visited as they are held, comes after `last` and the one
/// before it, as reading data.toml leaves them.
fn in_order(table: &Table, last: &mut Option<isize>) -> bool {
    table.iter().all(|(_, item)| match item {
        Item::Table(table) => {
            if table.is_dotted() {
                return false;
            }
            if let Some(position) = table.position() {
                if last.is_some_and(|last| last >= position) {
                    return false;
                }
                *last = Some(position);
            }
            in_order(table, last)
        }
        Item::ArrayOfTables(_) => false,
        Item::None | Item::Value(_) => true,
    })
}

/// gives `name`, put after the other datasets, the place reading data.toml again would: just
/// after the last table under the datasets, or after every table when it is the only one.
fn place_last(document: &mut DocumentMut, name: &str) {
    let Some(datasets) = document.get("datasets").and_then(Item::as_table) else {
        return;
    };
    if datasets
        .get(name)
        .and_then(Item::as_table)
        .is_none_or(|table| table.position().is_some())
    {
        return;
    }
    let after = last_position(datasets)
        .or_else(|| last_position(document.as_table()))
        .unwrap_or_default()
        + 1;
    shift_positions(document.as_table_mut(), after);
    if let Some(table) = document
        .get_mut("datasets")
        .and_then(Item::as_table_mut)
        .and_then(|datasets| datasets.get_mut(name))
        .and_then(Item::as_table_mut)
    {
        table.set_position(Some(after));
    }
}

/// the latest place of any table below `table`.
fn last_position(table: &Table) -> Option<isize> {
    let mut last: Option<isize> = None;
    for (_, item) in table {
        if let Item::Table(table) = item {
            for position in table.position().into_iter().chain(last_position(table)) {
                last = Some(last.map_or(position, |last| last.max(position)));
            }
        }
    }
    last
}

/// takes the blank lines off the top of data.toml, as [`without_dataset`] does.
fn trim_top(document: &mut DocumentMut) {
    let Some(first) = first_position(document.as_table()) else {
        let trailing = document.trailing().as_str().unwrap_or_default();
        let trimmed = trailing.trim_start_matches('\n').to_owned();
        document.set_trailing(trimmed);
        return;
    };
    if let Some(table) = table_at(document.as_table_mut(), first) {
        let trimmed = prefix(table).trim_start_matches('\n').to_owned();
        table.decor_mut().set_prefix(trimmed);
    }
}

/// the table below `table` at `position`.
fn table_at(table: &mut Table, position: isize) -> Option<&mut Table> {
    table.iter_mut().find_map(|(_, item)| {
        let table = item.as_table_mut()?;
        if table.position() == Some(position) {
            Some(table)
        } else {
            table_at(table, position)
        }
    })
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
    #[cfg(test)]
    tests::reading();
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
pub(crate) mod tests;
