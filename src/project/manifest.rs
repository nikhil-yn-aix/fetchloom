//! `data.toml`: the datasets a project needs. people write it, and `add` and `remove` edit it in
//! place, keeping comments, order and spacing.

mod required;

use std::collections::BTreeMap;
use std::ops::{Bound, Range};

use toml_edit::{Array, Document, DocumentMut, Item, Key, Table, TableLike, TomlError, Value};

use crate::error::{Error, Kind};
use crate::glob::{self, Glob};
use crate::name;

pub(crate) const FILE: &str = "data.toml";
/// the page that says how to install another fetchloom.
pub(crate) const INSTALL: &str = "https://nikhil-yn-aix.github.io/fetchloom/user/start/install/";
/// the data folder of a project whose data.toml names none.
pub(crate) const DATA_DIR: &str = "data";
/// the `path` that puts a dataset's files straight in the data folder.
pub(crate) const DATA_FOLDER_ITSELF: &str = ".";
/// the names the project's own files take beside data.toml, which no data folder may take.
const PROJECT_FILES: [&str; 3] = [FILE, "data.lock", "data.lock.d"];
/// the files git reads its rules from in a repository's folders, besides its `.git` folder.
const GIT_FILES: [&str; 3] = [".gitignore", ".gitattributes", ".gitmodules"];
/// folders whose files tools run with no one asking: git hooks, as husky or a hooks folder by its
/// usual name holds them, editor tasks, dev container commands and ci workflows.
const TOOL_FOLDERS: [&str; 5] = [".devcontainer", ".github", ".githooks", ".husky", ".vscode"];
const TOP_KEYS: [&str; 3] = [required::KEY, "project", "datasets"];
const PROJECT_KEYS: [&str; 1] = ["data_dir"];
const DATASET_KEYS: [&str; 10] = [
    "ref",
    "select",
    "exclude",
    "extract",
    "decompress",
    "members",
    "web_page",
    "copy",
    "path",
    "at",
];
/// the keys an output takes no value for: its files stay where and as its pipeline wrote them.
const NOT_FOR_OUTPUTS: [&str; 6] = [
    "path",
    "extract",
    "decompress",
    "copy",
    "web_page",
    "members",
];
/// the keys of the table that chooses the files of one archive.
const MEMBER_KEYS: [&str; 2] = ["select", "exclude"];

#[derive(Clone, Debug)]
pub(crate) struct Manifest {
    /// where datasets land, relative to the project, with `/` between folders.
    pub(crate) data_dir: String,
    pub(crate) datasets: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) source: Source,
    pub(crate) copy: bool,
    /// where the dataset lands inside the data folder, when not under its name: a folder, or
    /// [`DATA_FOLDER_ITSELF`].
    pub(crate) path: Option<String>,
    /// for an output, a dataset pinned from the project's own files where its pipeline wrote
    /// them, that folder, from the project's top.
    pub(crate) at: Option<String>,
    /// whether a run places its files in the data folder: false only for a dataset `fetchloom
    /// drop` freed here, which a run that does not name it leaves out.
    pub(crate) place: bool,
    /// each key's line in data.toml, as `data.toml:7   select = ["*.edf"]`, for messages.
    pub(crate) lines: BTreeMap<&'static str, String>,
}

/// a dataset fetched from a source.
#[derive(Clone, Debug)]
pub(crate) struct Source {
    pub(crate) reference: String,
    pub(crate) select: Vec<Glob>,
    pub(crate) exclude: Vec<Glob>,
    pub(crate) extract: bool,
    /// whether each single compressed file, as `reads.fastq.gz`, lands decompressed, under its
    /// name without the ending.
    pub(crate) decompress: bool,
    /// which files of each archive it unpacks the dataset keeps.
    pub(crate) members: Members,
    /// whether a web page that answers where a file was expected is the data a person asked for,
    /// rather than a folder listing or a sign-in page to refuse.
    pub(crate) web_page: bool,
}

