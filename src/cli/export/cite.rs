//! citations of the datasets data.lock pins, as `export --format bibtex`, `csl` and `ris` write
//! them, from each DOI's record, which doi.org gives once a DOI, or from data.lock alone, saying
//! what it lacks; and the data availability statement `--format statement` writes from data.lock.
//! data.lock and doi.org's answers are not trusted: every word is cleaned and escaped for its form.

use std::fmt::Write as _;
use std::sync::Arc;

use serde::Serialize;
use tokio::task::JoinSet;

use crate::cli::Context;
use crate::cli::args::Format;
use crate::error::{Error, Kind};
use crate::name;
use crate::output::{Output, Warning};
use crate::project::lock::Locked;
use crate::source::Reference;
use crate::units;

/// what a citation of one dataset names, as `--json` gives it.
#[derive(Debug, Serialize)]
pub(super) struct Cited {
    dataset: String,
    /// where its facts come from: `doi.org`, or `data.lock` alone.
    from: &'static str,
    /// why doi.org gave nothing for its DOI: not asked offline, or what went wrong.
    #[serde(skip_serializing_if = "Option::is_none")]
    unasked: Option<String>,
    /// its CSL type, as `dataset` or `article-journal`.
    #[serde(rename = "type")]
    kind: String,
    title: Option<String>,
    authors: Vec<Name>,
    /// the year, month and day it was published, as many as are known.
    issued: Vec<i64>,
    publisher: Option<String>,
    /// the journal of an article.
    container: Option<String>,
    version: Option<String>,
    doi: Option<String>,
    /// its DOI's link, else data.lock's link for it or the page its ref names.
    url: Option<String>,
    license: Option<String>,
    /// when data.lock pinned it, as `2026-11-13`.
    accessed: Option<String>,
    /// what a citation needs that the entry lacks: `author`, `year`, `title` or `link`.
    missing: Vec<&'static str>,
}

/// an author, as CSL-JSON names one: a family and given name, or one name for a group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct Name {
    #[serde(skip_serializing_if = "Option::is_none")]
    family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    given: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    literal: Option<String>,
}

impl Cited {
    /// the entry data.lock alone gives `locked`, which `link` links to when no DOI does.
    fn of_lock(locked: &Locked, link: Option<String>) -> Cited {
        let doi = locked.doi.as_deref().and_then(bare_doi);
        let paper =
            Reference::parse(&locked.resolved).is_ok_and(|parsed| parsed.reference.is_paper());
        let mut cited = Cited {
            dataset: locked.name.clone(),
            from: "data.lock",
            unasked: None,
            kind: if paper { "article" } else { "dataset" }.to_owned(),
            title: locked
                .title
                .as_deref()
                .map(words)
                .filter(|title| !title.is_empty()),
            authors: Vec::new(),
            issued: Vec::new(),
            publisher: None,
            container: None,
            version: version(&locked.resolved),
            url: doi.as_deref().map(doi_link).or(link),
            doi,
            license: locked
                .license
                .as_deref()
                .map(words)
                .filter(|license| !license.is_empty()),
            accessed: locked
                .timestamp
                .get(..10)
                .filter(|day| {
                    day.bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'-')
                })
                .map(str::to_owned),
            missing: Vec::new(),
        };
        cited.missing = cited.lacks();
        cited
    }

    /// takes the facts of a DOI's record, CSL-JSON as doi.org gave it, keeping data.lock's where
    /// the record has none.
    fn learn(&mut self, record: &serde_json::Value) {
        let text = |key: &str| {
            let value = record.get(key)?;
            let text = value
                .as_str()
                .or_else(|| value.as_array()?.first()?.as_str())?;
            Some(words(text)).filter(|text| !text.is_empty())
        };
        self.from = "doi.org";
        if let Some(kind) = text("type").filter(|kind| {
            kind.chars()
                .all(|c| c.is_ascii_lowercase() || c == '-' || c == '_')
        }) {
            self.kind = kind;
        }
        self.title = text("title").or(self.title.take());
        self.publisher = text("publisher");
        self.container = text("container-title");
        self.version = text("version").or(self.version.take());
        self.authors = record
            .get("author")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(author)
            .collect();
        self.issued = record
            .get("issued")
            .and_then(|issued| {
                issued
                    .get("date-parts")?
                    .as_array()?
                    .first()?
                    .as_array()
                    .cloned()
            })
            .into_iter()
            .flatten()
            .map_while(|part| part.as_i64().or_else(|| part.as_str()?.parse().ok()))
            .take(3)
            .collect();
        self.missing = self.lacks();
    }

    /// what a citation needs that the entry lacks.
    fn lacks(&self) -> Vec<&'static str> {
        [
            ("author", self.authors.is_empty()),
            ("year", self.issued.is_empty()),
            ("title", self.title.is_none()),
            ("link", self.url.is_none()),
        ]
        .into_iter()
        .filter_map(|(what, lacks)| lacks.then_some(what))
        .collect()
    }

    /// the title it is cited by: its own, else its dataset's name.
    fn title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.dataset)
    }

    /// what the entry says of itself, where a reader of the file sees it: what it lacks, and,
    /// when `license` asks, the license data.lock records.
    fn note(&self, license: bool) -> Option<String> {
        let mut said = Vec::new();
        if !self.missing.is_empty() {
            let missing = units::listed(&self.missing, "or");
            said.push(match self.from {
                "doi.org" => format!("Incomplete: its DOI record gives no {missing}."),
                _ => format!("Incomplete: built from data.lock, which gives no {missing}."),
            });
        }
        if license && let Some(license) = &self.license {
            said.push(format!("License: {license}."));
        }
        (!said.is_empty()).then(|| said.join(" "))
    }

    fn year(&self) -> Option<i64> {
        self.issued.first().copied()
    }
}

