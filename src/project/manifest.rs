//! `data.toml`: the datasets a project needs. people write it, and `add` and `remove` edit it in
//! place, keeping comments, order and spacing.

use std::collections::BTreeMap;
use std::ops::Range;

use toml_edit::{Array, Document, DocumentMut, Item, Key, Table, TableLike, TomlError};

use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::name;

pub(crate) const FILE: &str = "data.toml";
const DATA_DIR: &str = "data";
const TOP_KEYS: [&str; 2] = ["project", "datasets"];
const PROJECT_KEYS: [&str; 1] = ["data_dir"];
const DATASET_KEYS: [&str; 6] = ["ref", "select", "exclude", "extract", "copy", "path"];

#[derive(Clone, Debug)]
pub(crate) struct Manifest {
    /// where datasets land, relative to the project, with `/` between folders.
    pub(crate) data_dir: String,
    pub(crate) datasets: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) reference: String,
    pub(crate) select: Vec<Glob>,
    pub(crate) exclude: Vec<Glob>,
    pub(crate) extract: bool,
    pub(crate) copy: bool,
    /// where the dataset lands inside the data folder, when not under its name.
    pub(crate) path: Option<String>,
    /// each key's line in data.toml, as `data.toml:7   select = ["*.edf"]`, for messages.
    pub(crate) lines: BTreeMap<&'static str, String>,
}

impl Manifest {
    pub(crate) fn parse(text: &str) -> Result<Manifest, Error> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let reader = Reader { text };
        let document = Document::parse(text).map_err(|err| not_toml(text, &err))?;
        let root = document.as_table();
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
        Ok(Manifest { data_dir, datasets })
    }
}

/// reads data.toml's items into types, pointing at the line of any mistake.
struct Reader<'a> {
    text: &'a str,
}

