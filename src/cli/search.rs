//! public catalog search and explicit selection for add and get.

use serde::Serialize;

use super::{Context, args};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::output::Output;
use crate::source::search::{self, Candidate};
use crate::units;

#[derive(Serialize)]
pub(super) struct Report {
    candidates: Vec<Candidate>,
}

pub(super) async fn run(context: &Context, args: &args::Search) -> Result<Report, Error> {
    let words = args.words.join(" ");
    let report = find(context, &context.net(), &words).await?;
    let output = &context.output;
    output.done(
        "Found",
        &format!(
            "{} for \"{words}\"",
            units::count(report.candidates.len() as u64, "dataset", "datasets")
        ),
    );
    for (number, row) in rows(output, &report.candidates).iter().enumerate() {
        output.step(&(number + 1).to_string(), row);
    }
    for candidate in &report.candidates {
        output.answer_untold(&candidate.reference.to_string());
    }
    Ok(report)
}

pub(super) async fn pick(
    context: &Context,
    api: &impl Api,
    words: &str,
    command: &str,
) -> Result<String, Error> {
    let report = find(context, api, words).await?;
    let rows = rows(&context.output, &report.candidates);
    if let Some(index) = context.output.choose("which one?", &rows)? {
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

async fn find(context: &Context, api: &impl Api, words: &str) -> Result<Report, Error> {
    if context.offline.enabled() {
        return Err(Error::new(
            Kind::OfflineMissing,
            format!("searching for \"{words}\" needs the internet"),
        )
        .help("paste a link or an id instead, such as zenodo:3608990, or search again online"));
    }
    context.output.step(
        "Searching",
        &format!("hugging face, kaggle and zenodo for \"{words}\""),
    );
    let result = search::search(api, words).await?;
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

fn rows(output: &Output, candidates: &[Candidate]) -> Vec<String> {
    let references: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.reference.to_string())
        .collect();
    let width = references.iter().map(String::len).max().unwrap_or_default();
    candidates
        .iter()
        .zip(references)
        .map(|(candidate, reference)| {
            // hugging face titles are the repository id the reference already shows
            let mut facts: Vec<String> = Vec::new();
            if !reference.ends_with(&candidate.title) {
                facts.push(output.clip(&candidate.title, 52));
            }
            facts.extend(candidate.size.map(units::size));
            facts.extend(
                candidate
                    .downloads
                    .map(|count| format!("{} downloads", units::compact(count))),
            );
            facts.extend(candidate.note.iter().cloned());
            format!("{reference:width$}  {}", facts.join(" \u{b7} "))
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Level, Style};
    use crate::source::Reference;

    #[test]
    fn rows_align_references_and_omit_unknown_metadata() {
        let output = Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
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
        assert_eq!(
            rows(&output, &candidates),
            [
                "kaggle:uciml/iris  Iris Species \u{b7} 3.6 KiB \u{b7} 12k downloads",
                "zenodo:1           Iris \u{b7} needs access",
                "hf:datasets/a/b    950 downloads",
            ]
        );
    }
}