/// a CSL-JSON author that has a name, cleaned: a family name, else one name for a group.
fn author(value: &serde_json::Value) -> Option<Name> {
    let part = |key: &str| {
        value
            .get(key)?
            .as_str()
            .map(words)
            .filter(|part| !part.is_empty())
    };
    let family = part("family");
    let literal = part("literal").or_else(|| part("name"));
    if family.is_none() && literal.is_none() {
        return None;
    }
    Some(Name {
        given: part("given").filter(|_| family.is_some()),
        literal: literal.filter(|_| family.is_none()),
        family,
    })
}

/// `text` on one line: each control character, a line break or tab among them, a space, and
/// spaces run together.
fn words(text: &str) -> String {
    text.split(|c: char| c.is_whitespace() || name::is_control(c))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// a DOI as data.lock or a source may write it, as `doi:10.5281/zenodo.1` or its doi.org link,
/// without that prefix, on one line; none when nothing is left.
fn bare_doi(text: &str) -> Option<String> {
    let text = words(text);
    let lower = text.to_ascii_lowercase();
    let prefix = [
        "doi:",
        "https://doi.org/",
        "http://doi.org/",
        "https://dx.doi.org/",
        "http://dx.doi.org/",
    ]
    .into_iter()
    .find(|prefix| lower.starts_with(prefix))
    .map_or(0, str::len);
    Some(text[prefix..].trim().to_owned()).filter(|doi| !doi.is_empty())
}

/// the version `resolved`, as data.lock writes it, names after `@`, as `kaggle:uciml/iris@2`.
fn version(resolved: &str) -> Option<String> {
    let (base, version) = resolved.rsplit_once('@')?;
    Reference::version_left_out(resolved, base).then(|| words(version))
}

/// `doi`'s link at doi.org, each character a link cannot hold escaped.
fn doi_link(doi: &str) -> String {
    const KEPT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'/')
        .remove(b'.')
        .remove(b'-')
        .remove(b'_')
        .remove(b'(')
        .remove(b')')
        .remove(b':')
        .remove(b';');
    format!(
        "https://doi.org/{}",
        percent_encoding::utf8_percent_encode(doi, KEPT)
    )
}

