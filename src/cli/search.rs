//! public catalog search and explicit selection for add and get.

use serde::Serialize;

use super::{Context, args};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::output::{Output, Warning};
use crate::source::local;
use crate::source::search::{self, Candidate, Catalog};
use crate::units;

#[derive(Serialize)]
pub(super) struct Report {
    candidates: Vec<Candidate>,
    /// the matches found but not shown, which `--limit` would show.
    #[serde(skip)]
    left_out: usize,
}

pub(super) async fn run(context: &Context, args: &args::Search) -> Result<Report, Error> {
    let words = args.words.join(" ");
    let limit = args.limit.map(usize::from);
    let report = find(context, &context.net(), &words, &args.source, limit).await?;
    let output = &context.output;
    output.done(
        "Found",
        &format!("{} for \"{words}\"", counted(&report.candidates)),
    );
    // without an interactive terminal, as under CI, the references on stdout are the list
    if output.watched_story() {
        let parts = parts(output, &report.candidates, output.width(), LIST_LEAD);
        let pad = widest(parts.iter().map(|shown| shown.reference.as_str()))
            .min(crate::output::REFERENCE_COLUMN);
        for shown in &parts {
            output.candidate(
                &shown.reference,
                pad,
                &shown.title,
                (&shown.facts, shown.below),
            );
        }
        for candidate in &report.candidates {
            output.answer_untold(&line(candidate));
        }
    } else {
        for candidate in &report.candidates {
            output.answer(&line(candidate));
        }
    }
    let each = limit.unwrap_or(search::LIMIT);
    if report.left_out > 0 {
        output.hint(&format!(
            "each site's best matches are shown, {} in all; `--limit 20` shows up to 20 from each",
            report.candidates.len()
        ));
    } else if each < 20 && more(&report.candidates, each) {
        let first = if each == 1 {
            "first match is".to_owned()
        } else {
            format!("first {each} matches are")
        };
        output.hint(&format!(
            "each site's {first} shown; `--limit 20` shows up to 20 from each"
        ));
    }
    if report
        .candidates
        .iter()
        .any(|candidate| candidate.size.is_some())
    {
        output.hint(
            "sizes are as each site lists them, and a zipped download is larger once unpacked",
        );
    }
    if let Some(first) = report.candidates.first() {
        output.next(&format!(
            "`{}` puts the first match in a project",
            super::project::command(&format!(
                "add {}",
                super::shell_word(&first.reference.to_string())
            ))
        ));
    }
    Ok(report)
}

/// refuses `text`, given to add or get, before any request when it names something on this
/// machine, a path or a name in the current folder; `pins` says the command can pin it, as add can.
pub(super) fn refuse_local(text: &str, pins: bool) -> Result<(), Error> {
    if local::is_path(text) {
        return Err(local::refused(text));
    }
    let word = text.trim();
    if !search::is_search(word) {
        return Ok(());
    }
    let Some(what) = std::env::current_dir()
        .ok()
        .and_then(|here| local::here(word, &here))
    else {
        return Ok(());
    };
    let search = format!("`fetchloom search {}` to search", super::shell_word(word));
    let help = if pins {
        format!(
            "run `{}` to pin it, or {search}",
            super::project::command(&format!("add {}", super::shell_word(&format!("./{word}"))))
        )
    } else {
        format!("run {search}")
    };
    Err(Error::new(
        Kind::Usage,
        format!("\"{word}\" is a {what} here, so fetchloom did not search for it"),
    )
    .help(help))
}