impl Source {
    /// whether `other` takes the same files of its source as this: the same patterns, and
    /// archives and compressed files unpacked alike.
    pub(crate) fn same_settings(&self, other: &Source) -> bool {
        self.select == other.select
            && self.exclude == other.exclude
            && self.extract == other.extract
            && self.decompress == other.decompress
    }
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

impl Entry {
    /// whether the dataset is an import: files from this machine, pinned by their bytes.
    pub(crate) fn is_import(&self) -> bool {
        crate::source::local::is_imported(&self.source.reference)
    }

    /// whether the dataset is an output: its tree is the one its pipeline wrote at `at`, extra
    /// files there count, and its files stay writable.
    pub(crate) fn is_output(&self) -> bool {
        self.at.is_some()
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
        // before any other key, so a key a newer fetchloom added is not what stops the read
        if let Some(item) = root.get(required::KEY) {
            reader.required(item, required::RUNNING)?;
        }
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
        for (name, entry) in &datasets {
            refuse_holding_data(name, entry, &data_dir)?;
        }
        Ok(Manifest { data_dir, datasets })
    }

    /// the datasets `names` taken out of this manifest into one of their own, with the same data
    /// folder. what stays is what [`Manifest::parse`] reads of data.toml without them, but for
    /// the lines each entry cites, which still count the lines of the text read.
    pub(crate) fn take(&mut self, names: &[String]) -> Manifest {
        let datasets = names
            .iter()
            .filter_map(|name| self.datasets.remove_entry(name))
            .collect();
        Manifest {
            data_dir: self.data_dir.clone(),
            datasets,
        }
    }
}

/// data.toml as a settled sync reads it, when it holds only what `add` writes for a dataset of
/// one ref: `[datasets.<name>]` tables of a `ref`, blank lines and comments, below a
/// `required_version` this fetchloom is in. it reads them without a TOML parser, and only where
/// [`Manifest::parse`] would read the same; none for anything else. an entry keeps no select its
/// link implies and cites no line, so a sync that finds anything to do parses data.toml whole.
pub(crate) fn read_plain(text: &str) -> Option<Manifest> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut datasets = BTreeMap::new();
    let mut open: Option<(String, Option<String>)> = None;
    let mut required = false;
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
            open = Some((name.to_owned(), None));
            continue;
        }
        let Some((_, reference)) = open.as_mut() else {
            // above the tables, only a range the parser lets this fetchloom through
            let value = line
                .strip_prefix(required::KEY)
                .and_then(|rest| rest.strip_prefix(" = \""))
                .and_then(|rest| rest.strip_suffix('"'))?;
            if required || value.contains(['"', '\\']) || value.contains(char::is_control) {
                return None;
            }
            let range = required::Range::parse(value).ok()?;
            if range.verdict(required::RUNNING) != required::Verdict::Allowed {
                return None;
            }
            required = true;
            continue;
        };
        let value = line
            .strip_prefix("ref = \"")
            .and_then(|rest| rest.strip_suffix('"'))?;
        // no escape or quote, so the string is its text
        if reference.is_some() || value.contains(['"', '\\']) || value.contains(char::is_control) {
            return None;
        }
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        *reference = Some(value.to_owned());
    }
    put_plain(&mut datasets, open)?;
    Some(Manifest {
        data_dir: DATA_DIR.to_owned(),
        datasets,
    })
}

/// adds a dataset [`read_plain`] read, its name and ref, to `datasets`; none when it has no ref,
/// which only the parser's error answers.
fn put_plain(
    datasets: &mut BTreeMap<String, Entry>,
    read: Option<(String, Option<String>)>,
) -> Option<()> {
    let Some((name, reference)) = read else {
        return Some(());
    };
    let source = Source {
        reference: reference?,
        select: Vec::new(),
        exclude: Vec::new(),
        extract: true,
        decompress: false,
        members: Members::new(),
        web_page: false,
    };
    datasets.insert(
        name,
        Entry {
            source,
            copy: false,
            path: None,
            at: None,
            place: true,
            lines: BTreeMap::new(),
        },
    );
    Some(())
}

