//! public catalog search and explicit selection for add and get.

use serde::Serialize;

use super::{Context, args};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::output::Output;
use crate::source::local;
use crate::source::search::{self, Candidate, Catalog};
use crate::units;

#[derive(Serialize)]
pub(super) struct Report {
    candidates: Vec<Candidate>,
}

pub(super) async fn run(context: &Context, args: &args::Search) -> Result<Report, Error> {
    let words = args.words.join(" ");
    let limit = usize::from(args.limit);
    let report = find(context, &context.net(), &words, &args.source, limit).await?;
    let output = &context.output;
    output.done(
        "Found",
        &format!(
            "{} for \"{words}\"",
            units::count(report.candidates.len() as u64, "dataset", "datasets")
        ),
    );
    // without an interactive terminal, as under CI, the references on stdout are the list
    if output.watched_story() {
        let parts = parts(output, &report.candidates);
        let pad = widest(parts.iter().map(|(reference, _, _)| reference.as_str()))
            .min(crate::output::REFERENCE_COLUMN);
        for (number, (reference, title, facts)) in parts.iter().enumerate() {
            output.candidate(number + 1, reference, pad, title, facts);
        }
        for candidate in &report.candidates {
            output.answer_untold(&line(candidate));
        }
    } else {
        for candidate in &report.candidates {
            output.answer(&line(candidate));
        }
    }
    if limit < 20 && more(&report.candidates, limit) {
        output.hint(&format!(
            "each site's first {limit} matches are shown; `--limit 20` shows up to 20 from each"
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
            "`fetchloom add {}` puts the first match in a project",
            super::shell_word(&first.reference.to_string())
        ));
    }
    Ok(report)
}

/// refuses `text`, given to add or get, before any request when it names something on this
/// machine: a path, or words that name a file or folder in the current folder.
pub(super) fn refuse_local(text: &str) -> Result<(), Error> {
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
    Err(Error::new(
        Kind::Usage,
        format!("\"{word}\" is a {what} here, so fetchloom did not search for it"),
    )
    .help(format!(
        "run `fetchloom add {}` to pin it, or `fetchloom search {}` to search",
        super::shell_word(&format!("./{word}")),
        super::shell_word(word)
    )))
}

pub(super) async fn pick(
    context: &Context,
    api: &impl Api,
    words: &str,
    command: &str,
) -> Result<String, Error> {
    let report = find(context, api, words, &[], search::LIMIT).await?;
    let rows = rows(&context.output, &report.candidates);
    let footer = if more(&report.candidates, search::LIMIT) {
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
        Ok(reference)
    } else {
        let first = &report.candidates[0].reference;
        let mut error = Error::new(Kind::ReferenceAmbiguous, format!(
                "\"{words}\" is not a link or an id, and fetchloom will not guess which dataset you mean"
            )).help(format!("run it again with one of these, such as `fetchloom {command} {first}`"))
                .with_result(serde_json::json!({"candidates": report.candidates}));
        error.details = rows;
        Err(error)
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
    limit: usize,
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
    let asked: Vec<&str> = Catalog::ALL
        .into_iter()
        .filter(|catalog| catalogs.is_empty() || catalogs.contains(catalog))
        .map(Catalog::label)
        .collect();
    let named = match asked.as_slice() {
        [first @ .., last] if !first.is_empty() => format!("{} and {last}", first.join(", ")),
        _ => asked.join(""),
    };
    context
        .output
        .step("Searching", &format!("{named} for \"{words}\""));
    let result = search::search(api, words, catalogs, limit).await?;
    for warning in result.warnings {
        context.output.warn(&warning);
    }
    if result.candidates.is_empty() {
        return Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("found no dataset matching \"{words}\""),
        )
        .help("try other words, or paste a link or id such as zenodo:3608990")
        .with_result(serde_json::json!({"candidates": []})));
    }
    Ok(Report {
        candidates: result.candidates,
    })
}

/// one candidate for a script reading stdout: its reference first, then its title and, when
/// known, its size, separated by tabs so the first column stays the reference.
fn line(candidate: &Candidate) -> String {
    let shown: String = candidate
        .title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let title = units::clip(&shown, usize::MAX, "");
    let mut line = format!("{}\t{title}", candidate.reference);
    if let Some(size) = candidate.size {
        line.push('\t');
        line.push_str(&units::size(size));
    }
    line
}

/// each candidate as what to type, its title and its facts, padded so every row has the same
/// columns: title, size, downloads, note. a cell the catalog leaves out stays blank and a column
/// nobody fills is left out. hugging face titles are the repository id the reference already
/// shows, so they stay blank. the title is clipped to what the terminal leaves after the facts.
fn parts(output: &Output, candidates: &[Candidate]) -> Vec<(String, String, String)> {
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
                    .map(|count| format!("{} downloads", units::compact(count)))
                    .unwrap_or_default(),
                candidate.note.clone().unwrap_or_default(),
            ]
        })
        .collect();
    let widths: Vec<usize> = (0..3)
        .map(|column| widest(cells.iter().map(|row| row[column].as_str())))
        .collect();
    let facts: Vec<String> = cells
        .iter()
        .map(|row| {
            row.iter()
                .zip(&widths)
                .enumerate()
                .filter(|(_, (_, width))| **width > 0)
                .map(|(column, (cell, width))| pad(cell, *width, column < 2))
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect();
    let facts_width = widest(facts.iter().map(String::as_str));
    let reference_width =
        widest(references.iter().map(String::as_str)).min(crate::output::REFERENCE_COLUMN);
    let most = output
        .width()
        .map_or(52, |width| {
            width.saturating_sub(12 + reference_width + 4 + facts_width)
        })
        .clamp(12, 52);
    let titles: Vec<String> = candidates
        .iter()
        .zip(&references)
        .map(|(candidate, reference)| {
            if reference.ends_with(&candidate.title) {
                return String::new();
            }
            let shown: String = candidate
                .title
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            output.clip(&shown, most)
        })
        .collect();
    let title_width = widest(titles.iter().map(String::as_str));
    references
        .into_iter()
        .zip(titles)
        .zip(facts)
        .map(|((reference, title), facts)| {
            let title = if facts.trim().is_empty() {
                title
            } else {
                pad(&title, title_width, false)
            };
            (reference, title, facts.trim_end().to_owned())
        })
        .collect()
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

/// the candidates as plain aligned lines, for the picker and for an error's details.
fn rows(output: &Output, candidates: &[Candidate]) -> Vec<String> {
    let parts = parts(output, candidates);
    let width = widest(parts.iter().map(|(reference, _, _)| reference.as_str()))
        .min(crate::output::REFERENCE_COLUMN);
    parts
        .into_iter()
        .map(|(reference, title, facts)| {
            let mut row = format!("{reference:width$}");
            for part in [title, facts] {
                if !part.is_empty() {
                    row.push_str("  ");
                    row.push_str(&part);
                }
            }
            row.trim_end().to_owned()
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Level, Style};
    use crate::source::Reference;

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
        ];
        let rows = rows(&output, &candidates);
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
            ]
        );
        assert_eq!(rows[0].len(), rows[2].len(), "downloads end in one column");
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
        let rows = rows(&output, &candidates);
        assert_eq!(rows[0].find("Iris Species"), Some(50), "{}", rows[0]);
        assert!(rows[1].starts_with(&format!("{long}  ")), "{}", rows[1]);
    }
}