pub(super) async fn pick(
    context: &Context,
    api: &impl Api,
    words: &str,
    command: &str,
) -> Result<String, Error> {
    let report = find(context, api, words, &[], None).await?;
    let rows = rows(
        &context.output,
        &report.candidates,
        context.output.width(),
        crate::output::picker_lead(report.candidates.len()),
    );
    let footer = if report.left_out > 0 || more(&report.candidates, search::LIMIT) {
        // words a shell would read differently are not offered as something to paste
        let safe = !words.trim_start().starts_with('-')
            && words
                .chars()
                .all(|c| c.is_alphanumeric() || " -_./".contains(c));
        let typed = if safe { words.trim() } else { "<words>" };
        format!("more matches: fetchloom search {typed} --limit 20")
    } else {
        String::new()
    };
    let chosen = context
        .output
        .choose("which one?", &rows, &footer)
        .map_err(|error| error.cancelling("the choice of a dataset, so nothing changed"))?;
    if let Some(index) = chosen {
        let reference = report.candidates[index].reference.to_string();
        context.output.step("Chose", &reference);
        return Ok(reference);
    }
    let first = &report.candidates[0].reference;
    let mut error = Error::new(Kind::ReferenceAmbiguous, format!(
            "\"{words}\" is not a link or an id, and fetchloom will not guess which dataset you mean"
        )).help(format!(
            "run it again with one of these, such as `{}`",
            super::project::command(&format!("{command} {first}"))
        ))
            .with_result(serde_json::json!({"candidates": report.candidates}));
    let output = &context.output;
    if !output.shows_items() {
        error.details = rows;
        return Err(error);
    }
    // what a person reads first is the list to add from, in the width of their terminal, as
    // with `TERM=dumb`, which opens no picker; the error after it says why nothing was added
    output.done(
        "Found",
        &format!(
            "{} for \"{words}\"; add one by its id:",
            counted(&report.candidates)
        ),
    );
    let width = console::Term::stderr()
        .size_checked()
        .map(|(_, columns)| usize::from(columns));
    let parts = parts(output, &report.candidates, width, LIST_LEAD);
    let pad = widest(parts.iter().map(|shown| shown.reference.as_str()))
        .min(crate::output::REFERENCE_COLUMN);
    for shown in parts {
        // facts that do not fit beside the reference go on a line of their own under it
        if shown.below {
            output.item(&shown.reference);
            output.item(&format!("  {}", shown.facts));
            continue;
        }
        let row = [shown.title.as_str(), shown.facts.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .fold(format!("{:pad$}", shown.reference), |row, part| {
                format!("{row}  {part}")
            });
        output.item(row.trim_end());
    }
    Err(error)
}

/// the columns before a reference in the list `search` prints.
const LIST_LEAD: usize = 12;

/// how many `candidates` there are, by what they are: `3 datasets`, `2 papers`, or
/// `1 dataset and 2 papers`.
fn counted(candidates: &[Candidate]) -> String {
    let papers = candidates
        .iter()
        .filter(|candidate| candidate.reference.is_paper())
        .count() as u64;
    let datasets = candidates.len() as u64 - papers;
    let (datasets, papers) = (
        units::count(datasets, "dataset", "datasets"),
        (papers > 0).then(|| units::count(papers, "paper", "papers")),
    );
    match papers {
        None => datasets,
        Some(papers)
            if candidates
                .iter()
                .all(|candidate| candidate.reference.is_paper()) =>
        {
            papers
        }
        Some(papers) => format!("{datasets} and {papers}"),
    }
}

/// what a search of the `asked` catalogs that found nothing found none of.
fn unmatched(asked: &[Catalog]) -> &'static str {
    let papers = asked.iter().any(|catalog| catalog.finds_papers());
    let datasets = asked.iter().any(|catalog| !catalog.finds_papers());
    match (datasets, papers) {
        (true, true) => "dataset or paper",
        (false, true) => "paper",
        _ => "dataset",
    }
}

/// whether a catalog filled its share, so it may hold more matches than were shown.
fn more(candidates: &[Candidate], limit: usize) -> bool {
    candidates.iter().any(|candidate| {
        candidates
            .iter()
            .filter(|other| other.source == candidate.source)
            .count()
            >= limit
    })
}