/// the entry of each of `pins`, from its DOI's record where doi.org gives one, the DOIs asked
/// together as the network's limits allow; offline, or when doi.org gives nothing, from data.lock
/// alone, as a warning says. `link` gives data.lock's link for a dataset. fails at a stop.
pub(super) fn cite(
    context: &Context,
    pins: &[Locked],
    link: impl Fn(&Locked) -> Option<String>,
) -> Result<Vec<Cited>, Error> {
    let mut cited: Vec<Cited> = pins
        .iter()
        .map(|locked| Cited::of_lock(locked, link(locked)))
        .collect();
    let asked: Vec<(usize, Reference)> = cited
        .iter()
        .enumerate()
        .filter_map(|(at, entry)| {
            let parsed = Reference::parse(&format!("doi:{}", entry.doi.as_deref()?)).ok()?;
            Some((at, parsed.reference))
        })
        .collect();
    let output = &context.output;
    if context.offline.enabled() {
        let names: Vec<&str> = asked
            .iter()
            .map(|(at, _)| cited[*at].dataset.as_str())
            .collect();
        if !names.is_empty() {
            let entries = if names.len() == 1 {
                "its entry is"
            } else {
                "their entries are"
            };
            output.warn_with(
                Warning::ExportIncomplete,
                &format!(
                    "offline, so doi.org was not asked for the DOI metadata of {}: {entries} built from data.lock alone",
                    units::listed(&names, "and")
                ),
                context.offline.retry(),
            );
        }
        for (at, _) in &asked {
            cited[*at].unasked = Some("offline, so doi.org was not asked".to_owned());
        }
    } else if !asked.is_empty() {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|err| Error::new(Kind::LocalIo, format!("cannot ask doi.org: {err}")))?;
        let _waiting = output.wait(
            "Asking",
            format!(
                "doi.org for {}",
                units::count(asked.len() as u64, "DOI record", "DOI records")
            ),
        );
        let net = Arc::new(context.net());
        for (at, record) in runtime.block_on(records(&net, asked))? {
            match record {
                Ok(record) => cited[at].learn(&record),
                Err(error) => {
                    output.warn(
                        Warning::ExportIncomplete,
                        &format!(
                            "doi.org gave no record for the DOI of {}, so its entry is built from data.lock alone: {}",
                            cited[at].dataset, error.message
                        ),
                    );
                    cited[at].unasked = Some(error.message);
                }
            }
        }
    }
    let alone: Vec<&str> = cited
        .iter()
        .filter(|entry| entry.doi.is_none() && !entry.missing.is_empty())
        .map(|entry| entry.dataset.as_str())
        .collect();
    if !alone.is_empty() {
        let (has, its, entry) = if alone.len() == 1 {
            ("has", "its", "entry is")
        } else {
            ("have", "their", "entries are")
        };
        output.warn_with(
            Warning::ExportIncomplete,
            &format!(
                "{} {has} no DOI, so {its} {entry} built from data.lock alone and marked incomplete",
                units::listed(&alone, "and")
            ),
            "fill in the authors and year by hand before you cite them",
        );
    }
    Ok(cited)
}

/// the CSL-JSON record of each DOI of `asked`, by its place. fails at a stop.
async fn records(
    net: &Arc<crate::net::Net>,
    asked: Vec<(usize, Reference)>,
) -> Result<Vec<(usize, Result<serde_json::Value, Error>)>, Error> {
    let mut tasks = JoinSet::new();
    for (at, reference) in asked {
        let net = Arc::clone(net);
        tasks.spawn(async move {
            let record = match &reference {
                Reference::Doi(doi) => doi.csl(&*net).await,
                other => Err(Error::new(
                    Kind::ReferenceInvalid,
                    format!("{} is not a DOI", other.shown()),
                )),
            };
            (at, record)
        });
    }
    let mut records = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        let (at, record) = joined.map_err(|err| {
            Error::new(
                Kind::LocalIo,
                format!("a question to doi.org stopped unexpectedly: {err}"),
            )
        })?;
        if let Err(error) = &record
            && error.kind == Kind::Interrupted
        {
            tasks.abort_all();
            return Err(error.clone());
        }
        records.push((at, record));
    }
    records.sort_by_key(|(at, _)| *at);
    Ok(records)
}

/// the citations of `cited` in `format`: bibtex, csl or ris.
pub(super) fn render(cited: &[Cited], format: Format) -> String {
    match format {
        Format::Csl => csl(cited),
        Format::Ris => ris(cited),
        _ => bibtex(cited),
    }
}

/// `text` ended by one line break, or empty, for entries each followed by a blank line.
fn ended(mut text: String) -> String {
    text.truncate(text.trim_end_matches('\n').len());
    if !text.is_empty() {
        text.push('\n');
    }
    text
}

