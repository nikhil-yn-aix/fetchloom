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
    // a pipe a person reads at the same terminal, as `| less` or `| grep`, gets the rows alone:
    // lines told on stderr meanwhile showed each match twice and landed inside the pager's screen
    let piped = output.pipes_answer();
    if !piped {
        output.done(
            "Found",
            &format!("{} for \"{words}\"", counted(&report.candidates)),
        );
    }
    // without an interactive terminal, as under CI, the references on stdout are the list
    if output.watched_story() && !piped {
        let table = parts(output, &report.candidates, output.width(), LIST_LEAD);
        let pad = table.column;
        for shown in &table.rows {
            if shown.title_below {
                output.candidate_under(&shown.reference, &shown.title, &shown.facts);
            } else {
                output.candidate(
                    &shown.reference,
                    pad,
                    &shown.title,
                    (&shown.facts, shown.below),
                );
            }
        }
        for candidate in &report.candidates {
            output.answer_untold(&line(candidate));
        }
    } else {
        for candidate in &report.candidates {
            output.answer(&line(candidate));
        }
    }
    if piped {
        return Ok(report);
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
    let Ok(here) = std::env::current_dir() else {
        return Ok(());
    };
    let typed = text.trim();
    // quotes a shell passed on, as `'"flowers"'` does, are no part of a name here
    let unquoted = typed.trim_matches(['"', '\'']);
    // a path pasted with its own quotes, as windows' copy as path gives one inside a string,
    // names no file with them
    if unquoted != typed
        && !unquoted.is_empty()
        && (local::is_path(unquoted) || local::reads_as_path(unquoted))
    {
        if !pins {
            return Err(local::refused(unquoted));
        }
        let line = super::meant(
            &[text],
            &[unquoted],
            super::project::command(&format!("add {}", super::shell_word(unquoted))),
        );
        return Err(Error::new(
            Kind::Usage,
            format!("the path {unquoted} came with quotes around it"),
        )
        .help(format!(
            "quotes typed inside a quoted word stay in it: did you mean `{line}`?"
        )));
    }
    let word =
        if unquoted != typed && !unquoted.is_empty() && local::here(unquoted, &here).is_some() {
            unquoted
        } else {
            typed
        };
    let written_as_path = local::reads_as_path(word);
    if !written_as_path && !search::is_search(word) {
        return Ok(());
    }
    let Some(what) = local::here(word, &here) else {
        if written_as_path {
            return Err(no_such_path(word, &here, pins));
        }
        return Ok(());
    };
    // a person pins data a project keeps in data/ by moving it out first, which `./<word>` misses
    if pins && let Some(error) = super::import::word_in_data_folder(word, &here) {
        return Err(error);
    }
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

/// the refusal of `word`, written as a path that names nothing in `here`, never sent to a search
/// site: a typo of a near name is likelier, which `pins` says the command can import.
fn no_such_path(word: &str, here: &std::path::Path, pins: bool) -> Error {
    let missing = format!("there is no file or folder \"{word}\" here");
    if let Some(link) = as_link(word) {
        let alone = if pins {
            super::project::command(&format!("add {}", super::shell_word(&link)))
        } else {
            format!("fetchloom get {}", super::shell_word(&link))
        };
        let line = super::meant(&[word], &[&link], alone);
        return Error::new(Kind::Usage, missing).help(link_meant(&line, &link));
    }
    let search = format!(
        "run `fetchloom search {}` to search for these words",
        super::shell_word(word.trim_end_matches('\\'))
    );
    let guess = pins
        .then(|| super::import::near(word, &here.join(word)))
        .flatten();
    let help = match guess {
        Some(guess) => {
            let guess = format!("./{}", guess.trim_end_matches(['/', '\\']));
            let line = super::meant(
                &[word],
                &[&guess],
                super::project::command(&format!("add {}", super::shell_word(&guess))),
            );
            format!("did you mean `{line}`? or {search}")
        }
        None if pins => format!("check the path, or {search}"),
        None => format!("give a link, a DOI or an id, or {search}"),
    };
    Error::new(Kind::Usage, missing).help(help)
}

/// `word` as a link when it reads as one without its scheme, a host and a port if any, then a
/// path: http for this machine's name or an address, as a server a person runs answers, as
/// `localhost:8000/data.csv` does, and https for a host name of two or more parts, the last of
/// letters alone, as `zenodo.org/records/1`.
pub(super) fn as_link(word: &str) -> Option<String> {
    let (host, _) = word.split_once('/')?;
    let name = match host.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']').map(|(inside, _)| inside)?,
        None => host.split_once(':').map_or(host, |(name, _)| name),
    };
    let addressed = name.eq_ignore_ascii_case("localhost")
        || name.parse::<std::net::Ipv4Addr>().is_ok()
        || (host.starts_with('[') && name.parse::<std::net::Ipv6Addr>().is_ok());
    let labels: Vec<&str> = name.split('.').collect();
    let named = labels.len() > 1
        && labels.iter().all(|label| {
            !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && labels
            .last()
            .is_some_and(|last| last.len() > 1 && last.chars().all(|c| c.is_ascii_alphabetic()));
    let link = match (addressed, named) {
        (true, _) => format!("http://{word}"),
        (false, true) => format!("https://{word}"),
        (false, false) => return None,
    };
    url::Url::parse(&link).is_ok().then_some(link)
}

/// the help offering `command`, a command line with `link` in place of what was typed without its
/// scheme.
pub(super) fn link_meant(command: &str, link: &str) -> String {
    let scheme = link.split_once("://").map_or("https", |(scheme, _)| scheme);
    format!("did you mean `{command}`? a link starts with {scheme}://")
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
    let first = super::shell_word(&report.candidates[0].reference.to_string()).into_owned();
    let message = format!(
        "\"{words}\" is not a link or an id, and fetchloom will not guess which dataset you mean"
    );
    let help = format!(
        "run it again with one of these, such as `{}`",
        super::project::command(&format!("{command} {first}"))
    );
    let mut error = Error::new(Kind::ReferenceAmbiguous, message)
        .help(help)
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
    let table = parts(output, &report.candidates, width, LIST_LEAD);
    for shown in table.rows {
        if shown.title_below {
            output.item(&shown.reference);
            for under in [&shown.title, &shown.facts] {
                if !under.is_empty() {
                    output.item(&format!("  {under}"));
                }
            }
            continue;
        }
        // facts that do not fit beside the reference go on a line of their own under it
        if shown.below {
            output.item(&shown.reference);
            output.item(&format!("  {}", shown.facts));
            continue;
        }
        output.item(&shown.line(table.column));
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

/// whether `word`, one of the words to search for, holds a link's login or key, or a login with
/// its password before a host, as `ada:pw@data.example.org` does, which no search site may get.
/// an address alone, as an author's, holds no secret.
fn holds_secret(word: &str) -> bool {
    if matches!(crate::shown::mask_secrets(word), std::borrow::Cow::Owned(_)) {
        return true;
    }
    word.split_once('@')
        .is_some_and(|(login, _)| login.contains(':'))
        && crate::shown::mask_reference(word) != word
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
                "\"{}\" is a path on this machine, and fetchloom does not send paths to search sites",
                crate::shown::mask_query(path)
            ),
        )
        .help("search with words that describe the data, as in `fetchloom search iris`"));
    }
    if let Some(word) = words.split_whitespace().find(|word| holds_secret(word)) {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "\"{}\" holds a login or a key, and fetchloom sends no secret to search sites",
                crate::shown::mask_reference(word)
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
        match &warning.help {
            Some(help) => context
                .output
                .warn_with(Warning::SearchPartial, &warning.message, help),
            None => context
                .output
                .warn(Warning::SearchPartial, &warning.message),
        }
    }
    let mut candidates = result.candidates;
    for candidate in &mut candidates {
        candidate.time = expected(context, candidate);
    }
    if candidates.is_empty() {
        return Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("found no {} matching \"{words}\"", unmatched(&asked)),
        )
        .help("try other words, or paste a link or id such as zenodo:3608990")
        .with_result(serde_json::json!({"candidates": []})));
    }
    Ok(Report {
        candidates,
        left_out: result.left_out,
    })
}