async fn find(
    context: &Context,
    api: &impl Api,
    words: &str,
    catalogs: &[Catalog],
    limit: Option<usize>,
) -> Result<Report, Error> {
    if words.trim().is_empty() {
        return Err(Error::new(Kind::Usage, "there are no words to search for")
            .help("name what you are looking for, as in `fetchloom search iris`"));
    }
    if let Some(path) = words.split_whitespace().find(|word| local::is_path(word)) {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "\"{path}\" is a path on this machine, and fetchloom does not send paths to search sites"
            ),
        )
        .help("search with words that describe the data, as in `fetchloom search iris`"));
    }
    if context.offline.enabled() {
        return Err(Error::new(
            Kind::OfflineMissing,
            format!("searching for \"{words}\" needs the internet"),
        )
        .help("search again online. offline, `fetchloom sync --offline` places what a project already pinned"));
    }
    if context.mirrors.mirrors.only {
        return Err(super::fetch::mirrors::refused(
            context,
            &format!("searching for \"{words}\" asks sites such as hugging face"),
        ));
    }
    let asked = Catalog::asked(catalogs);
    let labels: Vec<&str> = asked.iter().copied().map(Catalog::label).collect();
    let named = units::listed(&labels, "and");
    context
        .output
        .step("Searching", &format!("{named} for \"{words}\""));
    let result = search::search(api, words, catalogs, limit).await;
    // a stop cuts short the catalogs still searching, so what the others found is no choice
    context.stop.check()?;
    let result = result?;
    for warning in result.warnings {
        context.output.warn(Warning::SearchPartial, &warning);
    }
    if result.candidates.is_empty() {
        return Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("found no {} matching \"{words}\"", unmatched(&asked)),
        )
        .help("try other words, or paste a link or id such as zenodo:3608990")
        .with_result(serde_json::json!({"candidates": []})));
    }
    Ok(Report {
        candidates: result.candidates,
        left_out: result.left_out,
    })
}

/// one candidate for a script reading stdout: its reference first, then its title and, when
/// known, its size, separated by tabs so the first column stays the reference. a control in the
/// title reads as a space, and a bidi control as its escape, as the story writes one.
fn line(candidate: &Candidate) -> String {
    let mut shown = String::with_capacity(candidate.title.len());
    for c in candidate.title.chars() {
        if c.is_control() {
            shown.push(' ');
        } else if crate::name::is_bidi_control(c) {
            shown.extend(c.escape_default());
        } else {
            shown.push(c);
        }
    }
    let title = units::clip(&shown, usize::MAX, "");
    let mut line = format!("{}\t{title}", candidate.reference);
    if let Some(size) = candidate.size {
        line.push('\t');
        line.push_str(&units::size(size));
    }
    line
}

/// one candidate as a person reads it: what to type, its title and its facts, already padded
/// into columns, and whether the facts take a line of their own under the reference.
struct Shown {
    reference: String,
    title: String,
    facts: String,
    below: bool,
}

/// the fewest columns a title keeps before it gives way altogether, and the most it takes.
const TITLE_LEAST: usize = 12;
const TITLE_MOST: usize = 52;