/// a BibTeX entry for each of `cited`, keyed by its dataset's name.
fn bibtex(cited: &[Cited]) -> String {
    let mut text = String::new();
    for entry in cited {
        let article = entry.kind == "article-journal";
        let _ = writeln!(
            text,
            "@{}{{{},",
            if article { "article" } else { "misc" },
            key(&entry.dataset)
        );
        let mut field = |name: &str, value: &str| {
            let _ = writeln!(text, "  {name} = {{{value}}},");
        };
        if !entry.authors.is_empty() {
            let names: Vec<String> = entry.authors.iter().map(bib_name).collect();
            field("author", &names.join(" and "));
        }
        field("title", &tex(entry.title()));
        if article && let Some(journal) = &entry.container {
            field("journal", &tex(journal));
        }
        if let Some(publisher) = &entry.publisher {
            field("publisher", &tex(publisher));
        }
        if let Some(year) = entry.year() {
            field("year", &year.to_string());
        }
        if let Some(version) = &entry.version {
            field("version", &tex(version));
        }
        if let Some(doi) = &entry.doi {
            field("doi", &verbatim(doi));
        }
        if let Some(url) = &entry.url {
            field("url", &verbatim(url));
        }
        if let Some(accessed) = &entry.accessed {
            field("urldate", accessed);
        }
        if let Some(license) = &entry.license {
            field("copyright", &tex(license));
        }
        if let Some(note) = entry.note(false) {
            field("note", &tex(&note));
        }
        text.push_str("}\n\n");
    }
    ended(text)
}

/// a dataset's name as a BibTeX key, which holds no space, comma, brace or quote.
fn key(dataset: &str) -> String {
    dataset
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// an author as BibTeX reads one: `family, given`, or a group's name kept whole in braces.
fn bib_name(name: &Name) -> String {
    let whole = |part: &str| {
        let part = tex(part);
        // " and " splits authors, so a part holding it stays whole
        if part.to_ascii_lowercase().contains(" and ") {
            format!("{{{part}}}")
        } else {
            part
        }
    };
    match (&name.family, &name.given, &name.literal) {
        (Some(family), Some(given), _) => format!("{}, {}", whole(family), whole(given)),
        (Some(family), None, _) => whole(family),
        (None, _, Some(literal)) => format!("{{{}}}", tex(literal)),
        (None, _, None) => String::new(),
    }
}

/// `text` on one line for a BibTeX field in braces: LaTeX's special characters escaped, and a
/// brace written as a command, so no text can close the field or start another entry.
fn tex(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in words(text).chars() {
        match c {
            '\\' => escaped.push_str("\\textbackslash{}"),
            '{' => escaped.push_str("\\textbraceleft{}"),
            '}' => escaped.push_str("\\textbraceright{}"),
            '^' => escaped.push_str("\\textasciicircum{}"),
            '~' => escaped.push_str("\\textasciitilde{}"),
            '%' | '&' | '$' | '#' | '_' => {
                escaped.push('\\');
                escaped.push(c);
            }
            c => escaped.push(c),
        }
    }
    escaped
}

/// a link or DOI for a BibTeX field read as written, as biblatex reads `url` and `doi`: the few
/// characters that would end the field or start a command escaped as a link escapes them.
fn verbatim(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in words(text).chars() {
        match c {
            '{' => escaped.push_str("%7B"),
            '}' => escaped.push_str("%7D"),
            '\\' => escaped.push_str("%5C"),
            ' ' => escaped.push_str("%20"),
            c => escaped.push(c),
        }
    }
    escaped
}

/// an RIS record for each of `cited`.
fn ris(cited: &[Cited]) -> String {
    let mut text = String::new();
    for entry in cited {
        let mut line = |tag: &str, value: &str| {
            let value = words(value);
            if !value.is_empty() {
                let _ = writeln!(text, "{tag}  - {value}");
            }
        };
        line(
            "TY",
            match entry.kind.as_str() {
                "article-journal" => "JOUR",
                "article" => "GEN",
                _ => "DATA",
            },
        );
        for author in &entry.authors {
            line("AU", &plain_name(author));
        }
        line("TI", entry.title());
        if let Some(journal) = &entry.container {
            line("T2", journal);
        }
        if let Some(year) = entry.year() {
            line("PY", &year.to_string());
        }
        if let Some(publisher) = &entry.publisher {
            line("PB", publisher);
        }
        if let Some(version) = &entry.version {
            line("ET", version);
        }
        if let Some(doi) = &entry.doi {
            line("DO", doi);
        }
        if let Some(url) = &entry.url {
            line("UR", url);
        }
        if let Some(accessed) = &entry.accessed {
            line("Y2", &accessed.replace('-', "/"));
        }
        if let Some(note) = entry.note(true) {
            line("N1", &note);
        }
        text.push_str("ER  - \n\n");
    }
    ended(text)
}

/// an author as one line names one: `family, given`, or a group's name.
fn plain_name(name: &Name) -> String {
    match (&name.family, &name.given, &name.literal) {
        (Some(family), Some(given), _) => format!("{family}, {given}"),
        (Some(family), None, _) => family.clone(),
        (None, _, Some(literal)) => literal.clone(),
        (None, _, None) => String::new(),
    }
}

/// a CSL-JSON item, as a citation processor reads one.
#[derive(Serialize)]
struct Item<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    title: &'a str,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    author: &'a [Name],
    #[serde(skip_serializing_if = "Option::is_none")]
    issued: Option<Date>,
    #[serde(skip_serializing_if = "Option::is_none")]
    publisher: Option<&'a str>,
    #[serde(rename = "container-title", skip_serializing_if = "Option::is_none")]
    container: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<&'a str>,
    #[serde(rename = "DOI", skip_serializing_if = "Option::is_none")]
    doi: Option<&'a str>,
    #[serde(rename = "URL", skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    accessed: Option<Date>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Serialize)]
struct Date {
    #[serde(rename = "date-parts")]
    parts: [Vec<i64>; 1],
}

/// a CSL-JSON list of `cited`, as pandoc, zotero and other citation processors read it.
fn csl(cited: &[Cited]) -> String {
    let items: Vec<Item<'_>> = cited
        .iter()
        .map(|entry| Item {
            id: &entry.dataset,
            kind: &entry.kind,
            title: entry.title(),
            author: &entry.authors,
            issued: (!entry.issued.is_empty()).then(|| Date {
                parts: [entry.issued.clone()],
            }),
            publisher: entry.publisher.as_deref(),
            container: entry.container.as_deref(),
            version: entry.version.as_deref(),
            doi: entry.doi.as_deref(),
            url: entry.url.as_deref(),
            accessed: entry.accessed.as_deref().and_then(|day| {
                let parts: Vec<i64> = day.split('-').map_while(|part| part.parse().ok()).collect();
                (parts.len() == 3).then_some(Date { parts: [parts] })
            }),
            note: entry.note(true),
        })
        .collect();
    let mut text = serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_owned());
    text.push('\n');
    text
}