/// about how long `candidate`'s listed size takes from the first of its hosts with a remembered
/// speed, as [`estimate::time`](crate::net::estimate::time) plans it; none without either.
fn expected(context: &Context, candidate: &Candidate) -> Option<crate::net::estimate::Time> {
    use crate::net::estimate;
    let size = candidate.size?;
    let hosts: Vec<&str> = candidate.reference.access().files.hosts().collect();
    let name = crate::source::paced_as(hosts.first()?);
    let speed = hosts
        .iter()
        .find_map(|host| context.speeds.speed(&crate::source::paced_as(host)))?;
    let host = estimate::Host {
        files: candidate.files,
        bytes: size,
        largest: candidate.largest.unwrap_or_default(),
        speed: Some(speed),
        spacing: crate::source::access::spacing(&name),
        most: crate::source::access::most_at_once(&name),
        ..estimate::Host::default()
    };
    estimate::time(&[host], super::fetch::limits(context))
}

/// one candidate for a script reading stdout: its reference first, then its title and, when
/// known, its size, separated by tabs so the first column stays the reference. a control in the
/// title reads as a space, and a bidi control as its escape, as the story writes one; the
/// reference, which comes from a catalog too, shows both as escapes.
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
    let mut reference = String::new();
    for c in candidate.reference.to_string().chars() {
        if c.is_control() || crate::name::is_bidi_control(c) {
            reference.extend(c.escape_default());
        } else {
            reference.push(c);
        }
    }
    let mut line = format!("{reference}\t{title}");
    if let Some(size) = candidate.size {
        line.push('\t');
        line.push_str(&units::size(size));
    }
    line
}