/// each candidate as what to type, its title and its facts, in columns every row shares: title,
/// size, downloads, note, a missing cell blank and an empty column left out. hugging face titles
/// stay blank, as the reference names the repository. in `width` columns after `lead`, downloads
/// give way first, then the title; size and note go under the reference when they do not fit.
fn parts(
    output: &Output,
    candidates: &[Candidate],
    width: Option<usize>,
    lead: usize,
) -> Vec<Shown> {
    let references: Vec<String> = candidates
        .iter()
        .map(|candidate| candidate.reference.to_string())
        .collect();
    let cells: Vec<[String; 3]> = candidates
        .iter()
        .map(|candidate| {
            [
                candidate.size.map(units::size).unwrap_or_default(),
                candidate
                    .downloads
                    .map(worded_downloads)
                    .unwrap_or_default(),
                candidate.note.clone().unwrap_or_default(),
            ]
        })
        .collect();
    let reference_width =
        widest(references.iter().map(String::as_str)).min(crate::output::REFERENCE_COLUMN);
    // what a row leaves after its reference and two spaces, with the last column spare
    let room = |reference: &str| {
        width.map(|width| {
            width.saturating_sub(
                lead + console::measure_text_width(reference).max(reference_width) + 3,
            )
        })
    };
    let given: Vec<String> = candidates
        .iter()
        .zip(&references)
        .map(|(candidate, reference)| {
            if reference.ends_with(&candidate.title) {
                return String::new();
            }
            candidate
                .title
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect()
        })
        .collect();
    let any_title = given.iter().any(|title| !title.is_empty());
    let shared = room("");
    let more = console::measure_text_width(output.more());
    let mut plan = (false, None);
    for downloads in [true, false] {
        let facts_width = widest(facts(&cells, downloads).iter().map(String::as_str));
        let gap = if facts_width > 0 { 2 } else { 0 };
        match shared {
            None => {
                plan = (downloads, Some(TITLE_MOST));
                break;
            }
            Some(room) if any_title && room >= facts_width + gap + more + TITLE_LEAST => {
                plan = (
                    downloads,
                    Some((room - facts_width - gap - more).min(TITLE_MOST)),
                );
                break;
            }
            Some(room) if !any_title && facts_width <= room => {
                plan = (downloads, Some(TITLE_MOST));
                break;
            }
            Some(_) => {}
        }
    }
    let (downloads, most) = plan;
    let facts = facts(&cells, downloads);
    let titles: Vec<String> = given
        .iter()
        .map(|title| most.map_or_else(String::new, |most| output.clip(title, most)))
        .collect();
    let title_width = widest(titles.iter().map(String::as_str));
    references
        .into_iter()
        .zip(titles)
        .zip(facts)
        .map(|((reference, title), facts)| {
            let facts = facts.trim_end().to_owned();
            let title = if facts.is_empty() {
                title
            } else {
                pad(&title, title_width, false)
            };
            let rest = [title.as_str(), facts.as_str()]
                .iter()
                .filter(|part| !part.is_empty())
                .map(|part| console::measure_text_width(part))
                .fold(0, |sum, part| sum + part + 2)
                .saturating_sub(2);
            let below = room(&reference).is_some_and(|room| rest > room);
            Shown {
                title: if below { String::new() } else { title },
                reference,
                facts,
                below,
            }
        })
        .collect()
}