impl Reader<'_> {
    fn entry(&self, datasets: &dyn TableLike, name: &str, item: &Item) -> Result<Entry, Error> {
        let key_span = datasets.key(name).and_then(Key::span);
        if !name::is_dataset_name(name) {
            let error = self.error(key_span, format!("\"{name}\" is not a valid dataset name"));
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
        let Some(reference) = table.get("ref") else {
            return Err(self
                .error(
                    item.span().or(key_span),
                    format!("datasets.{name} in data.toml has no ref"),
                )
                .help("add a line such as ref = \"zenodo:3608990\""));
        };
        let reference = self.string(reference, "ref")?.trim().to_owned();
        if reference.is_empty() {
            return Err(self.error(
                table.get("ref").and_then(Item::span),
                format!("datasets.{name} has an empty ref"),
            ));
        }
        Ok(Entry {
            reference,
            select: self.patterns(table.get("select"), "select")?,
            exclude: self.patterns(table.get("exclude"), "exclude")?,
            extract: self.boolean(table.get("extract"), "extract", true)?,
            copy: self.boolean(table.get("copy"), "copy", false)?,
            path: table
                .get("path")
                .map(|item| self.relative(item, "path"))
                .transpose()?,
            lines,
        })
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

    /// a path inside the project, which may not climb out of it.
    fn relative(&self, item: &Item, key: &str) -> Result<String, Error> {
        let text = self.string(item, key)?.trim().trim_end_matches('/');
        if let Some(reason) = name::metadata(text) {
            return Err(self
                .error(
                    item.span(),
                    format!("{key} = \"{text}\" in data.toml is refused because {reason}"),
                )
                .help(format!("choose a {key} outside the repository's metadata")));
        }
        match name::escapes(text) {
            None => Ok(text.to_owned()),
            Some(reason) => Err(self
                .error(
                    item.span(),
                    format!("{key} = \"{text}\" in data.toml is refused because {reason}"),
                )
                .help("use a folder inside the project, with / between folders")),
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
        let before = &self.text[..offset];
        let number = before.matches('\n').count() + 1;
        let start = before.rfind('\n').map_or(0, |at| at + 1);
        let end = self.text[start..]
            .find('\n')
            .map_or(self.text.len(), |at| start + at);
        format!("{FILE}:{number}   {}", self.text[start..end].trim())
    }
}

/// data.toml's text with one more dataset, or a new file holding just that dataset.
pub(crate) fn with_dataset(
    text: &str,
    name: &str,
    reference: &str,
    select: &[String],
    exclude: &[String],
    extract: bool,
) -> Result<String, Error> {
    let mut document = editable(text)?;
    let blank_line_first = !document.as_table().is_empty();
    let datasets = document.entry("datasets").or_insert_with(|| {
        let mut table = Table::new();
        table.set_implicit(true);
        Item::Table(table)
    });
    let Some(datasets) = datasets.as_table_mut() else {
        let reference = crate::net::mask_query(reference);
        return Err(Error::new(
            Kind::ProjectInvalid,
            "data.toml writes its datasets inline, so fetchloom cannot add to them",
        )
        .help(format!(
            "add [datasets.{name}] with ref = \"{reference}\" by hand"
        )));
    };
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
    if blank_line_first {
        table.decor_mut().set_prefix("\n");
    }
    datasets.insert(name, Item::Table(table));
    Ok(document.to_string())
}

/// data.toml's text without one dataset. comments touching its header go with it, and comments
/// a blank line above stay, such as a note at the top of the file.
pub(crate) fn without_dataset(text: &str, name: &str) -> Result<String, Error> {
    let mut document = editable(text)?;
    let Some(datasets) = document
        .get_mut("datasets")
        .and_then(Item::as_table_like_mut)
    else {
        return Ok(document.to_string());
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
        if let Some(table) = next_table(&mut document, position) {
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
    Ok(document.to_string().trim_start_matches('\n').to_owned())
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
    Reader { text }
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
        assert_eq!(eeg.reference, "osf:abc12");
        assert_eq!(
            eeg.select.iter().map(Glob::as_str).collect::<Vec<_>>(),
            ["**/*.edf", "participants.tsv"]
        );
        assert_eq!(eeg.exclude.len(), 1);
        assert!(eeg.extract && !eeg.copy && eeg.path.is_none());
        assert_eq!(
            eeg.lines["select"],
            "data.toml:6   select = [\"**/*.edf\", \"participants.tsv\"]"
        );
        let weights = &manifest.datasets["weights"];
        assert_eq!(weights.reference, "https://example.org/w.bin");
        assert!(!weights.extract && weights.copy);
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
            "# my data\n[project]\ndata_dir = \"data\" # here\n\n[datasets.b]\nref = \"zenodo:2\"\n\n[datasets.a]\nref = \"zenodo:1\"\nselect = [\"*.csv\"]\n"
        );
        assert_eq!(parse(&added).datasets.len(), 2);
        let fresh = with_dataset("", "iris", "zenodo:3608990", &[], &[], true).unwrap();
        assert_eq!(fresh, "[datasets.iris]\nref = \"zenodo:3608990\"\n");
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
    type Values = (String, Vec<String>, Vec<String>, bool, bool, Option<String>);

    fn patterns(globs: &[Glob]) -> Vec<String> {
        globs.iter().map(|glob| glob.as_str().to_owned()).collect()
    }

    fn stated(manifest: &Manifest) -> Stated {
        let datasets = manifest.datasets.iter().map(|(dataset, entry)| {
            let values = (
                entry.reference.clone(),
                patterns(&entry.select),
                patterns(&entry.exclude),
                entry.extract,
                entry.copy,
                entry.path.clone(),
            );
            (dataset.clone(), values)
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
                assert!(!entry.reference.is_empty(), "{dataset:?}");
                assert_eq!(entry.reference.trim(), entry.reference);
                if let Some(path) = &entry.path {
                    assert_eq!(name::escapes(path), None, "{path:?}");
                    assert_eq!(name::metadata(path), None, "{path:?}");
                }

                let removed = without_dataset(text, dataset).unwrap();
                let mut expected = before.clone();
                expected.1.remove(dataset);
                let after = Manifest::parse(&removed).map(|manifest| stated(&manifest));
                assert_eq!(after.ok(), Some(expected), "{removed:?}");

                let copy = format!("{dataset}-2");
                if !name::is_dataset_name(&copy) || manifest.datasets.contains_key(&copy) {
                    continue;
                }
                let added = match with_dataset(
                    text,
                    &copy,
                    &entry.reference,
                    &patterns(&entry.select),
                    &patterns(&entry.exclude),
                    entry.extract,
                ) {
                    Ok(added) => added,
                    Err(err) => {
                        assert!(err.message.contains("inline"), "{}", err.message);
                        continue;
                    }
                };
                let mut expected = before.clone();
                let (reference, select, exclude, extract, _, _) = before.1[dataset].clone();
                let values = (reference, select, exclude, extract, false, None);
                expected.1.insert(copy, values);
                let after = Manifest::parse(&added).map(|manifest| stated(&manifest));
                assert_eq!(after.ok(), Some(expected), "{added:?}");
            }
        });
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
                "the keys here are ref, select, exclude, extract, copy, path",
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
    fn data_directories_must_stay_inside_the_project() {
        for directory in [
            "",
            " ",
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
        let replacement = &manifest.datasets["replace"];
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
        assert_eq!(manifest.datasets["b"].reference, "zenodo:2");
        let removed = without_dataset(&removed, "b").unwrap();
        assert_eq!(parse(&removed).datasets.len(), 0);
        let added = with_dataset("\u{feff}# notes\n", "a", "zenodo:1", &[], &[], true).unwrap();
        assert!(!added.starts_with('\u{feff}'));
        assert!(added.contains("# notes"));
        assert_eq!(parse(&added).datasets["a"].reference, "zenodo:1");
    }
}