/// one candidate as a person reads it: what to type, its title and facts padded into columns, and
/// whether the facts, or the title with the facts, go on a line under the reference.
#[derive(Debug)]
struct Shown {
    reference: String,
    title: String,
    facts: String,
    below: bool,
    title_below: bool,
}

impl Shown {
    /// the row on one line: the reference padded to `pad` columns, then its title and facts.
    fn line(&self, pad: usize) -> String {
        [self.title.as_str(), self.facts.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .fold(format!("{:pad$}", self.reference), |row, part| {
                format!("{row}  {part}")
            })
            .trim_end()
            .to_owned()
    }
}

/// the fewest columns a title keeps before it gives way altogether, and the most it takes.
const TITLE_LEAST: usize = 12;
const TITLE_MOST: usize = 52;

/// the fewest columns a title cut short keeps on a row whose reference runs past its column, with
/// what ends it, before that row shows none.
const TITLE_SHORTEST: usize = 6;

/// the columns a title keeps before the references' column gives way to longer references.
const TITLE_ROOMY: usize = 20;

/// the rows of a list of matches and the column their titles start at, after the references.
struct Table {
    rows: Vec<Shown>,
    column: usize,
}

/// what a list of matches shows in its width: whether downloads and download times keep their
/// columns, how wide the references' column is, and how wide a title may be, none once titles go
/// under their references.
struct Plan {
    downloads: bool,
    time: bool,
    column: usize,
    most: Option<usize>,
}

/// each candidate's reference, title and facts in columns all rows share, in `width` after `lead`.
/// the title gives way last: downloads first, then download times, then the references' column
/// narrows, a longer reference running past it with a shorter title, and only then do titles go
/// under their references, a paper's byline before its title.
fn parts(output: &Output, candidates: &[Candidate], width: Option<usize>, lead: usize) -> Table {
    let references: Vec<String> = candidates
        .iter()
        .map(|candidate| candidate.reference.to_string())
        .collect();
    let told = cells(candidates, true);
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
    let plan = plan(output, (&references, &given), &told, width, lead);
    let column = plan.column;
    // what a row leaves after its reference and two spaces, with the last column spare
    let room = |reference: &str| {
        width.map(|width| {
            width.saturating_sub(lead + console::measure_text_width(reference).max(column) + 3)
        })
    };
    let more = console::measure_text_width(output.more());
    // once titles give way, papers keep theirs and leave out their author lines
    let facts = match plan.most {
        Some(_) => facts(&told, plan.downloads, plan.time),
        None => facts(&cells(candidates, false), plan.downloads, plan.time),
    };
    let (titles, title_width) = match plan.most {
        Some(most) => fitted_titles(output, (&references, &given), column, (most, width)),
        None => (vec![String::new(); given.len()], 0),
    };
    let rows = references
        .into_iter()
        .zip(titles)
        .zip(facts)
        .zip(&given)
        .map(|(((reference, title), facts), given)| {
            let facts = facts.trim_end().to_owned();
            let under = width.map_or(TITLE_MOST, |width| width.saturating_sub(lead + 1 + more));
            if plan.most.is_none() && !given.is_empty() {
                let beside = room(&reference);
                return paper_row(output, (reference, given), facts, beside, under);
            }
            // a reference so far past the column that none of its title fits beside it, as a
            // paper's can be, keeps its title under it
            if title.is_empty() && !given.is_empty() {
                return paper_row(output, (reference, given), facts, None, under);
            }
            // a reference past the column takes its room from its own title, so the facts line up
            let past = console::measure_text_width(&reference).saturating_sub(column);
            let title = if facts.is_empty() {
                title
            } else {
                pad(&title, title_width.saturating_sub(past), false)
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
                title_below: false,
            }
        })
        .collect();
    Table { rows, column }
}

/// what a list of the matches whose `references` and `given` titles these are shows in `width`
/// after `lead`, as [`parts`] gives way: the first of downloads and download times to go that
/// leaves titles [`TITLE_LEAST`] beside the widest reference, else the references' column that
/// shows the most of the titles, else titles under their references.
fn plan(
    output: &Output,
    (references, given): (&[String], &[String]),
    told: &[[String; 4]],
    width: Option<usize>,
    lead: usize,
) -> Plan {
    let any_title = given.iter().any(|title| !title.is_empty());
    // a reference whose row has no title lines up its facts past the titles' column instead
    let titled: Vec<&str> = references
        .iter()
        .zip(given)
        .filter(|(_, title)| !title.is_empty() || !any_title)
        .map(|(reference, _)| reference.as_str())
        .collect();
    let full = widest(titled.iter().copied()).min(crate::output::REFERENCE_COLUMN);
    let more = console::measure_text_width(output.more());
    // what titles and the references' column share: the line, less the lead, the gaps, the facts
    // and the last column
    let spare = |width: usize, facts: usize| {
        let gap = if facts > 0 { 2 } else { 0 };
        width.saturating_sub(lead + 3 + facts + gap + more)
    };
    for (downloads, time) in [(true, true), (false, true), (false, false)] {
        let facts_width = widest(facts(told, downloads, time).iter().map(String::as_str));
        let shown = |most| Plan {
            downloads,
            time,
            column: full,
            most,
        };
        match width {
            None => return shown(Some(TITLE_MOST)),
            Some(width) if !any_title => {
                if width.saturating_sub(lead + full + 3) >= facts_width {
                    return shown(Some(TITLE_MOST));
                }
            }
            Some(width) => {
                let spare = spare(width, facts_width);
                if spare >= full + TITLE_LEAST {
                    return shown(Some((spare - full).min(TITLE_MOST)));
                }
            }
        }
    }
    let narrowest = Plan {
        downloads: false,
        time: false,
        column: full,
        most: None,
    };
    let Some(width) = width.filter(|_| any_title) else {
        return narrowest;
    };
    let spare = spare(
        width,
        widest(facts(told, false, false).iter().map(String::as_str)),
    );
    // the widest column that leaves the titles a good part of the line, else the narrowest that
    // leaves them their least: a longer reference runs past it, so the shorter ones line up
    let columns: Vec<usize> = titled
        .iter()
        .map(|reference| console::measure_text_width(reference))
        .filter(|column| *column <= full)
        .collect();
    let roomy = columns
        .iter()
        .copied()
        .filter(|column| spare >= column + TITLE_ROOMY)
        .max();
    let least = columns
        .iter()
        .copied()
        .min()
        .filter(|column| spare >= column + TITLE_LEAST);
    match roomy.or(least) {
        Some(column) => Plan {
            downloads: false,
            time: false,
            column,
            most: Some((spare - column).min(TITLE_MOST)),
        },
        None => narrowest,
    }
}

/// each row's title in a list whose references' column is `column` wide and whose titles are at
/// most `most`, with the widest title: on a terminal `width` wide, a row whose reference runs past
/// the column cuts its title by as much, so its facts still line up, and shows none where too
/// little is left.
fn fitted_titles(
    output: &Output,
    (references, given): (&[String], &[String]),
    column: usize,
    (most, width): (usize, Option<usize>),
) -> (Vec<String>, usize) {
    let more = console::measure_text_width(output.more());
    let past = |reference: &str| console::measure_text_width(reference).saturating_sub(column);
    let lined: Vec<String> = references
        .iter()
        .zip(given)
        .map(|(reference, title)| {
            if title.is_empty() || past(reference) > 0 {
                String::new()
            } else {
                output.clip(title, most)
            }
        })
        .collect();
    let title_width = widest(lined.iter().map(String::as_str));
    let titles = references
        .iter()
        .zip(given)
        .zip(lined)
        .map(|((reference, title), lined)| {
            let past = past(reference);
            if past == 0 {
                return lined;
            }
            if width.is_none() {
                return output.clip(title, most);
            }
            let room = title_width.saturating_sub(past);
            if title.is_empty() || room < TITLE_SHORTEST {
                String::new()
            } else if console::measure_text_width(title) <= room {
                title.clone()
            } else {
                output.clip(title, room - more)
            }
        })
        .collect();
    (titles, title_width)
}

/// each row's size, download time, downloads and note, with a paper's author line before its
/// note where `bylines` keeps it.
fn cells(candidates: &[Candidate], bylines: bool) -> Vec<[String; 4]> {
    candidates
        .iter()
        .map(|candidate| {
            let note = if bylines {
                candidate.told()
            } else {
                candidate.note.clone()
            };
            [
                candidate.size.map(units::size).unwrap_or_default(),
                candidate
                    .time
                    .map(crate::net::estimate::Time::words)
                    .unwrap_or_default(),
                candidate
                    .downloads
                    .map(worded_downloads)
                    .unwrap_or_default(),
                note.unwrap_or_default(),
            ]
        })
        .collect()
}

/// a row whose title has no room in the titles' column, as a paper's with its long reference: its
/// title in the `beside` columns by the reference, its note under it when both do not fit, else
/// the title on a line below, cut to `under` columns.
fn paper_row(
    output: &Output,
    (reference, title): (String, &str),
    facts: String,
    beside: Option<usize>,
    under: usize,
) -> Shown {
    let more = console::measure_text_width(output.more());
    if let Some(room) = beside.filter(|room| *room >= TITLE_LEAST + more) {
        let title = output.clip(title, (room - more).min(TITLE_MOST));
        let noted = if facts.is_empty() {
            0
        } else {
            2 + console::measure_text_width(&facts)
        };
        return Shown {
            below: console::measure_text_width(&title) + noted > room,
            reference,
            title,
            facts,
            title_below: false,
        };
    }
    // the title leaves room for the facts after it, so a picker's line under the reference holds
    // both
    let noted = if facts.is_empty() {
        0
    } else {
        2 + console::measure_text_width(&facts)
    };
    Shown {
        title: output.clip(title, under.saturating_sub(noted).max(TITLE_LEAST)),
        reference,
        facts,
        below: false,
        title_below: true,
    }
}

/// each row's facts in their columns: size, download time where `time` keeps it, then downloads
/// where `downloads` keeps them, then the access note, numbers to the right.
fn facts(cells: &[[String; 4]], downloads: bool, time: bool) -> Vec<String> {
    let widths: Vec<usize> = (0..4)
        .map(|column| widest(cells.iter().map(|row| row[column].as_str())))
        .collect();
    cells
        .iter()
        .map(|row| {
            row.iter()
                .zip(&widths)
                .enumerate()
                .filter(|(column, (_, width))| {
                    **width > 0 && (downloads || *column != 2) && (time || *column != 1)
                })
                .map(|(column, (cell, width))| pad(cell, *width, column == 0 || column == 2))
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
    let table = parts(output, candidates, width, lead);
    table
        .rows
        .iter()
        .map(|shown| shown.line(table.column))
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
                    byline: None,
                    note: None,
                    time: None,
                    files: None,
                    largest: None,
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
            byline: None,
            note: None,
            time: None,
            files: None,
            largest: None,
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
        // nor can a catalog's id, which goes first and unclipped
        let turned = Candidate {
            reference: Reference::parse("doi:10.5061/x\u{202e}vsc")
                .unwrap()
                .reference,
            title: "Iris".to_owned(),
            ..reversed
        };
        assert_eq!(line(&turned), "doi:10.5061/x\\u{202e}vsc\tIris\t3.6 KiB");
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
                byline: None,
                note: None,
                time: None,
                files: None,
                largest: None,
            },
            Candidate {
                reference: Reference::parse("zenodo:1").unwrap().reference,
                title: "Iris".to_owned(),
                source: "zenodo",
                size: None,
                downloads: None,
                byline: None,
                note: Some("needs access".to_owned()),
                time: None,
                files: None,
                largest: None,
            },
            Candidate {
                reference: Reference::parse("hf:datasets/a/b").unwrap().reference,
                title: "a/b".to_owned(),
                source: "hugging face",
                size: None,
                downloads: Some(950),
                byline: None,
                note: None,
                time: None,
                files: None,
                largest: None,
            },
            Candidate {
                reference: Reference::parse("hf:datasets/c/d").unwrap().reference,
                title: "c/d".to_owned(),
                source: "hugging face",
                size: None,
                downloads: Some(1),
                byline: None,
                note: None,
                time: None,
                files: None,
                largest: None,
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

    /// the download time stands right after the size, blank without grounds. narrower, downloads
    /// give way, then titles shorten, while every size and time stays.
    #[test]
    fn a_download_time_follows_the_size_and_titles_give_way_first() {
        use crate::net::estimate::{Bound, Time};
        let output = quiet_output();
        let candidate = |reference: &str, title: &str, size, time| Candidate {
            reference: Reference::parse(reference).unwrap().reference,
            title: title.to_owned(),
            source: "zenodo",
            size: Some(size),
            downloads: Some(12_000),
            byline: None,
            note: None,
            time,
            files: None,
            largest: None,
        };
        let long = "a title long enough that it gives way on a terminal eighty columns wide";
        let about = Time {
            bound: Bound::About,
            seconds: 180,
        };
        let candidates = vec![
            candidate("zenodo:1", long, 3 << 30, Some(about)),
            candidate("zenodo:22", "Iris", 3687, None),
        ];
        let lead = picker_lead(candidates.len());
        let wide = rows(&output, &candidates, None, lead);
        assert!(
            wide[0].ends_with("3.0 GiB  about 3 min  12k downloads"),
            "{wide:?}"
        );
        let blank = format!("3.6 KiB{}12k downloads", " ".repeat(15));
        assert!(wide[1].ends_with(&blank), "{wide:?}");
        for width in [80, 60] {
            let narrow = rows(&output, &candidates, Some(width), lead);
            for row in &narrow {
                assert!(
                    console::measure_text_width(row) + lead <= width,
                    "{width}: {row}"
                );
            }
            assert!(!narrow[0].contains(long), "{width}: {narrow:?}");
            assert!(
                narrow[0].contains("3.0 GiB  about 3 min"),
                "{width}: {narrow:?}"
            );
            assert!(narrow[1].contains("3.6 KiB"), "{width}: {narrow:?}");
            assert_eq!(narrow[0].contains("downloads"), width == 80, "{narrow:?}");
        }
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

    /// the matches `add iris` listed: each reference, title, size and the minutes its download
    /// takes, where this machine knows them.
    const IRIS: [(&str, &str, Option<u64>, Option<u64>); 12] = [
        ("kaggle:uciml/iris", "Iris Species", Some(3687), None),
        (
            "hf:datasets/scikit-learn/iris",
            "scikit-learn/iris",
            Some(18_534),
            Some(1),
        ),
        (
            "hf:datasets/Hebrish/Irish-English-Sentences",
            "Hebrish/Irish-English-Sentences",
            Some(76 << 20),
            Some(2),
        ),
        (
            "zenodo:23233774",
            "IRIS: ICESat-2 River Surface Slope, a global river slope dataset",
            Some(1_181_116_006),
            Some(15),
        ),
        (
            "geo:GSE318345",
            "Early eye organoids demonstrate cadherin and integrin",
            None,
            None,
        ),
        (
            "openneuro:ds001037",
            "The brain of Chris",
            Some(8_600_815),
            None,
        ),
        (
            "kaggle:himanshunakrani/iris-dataset",
            "Iris dataset",
            Some(1006),
            None,
        ),
        (
            "zenodo:1319069",
            "Iris flower data set",
            Some(3687),
            Some(1),
        ),
        ("geo:GSE311178", "Single cell atlas of the iris", None, None),
        (
            "openneuro:ds007046",
            "Vision for action",
            Some(71_568_032_567),
            Some(600),
        ),
        (
            "hf:datasets/fatdove/Iris_Database",
            "fatdove/Iris_Database",
            Some(87_727_000_000),
            Some(700),
        ),
        (
            "kaggle:arshid/iris-flower-dataset",
            "Iris Flower Dataset",
            Some(1010),
            None,
        ),
    ];

    /// `add iris` at 80 columns once listed `zenodo:23233774` and `geo:GSE318345` with no title,
    /// since one long hugging face id set the column, and the titles came and went with known
    /// download times: at 80 and 100 columns every match a title names keeps one, the column
    /// narrowing and the times giving way first, and every line fits.
    #[test]
    fn titles_stay_at_eighty_and_a_hundred_columns() {
        use crate::net::estimate::{Bound, Time};
        let candidate =
            |reference: &str, title: &str, size: Option<u64>, minutes: Option<u64>| Candidate {
                reference: Reference::parse(reference).unwrap().reference,
                title: title.to_owned(),
                source: "test",
                size,
                downloads: size.map(|_| 2400),
                byline: None,
                note: reference
                    .contains("Hebrish")
                    .then(|| "needs access".to_owned()),
                time: minutes.map(|minutes| Time {
                    bound: Bound::About,
                    seconds: minutes * 60,
                }),
                files: None,
                largest: None,
            };
        let listed = |timed: bool| -> Vec<Candidate> {
            IRIS.iter()
                .map(|(reference, title, size, minutes)| {
                    candidate(reference, title, *size, minutes.filter(|_| timed))
                })
                .collect()
        };
        let (iris, untimed) = (listed(true), listed(false));
        let output = quiet_output();
        for (width, lead) in [
            (80, picker_lead(iris.len())),
            (80, LIST_LEAD),
            (100, LIST_LEAD),
        ] {
            let table = parts(&output, &iris, Some(width), lead);
            for (shown, candidate) in table.rows.iter().zip(&iris) {
                let named = ["zenodo:", "geo:", "openneuro:"]
                    .iter()
                    .any(|site| shown.reference.starts_with(site));
                assert!(
                    !named || shown.title.chars().count() > 6,
                    "{width} {lead}: {} lost its title: {shown:?}",
                    candidate.reference
                );
                let line = if shown.below || shown.title_below {
                    shown.reference.clone()
                } else {
                    shown.line(table.column)
                };
                assert!(
                    console::measure_text_width(&line) < width - lead,
                    "{width} {lead}: {line:?}"
                );
            }
            // a time this machine learned after a first download once cost every title
            let titles = |table: &Table| -> Vec<String> {
                table.rows.iter().map(|shown| shown.title.clone()).collect()
            };
            assert_eq!(
                titles(&table),
                titles(&parts(&output, &untimed, Some(width), lead)),
                "{width} {lead}"
            );
        }
    }

    /// at 60, 80 and 100 columns every row keeps its reference, access note and size whole,
    /// dropping downloads, then download times, and moves note and size under it when they do not
    /// fit; a title with no room beside its reference goes under it.
    #[test]
    fn narrow_rows_keep_the_access_note_and_the_size_whole() {
        let candidate = |reference: &str, title: &str, size, downloads, gated: bool| Candidate {
            reference: Reference::parse(reference).unwrap().reference,
            title: title.to_owned(),
            source: "kaggle",
            size: Some(size),
            downloads: Some(downloads),
            byline: None,
            note: gated.then(|| "needs access".to_owned()),
            time: None,
            files: None,
            largest: None,
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
                let table = parts(&output, &candidates, Some(width), lead);
                let column = table.column;
                for (shown, candidate) in table.rows.iter().zip(&candidates) {
                    assert_eq!(shown.reference, candidate.reference.to_string());
                    let lines = if shown.title_below {
                        vec![
                            shown.reference.clone(),
                            shown.title.clone(),
                            shown.facts.clone(),
                        ]
                    } else if shown.below {
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
        let wide = parts(&output, &candidates, Some(200), LIST_LEAD).rows;
        assert!(wide.iter().all(|shown| shown.facts.contains("downloads")));
        assert!(
            wide[1].title.starts_with("Iris dataset"),
            "{}",
            wide[1].title
        );
    }

    /// at 80 columns a zenodo match beside a paper with a 40-hex reference once narrowed the column
    /// to the short reference and left the paper none of its title: a title with no room beside its
    /// reference goes under it.
    #[test]
    fn a_paper_past_a_narrowed_column_keeps_its_title_under_it() {
        let candidate = |reference: &str, title: &str, byline: Option<&str>| Candidate {
            reference: Reference::parse(reference).unwrap().reference,
            title: title.to_owned(),
            source: "test",
            size: Some(3687),
            downloads: None,
            byline: byline.map(str::to_owned),
            note: None,
            time: None,
            files: None,
            largest: None,
        };
        let candidates = [
            candidate("zenodo:1", "Iris", None),
            candidate(
                "s2:649def34f8be52c8b66281af98ae884c09aef38b",
                "Attention Is All You Need",
                Some("Vaswani 2017"),
            ),
        ];
        let output = quiet_output();
        for lead in [LIST_LEAD, picker_lead(candidates.len())] {
            let table = parts(&output, &candidates, Some(80), lead);
            for (shown, candidate) in table.rows.iter().zip(&candidates) {
                let kept = shown.title.trim_end_matches(output.more());
                assert!(
                    kept.len() > 3 && candidate.title.starts_with(kept),
                    "{lead}: {shown:?}"
                );
            }
        }
    }

    /// a narrow terminal drops a paper's author line before its title, which stays beside the
    /// reference or goes under it with a note add needs, as `no open-access pdf`; wide shows both.
    #[test]
    fn a_papers_author_line_gives_way_before_its_title() {
        let paper = |reference: &str, title: &str, byline: &str, closed: bool| Candidate {
            reference: Reference::parse(reference).unwrap().reference,
            title: title.to_owned(),
            source: "arXiv",
            size: None,
            downloads: None,
            byline: Some(byline.to_owned()),
            note: closed.then(|| "no open-access pdf".to_owned()),
            time: None,
            files: None,
            largest: None,
        };
        let candidates = vec![
            paper(
                "arxiv:1706.03762",
                "Attention Is All You Need",
                "Vaswani et al. 2017",
                false,
            ),
            paper(
                "arxiv:2104.04692",
                "Not All Attention Is All You Need",
                "Wu et al. 2021",
                false,
            ),
            paper(
                "s2:CorpusId:229349233",
                "Channel Attention Is All You Need for Video Frame Interpolation",
                "Choi et al. 2020",
                true,
            ),
        ];
        let output = quiet_output();
        for width in [40, 50, 60] {
            let table = parts(&output, &candidates, Some(width), LIST_LEAD);
            let column = table.column;
            for (shown, candidate) in table.rows.iter().zip(&candidates) {
                let kept = shown.title.trim_end_matches(output.more());
                assert!(
                    kept.len() > 3 && candidate.title.starts_with(kept),
                    "{width}: {shown:?}"
                );
                let byline = candidate.byline.as_deref().unwrap_or_default();
                assert!(!shown.facts.contains(byline), "{width}: {shown:?}");
                assert_eq!(
                    shown.facts.contains("no open-access pdf"),
                    candidate.note.is_some(),
                    "{width}: {shown:?}"
                );
                let lines = if shown.title_below {
                    vec![
                        shown.reference.clone(),
                        shown.title.clone(),
                        shown.facts.clone(),
                    ]
                } else if shown.below {
                    let beside = format!("{:column$}  {}", shown.reference, shown.title);
                    vec![beside, shown.facts.clone()]
                } else {
                    let beside = format!("{:column$}  {}", shown.reference, shown.title);
                    vec![format!("{beside}  {}", shown.facts)]
                };
                for line in &lines {
                    assert!(
                        LIST_LEAD + console::measure_text_width(line.trim_end()) < width,
                        "{width}: {line:?}"
                    );
                }
            }
        }
        let wide = parts(&output, &candidates, Some(200), LIST_LEAD).rows;
        for (shown, candidate) in wide.iter().zip(&candidates) {
            assert!(!shown.below && !shown.title_below, "{shown:?}");
            assert!(
                shown
                    .facts
                    .contains(candidate.byline.as_deref().unwrap_or_default())
            );
        }
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
            byline: None,
            note: None,
            time: None,
            files: None,
            largest: None,
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
            let parts = parts(&output, &candidates, Some(width), LIST_LEAD).rows;
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
            byline: None,
            note: None,
            time: None,
            files: None,
            largest: None,
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
                let table = parts(&output, &candidates, Some(width), LIST_LEAD);
                let (column, parts) = (table.column, table.rows);
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
                byline: None,
                note: None,
                time: None,
                files: None,
                largest: None,
            },
            Candidate {
                reference: Reference::parse(&long).unwrap().reference,
                title: "Long".to_owned(),
                source: "hugging face",
                size: Some(3687),
                downloads: None,
                byline: None,
                note: None,
                time: None,
                files: None,
                largest: None,
            },
        ];
        let rows = rows(&output, &candidates, None, picker_lead(candidates.len()));
        assert_eq!(rows[0].find("Iris Species"), Some(50), "{}", rows[0]);
        assert!(rows[1].starts_with(&format!("{long}  ")), "{}", rows[1]);
    }
}