/// each row's facts in their columns: size, then downloads where `downloads` keeps them, then
/// the access note, numbers to the right.
fn facts(cells: &[[String; 3]], downloads: bool) -> Vec<String> {
    let widths: Vec<usize> = (0..3)
        .map(|column| widest(cells.iter().map(|row| row[column].as_str())))
        .collect();
    cells
        .iter()
        .map(|row| {
            row.iter()
                .zip(&widths)
                .enumerate()
                .filter(|(column, (_, width))| **width > 0 && (downloads || *column != 1))
                .map(|(column, (cell, width))| pad(cell, *width, column < 2))
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect()
}

/// a site's download count as a person reads it: `1 download`, `12k downloads`.
fn worded_downloads(count: u64) -> String {
    let noun = if count == 1 { "download" } else { "downloads" };
    format!("{} {noun}", units::compact(count))
}

fn widest<'a>(texts: impl Iterator<Item = &'a str>) -> usize {
    texts
        .map(console::measure_text_width)
        .max()
        .unwrap_or_default()
}

/// `text` filled with spaces to `width` columns, on the left when `right` aligns numbers.
fn pad(text: &str, width: usize, right: bool) -> String {
    let fill = " ".repeat(width.saturating_sub(console::measure_text_width(text)));
    if right {
        format!("{fill}{text}")
    } else {
        format!("{text}{fill}")
    }
}

/// the candidates as plain aligned lines, for the picker and for an error's details. a row whose
/// facts do not fit beside its reference has no title, so the picker can put them under it.
fn rows(
    output: &Output,
    candidates: &[Candidate],
    width: Option<usize>,
    lead: usize,
) -> Vec<String> {
    let parts = parts(output, candidates, width, lead);
    let width = widest(parts.iter().map(|shown| shown.reference.as_str()))
        .min(crate::output::REFERENCE_COLUMN);
    parts
        .into_iter()
        .map(|shown| {
            let mut row = format!("{:width$}", shown.reference);
            if !shown.title.is_empty() {
                row.push_str("  ");
                row.push_str(&shown.title);
            }
            if !shown.facts.is_empty() {
                row.push_str("  ");
                row.push_str(&shown.facts);
            }
            row.trim_end().to_owned()
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Level, Style, picker_lead};
    use crate::source::Reference;

    /// the count says what was found: datasets, papers or both.
    #[test]
    fn what_was_found_is_counted_as_datasets_papers_or_both() {
        let found = |references: &[&str]| {
            let candidates: Vec<Candidate> = references
                .iter()
                .map(|reference| Candidate {
                    reference: Reference::parse(reference).unwrap().reference,
                    title: String::new(),
                    source: "test",
                    size: None,
                    downloads: None,
                    note: None,
                })
                .collect();
            counted(&candidates)
        };
        assert_eq!(found(&["zenodo:1", "kaggle:uciml/iris"]), "2 datasets");
        assert_eq!(found(&["arxiv:1706.03762"]), "1 paper");
        assert_eq!(
            found(&["s2:CorpusId:1", "s2:CorpusId:2", "arxiv:1706.03762"]),
            "3 papers"
        );
        assert_eq!(
            found(&["zenodo:1", "arxiv:1706.03762", "s2:CorpusId:2"]),
            "1 dataset and 2 papers"
        );
        assert_eq!(unmatched(&[Catalog::Hf, Catalog::Zenodo]), "dataset");
        assert_eq!(unmatched(&[Catalog::Arxiv, Catalog::S2]), "paper");
        assert_eq!(unmatched(&[Catalog::Geo, Catalog::S2]), "dataset or paper");
    }

    #[test]
    fn script_lines_keep_the_reference_first() {
        let candidate = Candidate {
            reference: Reference::parse("kaggle:uciml/iris").unwrap().reference,
            title: "Iris\tSpecies\n\u{1b}x".to_owned(),
            source: "kaggle",
            size: Some(3687),
            downloads: None,
            note: None,
        };
        assert_eq!(
            line(&candidate),
            "kaggle:uciml/iris\tIris Species x\t3.6 KiB"
        );
        // a site's title cannot turn the line around on a terminal
        let reversed = Candidate {
            title: "Iris \u{202e}vsc.sirI".to_owned(),
            ..candidate
        };
        assert_eq!(
            line(&reversed),
            "kaggle:uciml/iris\tIris \\u{202e}vsc.sirI\t3.6 KiB"
        );
    }

    #[test]
    fn rows_keep_every_fact_in_its_column_and_leave_unknown_cells_blank() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let candidates = vec![
            Candidate {
                reference: Reference::parse("kaggle:uciml/iris").unwrap().reference,
                title: "Iris Species".to_owned(),
                source: "kaggle",
                size: Some(3687),
                downloads: Some(12_000),
                note: None,
            },
            Candidate {
                reference: Reference::parse("zenodo:1").unwrap().reference,
                title: "Iris".to_owned(),
                source: "zenodo",
                size: None,
                downloads: None,
                note: Some("needs access".to_owned()),
            },
            Candidate {
                reference: Reference::parse("hf:datasets/a/b").unwrap().reference,
                title: "a/b".to_owned(),
                source: "hugging face",
                size: None,
                downloads: Some(950),
                note: None,
            },
            Candidate {
                reference: Reference::parse("hf:datasets/c/d").unwrap().reference,
                title: "c/d".to_owned(),
                source: "hugging face",
                size: None,
                downloads: Some(1),
                note: None,
            },
        ];
        let rows = rows(&output, &candidates, None, picker_lead(candidates.len()));
        assert_eq!(
            rows,
            [
                "kaggle:uciml/iris  Iris Species  3.6 KiB  12k downloads".to_owned(),
                format!(
                    "zenodo:1{}Iris{}needs access",
                    " ".repeat(11),
                    " ".repeat(34)
                ),
                format!("hf:datasets/a/b{}950 downloads", " ".repeat(27)),
                format!("hf:datasets/c/d{}1 download", " ".repeat(30)),
            ]
        );
        assert_eq!(rows[0].len(), rows[2].len(), "downloads end in one column");
        assert_eq!(rows[0].len(), rows[3].len(), "downloads end in one column");
    }

    fn quiet_output() -> Output {
        Output::new(
            Style {
                color: false,
                unicode: true,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Quiet,
            false,
            false,
        )
    }

    /// at 60, 80 and 100 columns every row keeps its reference, access note and size whole,
    /// dropping downloads, then the title, and moves note and size under it when they do not fit.
    #[test]
    fn narrow_rows_keep_the_access_note_and_the_size_whole() {
        let candidate = |reference: &str, title: &str, size, downloads, gated: bool| Candidate {
            reference: Reference::parse(reference).unwrap().reference,
            title: title.to_owned(),
            source: "kaggle",
            size: Some(size),
            downloads: Some(downloads),
            note: gated.then(|| "needs access".to_owned()),
        };
        let candidates = vec![
            candidate(
                "hf:datasets/ILSVRC/imagenet-1k",
                "ILSVRC/imagenet-1k",
                166_022_000_000,
                44_000,
                true,
            ),
            candidate(
                "kaggle:himanshunakrani/iris-dataset",
                "Iris dataset",
                1006,
                117_000,
                false,
            ),
            candidate(
                "hf:datasets/fatdove/Iris_Database",
                "fatdove/Iris_Database",
                87_727_000_000,
                5000,
                false,
            ),
            candidate(
                "kaggle:arshid/iris-flower-dataset",
                "Iris Flower Dataset",
                1010,
                260_000,
                false,
            ),
        ];
        let output = quiet_output();
        for width in [60, 80, 100] {
            for lead in [LIST_LEAD, picker_lead(candidates.len())] {
                let parts = parts(&output, &candidates, Some(width), lead);
                let column = widest(parts.iter().map(|shown| shown.reference.as_str()));
                for (shown, candidate) in parts.iter().zip(&candidates) {
                    assert_eq!(shown.reference, candidate.reference.to_string());
                    let lines = if shown.below {
                        vec![shown.reference.clone(), shown.facts.clone()]
                    } else {
                        let mut line = format!("{:column$}", shown.reference);
                        for part in [&shown.title, &shown.facts] {
                            if !part.is_empty() {
                                line.push_str("  ");
                                line.push_str(part);
                            }
                        }
                        vec![line]
                    };
                    for line in &lines {
                        assert!(
                            console::measure_text_width(line.trim_end()) < width - lead,
                            "{width} {lead}: {line:?}"
                        );
                    }
                    let size = units::size(candidate.size.unwrap_or_default());
                    assert!(shown.facts.contains(&size), "{width} {lead}: {lines:?}");
                    if candidate.note.is_some() {
                        assert!(
                            shown.facts.contains("needs access"),
                            "{width} {lead}: {lines:?}"
                        );
                    }
                }
            }
        }
        let wide = parts(&output, &candidates, Some(200), LIST_LEAD);
        assert!(wide.iter().all(|shown| shown.facts.contains("downloads")));
        assert!(
            wide[1].title.starts_with("Iris dataset"),
            "{}",
            wide[1].title
        );
    }

    /// a chinese title is cut by the columns it shows, two for each of its characters, so it
    /// fits beside its facts as an ascii one does.
    #[test]
    fn a_wide_title_is_cut_to_the_columns_it_shows() {
        let candidate = |reference: &str, title: &str| Candidate {
            reference: Reference::parse(reference).unwrap().reference,
            title: title.to_owned(),
            source: "zenodo",
            size: Some(1_181_116_006),
            downloads: Some(2400),
            note: None,
        };
        let candidates = vec![
            candidate("kaggle:uciml/iris", "Iris Species"),
            candidate(
                "zenodo:23233774",
                "鸢尾花数据集：三种鸢尾花的花萼和花瓣的长度与宽度测量结果",
            ),
        ];
        let output = quiet_output();
        for width in [80, 100, 120] {
            let parts = parts(&output, &candidates, Some(width), LIST_LEAD);
            for shown in &parts {
                let line = format!("{:17}  {}  {}", shown.reference, shown.title, shown.facts);
                assert!(!shown.below, "{width}: {line:?}");
                assert!(
                    LIST_LEAD + console::measure_text_width(&line) < width,
                    "{width}: {line:?}"
                );
            }
            assert!(parts[1].title.trim_end().ends_with('…'), "{width}");
        }
    }

    /// `search iris` with `LC_ALL=C` at 100 columns: a title cut with `...` still fits beside
    /// its facts, so no row drops its title to put its size on a line of its own.
    #[test]
    fn a_cut_title_leaves_room_for_the_ellipsis_in_use() {
        let candidate = |reference: &str, title: &str, size, downloads| Candidate {
            reference: Reference::parse(reference).unwrap().reference,
            title: title.to_owned(),
            source: "zenodo",
            size: Some(size),
            downloads: Some(downloads),
            note: None,
        };
        let candidates = vec![
            candidate(
                "hf:datasets/scikit-learn/iris",
                "scikit-learn/iris",
                18_534,
                12_000,
            ),
            candidate("kaggle:uciml/iris", "Iris Species", 3687, 959_000),
            candidate(
                "zenodo:23233774",
                "IRIS: ICESat-2 River Surface Slope, a global river slope dataset",
                1_181_116_006,
                2400,
            ),
        ];
        for unicode in [true, false] {
            let output = Output::new(
                Style {
                    color: false,
                    unicode,
                    interactive: false,
                    watched: false,
                    links: false,
                    accessible: false,
                },
                Level::Quiet,
                false,
                false,
            );
            for width in 90..=110 {
                let parts = parts(&output, &candidates, Some(width), LIST_LEAD);
                let column = widest(parts.iter().map(|shown| shown.reference.as_str()));
                for shown in &parts {
                    let line = format!(
                        "{:column$}  {}  {}",
                        shown.reference, shown.title, shown.facts
                    );
                    assert!(!shown.below, "{unicode} {width}: {line:?}");
                    assert!(
                        LIST_LEAD + console::measure_text_width(&line) < width,
                        "{unicode} {width}: {line:?}"
                    );
                }
                assert!(
                    parts[1].title.starts_with("Iris Species"),
                    "{unicode} {width}"
                );
                assert!(
                    parts[2].title.trim_end().ends_with(output.more()),
                    "{unicode} {width}: {:?}",
                    parts[2].title
                );
            }
        }
    }

    #[test]
    fn one_long_reference_does_not_widen_every_row() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Quiet,
            false,
            false,
        );
        let long = format!("hf:datasets/a/{}", "x".repeat(80));
        let candidates = vec![
            Candidate {
                reference: Reference::parse("kaggle:uciml/iris").unwrap().reference,
                title: "Iris Species".to_owned(),
                source: "kaggle",
                size: Some(3687),
                downloads: None,
                note: None,
            },
            Candidate {
                reference: Reference::parse(&long).unwrap().reference,
                title: "Long".to_owned(),
                source: "hugging face",
                size: Some(3687),
                downloads: None,
                note: None,
            },
        ];
        let rows = rows(&output, &candidates, None, picker_lead(candidates.len()));
        assert_eq!(rows[0].find("Iris Species"), Some(50), "{}", rows[0]);
        assert!(rows[1].starts_with(&format!("{long}  ")), "{}", rows[1]);
    }
}