/// one dataset of a data availability statement.
pub(super) struct Stated<'a> {
    pub(super) locked: &'a Locked,
    pub(super) imported: bool,
    /// data.lock's link for it, or the page its ref names, when it has one a reader can open.
    pub(super) link: Option<String>,
    /// its files, by their path in it, with their SHA-256, or the MD5 its source states.
    pub(super) files: Vec<(&'a str, String)>,
    /// what pins `files` while no sync downloaded it, as
    /// [`crate::project::lock::Stated::digests`] says it.
    pub(super) by_source: Option<&'static str>,
}

/// a data availability statement for a paper: each of `datasets` with its DOI or link and the
/// SHA-256 of each of its files, as data.lock pins them, or the MD5 its source states.
pub(super) fn statement(datasets: &[Stated<'_>]) -> String {
    let mut text = String::from("Data availability\n\n");
    let (count, each) = match datasets.len() {
        1 => ("1 dataset".to_owned(), "It is"),
        many => (
            format!("{} datasets", units::grouped(many as u64)),
            "Each is",
        ),
    };
    let md5 = datasets.iter().any(|dataset| {
        dataset
            .by_source
            .is_some_and(|digests| digests.contains("MD5"))
    });
    let md5 = if md5 {
        ", or for a file not downloaded yet the MD5 its source states"
    } else {
        ""
    };
    let _ = writeln!(
        text,
        "This work uses {count}. {each} listed below with its DOI or link and the SHA-256 checksum of every file used{md5}, so that readers can check that their copies are identical.\n"
    );
    for dataset in datasets {
        let locked = dataset.locked;
        let mut said = vec![];
        if let Some(title) = locked
            .title
            .as_deref()
            .map(words)
            .filter(|title| !title.is_empty())
        {
            said.push(title);
        }
        let doi = locked.doi.as_deref().and_then(bare_doi);
        if dataset.imported {
            said.push("imported from a local folder, with no public source".to_owned());
        } else if let Some(doi) = doi {
            said.push(doi_link(&doi));
        } else if let Some(link) = &dataset.link {
            match version(&locked.resolved) {
                Some(version) => said.push(format!("{link} at version {version}")),
                None => said.push(link.clone()),
            }
        } else {
            let written = match version(&locked.resolved) {
                Some(version) => {
                    let base = locked
                        .resolved
                        .rsplit_once('@')
                        .map_or("", |(base, _)| base);
                    format!("{} at version {version}", words(base))
                }
                None => words(&locked.resolved),
            };
            said.push(format!("{written} (no DOI or web link is recorded)"));
        }
        said.push(units::count(dataset.files.len() as u64, "file", "files"));
        if let Some(digests) = dataset.by_source {
            said.push(format!(
                "not downloaded yet, so by the {digests} its source states"
            ));
        }
        let _ = writeln!(text, "{}: {}:", words(&locked.name), said.join(", "));
        for (path, checksum) in &dataset.files {
            let _ = writeln!(text, "{checksum}  {}", words(path));
        }
        text.push('\n');
    }
    ended(text)
}

/// tells that the citations leave out `imported`, which no source holds.
pub(super) fn tell_imported(output: &Output, imported: &[&str]) {
    if imported.is_empty() {
        return;
    }
    let (it, was) = if imported.len() == 1 {
        ("it", "it was")
    } else {
        ("them", "they were")
    };
    output.warn(
        Warning::ExportLeftOut,
        &format!(
            "the citations leave out {}: {was} imported from a folder on this machine, so no source of {it} can be cited",
            units::listed(imported, "and")
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked(name: &str, title: Option<&str>, doi: Option<&str>) -> Locked {
        Locked {
            name: name.to_owned(),
            reference: "https://example.org/a.csv".to_owned(),
            spec: String::new(),
            extract: true,
            decompress: false,
            web_page: false,
            resolved: "https://example.org/a.csv".to_owned(),
            title: title.map(str::to_owned),
            license: Some("cc-by-4.0".to_owned()),
            doi: doi.map(str::to_owned),
            access: crate::source::Access::Public,
            timestamp: "2026-11-13T00:09:08Z".to_owned(),
            from: Vec::new(),
            files: Vec::new(),
        }
    }

    /// a title or a name from a cloned data.lock or a DOI record cannot close its field, start
    /// another entry or run a command, in any form.
    #[test]
    fn words_from_outside_cannot_break_out_of_their_field() {
        let hostile =
            "x}, author = {Mallory}}\n@misc{evil, note = {\\input{/etc/passwd}}\rTY  - JOUR";
        let mut entry = Cited::of_lock(&locked("iris", Some(hostile), None), None);
        entry.authors = vec![Name {
            family: Some("O'Brien and Sons}".to_owned()),
            given: Some("A.\nER  - ".to_owned()),
            literal: None,
        }];
        let bib = bibtex(std::slice::from_ref(&entry));
        assert_eq!(bib.matches('{').count(), bib.matches('}').count(), "{bib}");
        assert_eq!(
            bib.lines().filter(|line| line.starts_with('@')).count(),
            1,
            "{bib}"
        );
        assert!(!bib.contains("\\input"), "{bib}");
        let ris = ris(std::slice::from_ref(&entry));
        let tags: Vec<&str> = ris.lines().map(|line| &line[..2]).collect();
        assert_eq!(tags, ["TY", "AU", "TI", "Y2", "N1", "ER"], "{ris}");
        let csl = csl(std::slice::from_ref(&entry));
        let read: serde_json::Value = serde_json::from_str(&csl).unwrap();
        assert_eq!(read[0]["title"], words(hostile));
        assert!(!words(hostile).contains(['\n', '\r']));
    }

    /// an entry built from data.lock alone says what it lacks, and one from a DOI's record takes
    /// its authors, year and publisher, keeping data.lock's license and pin date.
    #[test]
    fn a_doi_record_fills_what_data_lock_lacks() {
        let pin = locked("iris", Some("Iris"), Some("10.5281/zenodo.1"));
        let mut entry = Cited::of_lock(&pin, None);
        assert_eq!(entry.missing, ["author", "year"]);
        assert_eq!(
            entry.url.as_deref(),
            Some("https://doi.org/10.5281/zenodo.1")
        );
        entry.learn(&serde_json::json!({
            "type": "dataset",
            "title": "DatasetIris",
            "author": [{ "family": "Montero Loaiza", "given": "David" }, { "literal": "UCI" }, {}],
            "issued": { "date-parts": [[2020, 1, 15]] },
            "publisher": "Zenodo",
        }));
        assert_eq!(entry.from, "doi.org");
        assert!(entry.missing.is_empty(), "{:?}", entry.missing);
        assert_eq!(
            bibtex(std::slice::from_ref(&entry)),
            "@misc{iris,\n  author = {Montero Loaiza, David and {UCI}},\n  title = {DatasetIris},\n  publisher = {Zenodo},\n  year = {2020},\n  doi = {10.5281/zenodo.1},\n  url = {https://doi.org/10.5281/zenodo.1},\n  urldate = {2026-11-13},\n  copyright = {cc-by-4.0},\n}\n"
        );
        let alone = Cited::of_lock(&locked("uci", None, None), None);
        assert_eq!(alone.missing, ["author", "year", "title", "link"]);
        assert_eq!(
            alone.note(true).as_deref(),
            Some(
                "Incomplete: built from data.lock, which gives no author, year, title or link. License: cc-by-4.0."
            )
        );
    }

    #[test]
    fn a_doi_is_cited_without_the_prefix_a_source_wrote() {
        for written in [
            "10.5281/zenodo.1",
            "doi:10.5281/zenodo.1",
            "DOI:10.5281/zenodo.1",
            "https://doi.org/10.5281/zenodo.1",
            " http://dx.doi.org/10.5281/zenodo.1\n",
        ] {
            assert_eq!(
                bare_doi(written).as_deref(),
                Some("10.5281/zenodo.1"),
                "{written}"
            );
        }
        assert_eq!(bare_doi("doi:"), None);
    }

    #[test]
    fn a_version_after_an_at_is_one_only_where_the_source_writes_versions() {
        assert_eq!(version("kaggle:uciml/iris@2").as_deref(), Some("2"));
        assert_eq!(version("https://example.org/a@b.csv"), None);
        assert_eq!(version("zenodo:3608990"), None);
    }

    /// a dataset with no DOI whose ref has a page, or is a list at a link, links to that page or
    /// list, and the statement gives the version the page leaves out.
    #[test]
    fn a_dataset_without_a_doi_links_to_the_page_its_ref_names() {
        let commit = "0bda0ce801be0fa2f464ff845a9d5ceae99aad7d";
        for (resolved, page, version) in [
            (
                format!("hf:datasets/scikit-learn/iris@{commit}"),
                "https://huggingface.co/datasets/scikit-learn/iris",
                Some(commit),
            ),
            (
                "kaggle:uciml/iris@2".to_owned(),
                "https://www.kaggle.com/datasets/uciml/iris",
                Some("2"),
            ),
            (
                "ia:iris_20240626".to_owned(),
                "https://archive.org/details/iris_20240626",
                None,
            ),
            (
                "https://example.org/iris/".to_owned(),
                "https://example.org/iris/",
                None,
            ),
            (
                "list:https://physionet.org/files/mitdb/1.0.0/SHA256SUMS.txt".to_owned(),
                "https://physionet.org/files/mitdb/1.0.0/SHA256SUMS.txt",
                None,
            ),
        ] {
            let mut pin = locked("iris", Some("Iris"), None);
            pin.resolved = resolved;
            let link = super::super::link_of(&pin);
            let entry = Cited::of_lock(&pin, link.clone());
            assert_eq!(entry.url.as_deref(), Some(page), "{}", pin.resolved);
            assert_eq!(entry.missing, ["author", "year"], "{}", pin.resolved);
            let text = statement(&[Stated {
                locked: &pin,
                imported: false,
                link,
                files: Vec::new(),
                by_source: None,
            }]);
            let said = match version {
                Some(version) => format!("iris: Iris, {page} at version {version}, 0 files:\n"),
                None => format!("iris: Iris, {page}, 0 files:\n"),
            };
            assert!(text.contains(&said), "{said} in\n{text}");
        }
        // a list in the project has no link a reader can open
        let mut pin = locked("iris", Some("Iris"), None);
        pin.resolved = "list:lists/files.txt".to_owned();
        assert_eq!(super::super::link_of(&pin), None);
    }
}