/// where data.toml may hold a secret, sorted and merged: each ref, whose query can carry a key.
/// a message cites such a line by number alone.
fn private(root: &Table) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let datasets = root.get("datasets").and_then(Item::as_table_like);
    for (_, entry) in datasets.into_iter().flat_map(TableLike::iter) {
        let Some(entry) = entry.as_table_like() else {
            continue;
        };
        spans.extend(entry.get("ref").and_then(Item::span));
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
        if table.contains_key("members") && table.iter().all(|(key, _)| key == "members") {
            return Err(self.left_members(datasets, table, name, item.span().or(key_span)));
        }
        let at = table
            .get("at")
            .map(|item| self.relative(item, "at"))
            .transpose()?;
        let mut source = self.source(table, name, item.span().or(key_span))?;
        if let Some(at) = &at {
            self.output(table, name, at, &source)?;
            source.extract = false;
        }
        Ok(Entry {
            source,
            copy: self.boolean(table.get("copy"), "copy", false)?,
            path: table
                .get("path")
                .map(|item| self.relative(item, "path"))
                .transpose()?,
            at,
            place: true,
            lines,
        })
    }

    /// refuses what an output at `at`, the dataset `name` whose table is `table`, cannot take: a
    /// key that would move, unpack, link or fetch its files, or a ref other than an import's.
    fn output(
        &self,
        table: &dyn TableLike,
        name: &str,
        at: &str,
        source: &Source,
    ) -> Result<(), Error> {
        if let Some(key) = NOT_FOR_OUTPUTS
            .into_iter()
            .find(|key| table.contains_key(key))
        {
            return Err(self
                .error(
                    table.key(key).and_then(Key::span),
                    format!(
                        "datasets.{name} is an output, kept at {at} as its pipeline wrote it, so it takes no {key}"
                    ),
                )
                .help(format!("remove {key} from datasets.{name} in data.toml")));
        }
        if !crate::source::local::is_imported(&source.reference) {
            return Err(self
                .error(
                    table.get("ref").and_then(Item::span),
                    format!(
                        "datasets.{name} has at = \"{at}\", which only an output pinned from the project's own files has, so its ref must be \"{}\"",
                        crate::source::local::IMPORTED
                    ),
                )
                .help("to choose where a fetched dataset lands in the data folder, use path instead of at"));
        }
        Ok(())
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
            decompress: self.boolean(table.get("decompress"), "decompress", false)?,
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

    /// refuses any key not in `known`, suggesting the closest one.
    fn known(&self, table: &dyn TableLike, known: &[&str], within: &str) -> Result<(), Error> {
        for (key, _) in table.iter() {
            if !known.contains(&key) {
                let error = self.error(
                    table.key(key).and_then(Key::span),
                    format!("data.toml has a key fetchloom does not know: \"{key}\"{within}"),
                );
                return Err(match name::closest(key, known.iter().copied()) {
                    _ if key == required::KEY => error.help(format!(
                        "{key} goes at the top of data.toml, above every [table]"
                    )),
                    Some(best) => error.help(format!("did you mean `{best}`?")),
                    None => error.help(format!("the keys here are {}", known.join(", "))),
                });
            }
        }
        Ok(())
    }

    /// refuses data.toml when its `required_version`, `item`, leaves out fetchloom `running`, or
    /// is no range of versions.
    fn required(&self, item: &Item, running: &str) -> Result<(), Error> {
        let written = self.string(item, required::KEY)?;
        let range = required::Range::parse(written).map_err(|reason| {
            self.error(
                item.span(),
                format!(
                    "{} = \"{written}\" in data.toml is refused because {reason}",
                    required::KEY
                ),
            )
            .help("write the fetchloom versions the project works with as comparisons, such as \">=0.1.0\" for 0.1.0 or newer")
        })?;
        match range.verdict(running) {
            required::Verdict::Allowed => Ok(()),
            verdict => Err(self
                .error(
                    item.span(),
                    format!("this project needs fetchloom {range}, and this one is {running}"),
                )
                .help(range.help(verdict))),
        }
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

    /// a path inside the project, as [`folder`] reads it, written for `key` in `item`.
    fn relative(&self, item: &Item, key: &str) -> Result<String, Error> {
        let written = self.string(item, key)?;
        folder(written, key).map_err(|(reason, help)| {
            self.error(
                item.span(),
                format!("{key} = \"{written}\" in data.toml is refused because {reason}"),
            )
            .help(help)
        })
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

/// a path inside the project, written for `key`, which may not climb out of it, read as written,
/// so a space is kept and a folder ending in one refused, as windows drops it; a leading `./` and
/// a trailing `/` name the same folder without them. a dataset's `path` may also be
/// [`DATA_FOLDER_ITSELF`], the data folder. `data_dir` and an output's `at` are folders from the
/// project's top, held to the same rules. a refused one gives why, and what to write instead.
pub(crate) fn folder(written: &str, key: &str) -> Result<String, (String, String)> {
    let from_top = key == "data_dir" || key == "at";
    let own_folder = if key == "at" {
        "use a folder of its own inside the project, such as results"
    } else {
        "use a folder of its own inside the project, such as data"
    };
    let mut text = written.trim_end_matches('/');
    while let Some(rest) = text.strip_prefix("./") {
        text = rest;
    }
    let refused = |reason: &str, help: &str| Err((reason.to_owned(), help.to_owned()));
    if let Some(reason) = name::metadata(text) {
        let outside = if reason == name::OWN_FOLDER {
            ".fetchloom"
        } else {
            "the repository's metadata"
        };
        return refused(reason, &format!("choose a {key} outside {outside}"));
    }
    if shorthand(text) {
        let inside = if from_top {
            "the project"
        } else {
            "the data folder"
        };
        return refused(
            &format!(
                "fetchloom does not expand ~ or $NAME, and the folder is always inside {inside}"
            ),
            "to keep data on a scratch disk, set FETCHLOOM_CACHE to a folder there, or move the whole project there. see https://nikhil-yn-aix.github.io/fetchloom/user/guides/clusters/#keep-the-project-and-the-cache-on-one-drive",
        );
    }
    if text == DATA_FOLDER_ITSELF {
        let mixed = "it would mix data with the project's own files";
        return match key {
            "path" => Ok(text.to_owned()),
            "at" => refused(mixed, own_folder),
            _ => refused(mixed, "use a folder inside the project, such as data"),
        };
    }
    let named = |names: &[&str], part: &str| {
        let part = name::fold(part);
        names.iter().any(|known| name::fold(known) == part)
    };
    let first = text.split('/').next().unwrap_or_default();
    let part_of = |names: &[&str]| text.split('/').find(|part| named(names, part));
    let reason = if !from_top {
        None
    } else if named(&PROJECT_FILES, first) {
        Some(format!(
            "{first} is the name of one of the project's own files"
        ))
    } else if let Some(file) = part_of(&GIT_FILES) {
        // git reads these files in any folder, and a folder of that name stands in for one
        Some(format!("{file} is the name of a file git reads"))
    } else {
        // data placed there would run as code, as a hook husky points git at runs on commit
        part_of(&TOOL_FOLDERS)
            .map(|folder| format!("{folder} is a folder other tools run files from"))
    };
    if let Some(reason) = reason {
        return refused(&reason, own_folder);
    }
    if let Some(reason) = name::escapes(text) {
        let help = match key {
            "path" if text.is_empty() => "use \".\" for the data folder itself",
            "path" => {
                "use a folder inside the data folder, with / between folders, or \".\" for the data folder itself"
            }
            _ => "use a folder inside the project, with / between folders",
        };
        return refused(reason, help);
    }
    // a data.toml is shared with windows clones, where every file would hit the same name
    match name::unportable(text) {
        None => Ok(text.to_owned()),
        Some(reason) => {
            let such = if key == "data_dir" {
                ", such as data"
            } else {
                ""
            };
            refused(
                &reason,
                &format!(
                    "choose a {key} every system can store, since clones on windows read the same data.toml{such}"
                ),
            )
        }
    }
}

/// whether the folder `at` is or holds the data folder `data_dir`, where fetched datasets land, as
/// a disk that ignores case or unicode composition sees the two.
pub(crate) fn holds_data(at: &str, data_dir: &str) -> bool {
    let (at, data) = (name::fold(at), name::fold(data_dir));
    at == data || data.starts_with(&format!("{at}/"))
}

/// refuses an output of `name` whose folder [`holds_data`].
fn refuse_holding_data(name: &str, entry: &Entry, data_dir: &str) -> Result<(), Error> {
    let Some(at) = &entry.at else {
        return Ok(());
    };
    if !holds_data(at, data_dir) {
        return Ok(());
    }
    let error = Error::new(
        Kind::ProjectInvalid,
        format!(
            "at = \"{at}\" in datasets.{name} holds the data folder {data_dir}, where fetched datasets land"
        ),
    );
    let error = match entry.lines.get("at") {
        Some(line) => error.detail(line.clone()),
        None => error,
    };
    Err(error.help("pin the output in a folder of its own, such as data/processed or results"))
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
    pub(crate) decompress: bool,
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
        if self.decompress {
            table.insert("decompress", toml_edit::value(true));
        }
        if self.web_page {
            table.insert("web_page", toml_edit::value(true));
        }
        table
    }

    /// the refusal of a data.toml whose datasets are written inline, where no table can go.
    fn refused_inline(&self) -> Error {
        let reference = crate::shown::mask_query(self.reference);
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
        decompress: false,
        web_page: false,
    };
    let mut document = editable(text)?;
    if !put(&mut document, name, added.table()) {
        return Err(added.refused_inline());
    }
    Ok(document.to_string())
}

/// data.toml's text with the output `name` added: files of the project pinned where they are at
/// `at`, with the patterns that choose them.
pub(crate) fn with_output(
    text: &str,
    name: &str,
    at: &str,
    select: &[String],
    exclude: &[String],
) -> Result<String, Error> {
    let added = NewDataset {
        name,
        reference: crate::source::local::IMPORTED,
        select,
        exclude,
        extract: true,
        decompress: false,
        web_page: false,
    };
    let mut table = Table::new();
    table.insert("ref", toml_edit::value(added.reference));
    table.insert("at", toml_edit::value(at));
    for (key, patterns) in [("select", select), ("exclude", exclude)] {
        if !patterns.is_empty() {
            table.insert(key, toml_edit::value(patterns.iter().collect::<Array>()));
        }
    }
    let mut document = editable(text)?;
    if !put(&mut document, name, table) {
        return Err(added.refused_inline());
    }
    Ok(document.to_string())
}

/// data.toml's text with `key = true` on the dataset `name`, as `add --web-page` and `add
/// --decompress` write it once the dataset is in.
pub(crate) fn with_set(text: &str, name: &str, key: &str) -> Result<String, Error> {
    let mut document = editable(text)?;
    if let Some(table) = document
        .get_mut("datasets")
        .and_then(|datasets| datasets.get_mut(name))
        .and_then(Item::as_table_mut)
    {
        table.insert(key, toml_edit::value(true));
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
fn without_dataset(text: &str, name: &str) -> Result<String, Error> {
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
fn with_members_copied(
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
fn without_members(text: &str, name: &str, archive: &str) -> Result<String, Error> {
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

/// one change exclude makes to data.toml, as the function it names makes it.
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

/// data.toml's text without the datasets `names`, read and written once however many there are.
/// `between` runs before each name with its index, and an error from it ends the edit, so a stop
/// it reports leaves nothing half written.
pub(crate) fn without_datasets(
    text: &str,
    names: &[String],
    mut between: impl FnMut(usize) -> Result<(), Error>,
) -> Result<String, Error> {
    let mut document = editable(text)?;
    for (at, name) in names.iter().enumerate() {
        between(at)?;
        remove(&mut document, name);
    }
    Ok(document.to_string().trim_start_matches('\n').to_owned())
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
fn with_dataset_from(text: &str, other: &str, name: &str) -> Result<String, Error> {
    let mut document = editable(text)?;
    let table = copied(&editable(other)?, name)?;
    if !copy_in(&mut document, name, table) {
        return Err(Error::new(
            Kind::ProjectInvalid,
            "data.toml writes its datasets inline, so fetchloom cannot merge them",
        )
        .help(format!(
            "merge data.toml by hand, then run `{}`",
            crate::suggested::command("lock")
        )));
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
