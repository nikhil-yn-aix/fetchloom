//! `fetchloom export`: the SHA-256 of each file a project places, or the link and SHA-256 of each
//! file it downloads, in forms other tools read: `sha256sum -c`, a list of links, or an aria2c
//! input file. it reads data.toml, data.lock and the cache's records of what each archive
//! unpacked to, and asks no server anything. a text form is complete or not written at all, but
//! for the datasets it has nothing to write for, which it names; `--json` lists everything it
//! knows and names what it does not.

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;
use url::Url;

use super::Context;
use super::args::{self, Format};
use crate::cache::Cache;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::name;
use crate::place::Placer;
use crate::plan;
use crate::project::lock::{self, Lock};
use crate::project::manifest::{Entry, Manifest};

#[derive(Serialize)]
pub(super) struct Report {
    /// the file `-o` wrote, or none when the answer went to stdout or was not written.
    file: Option<PathBuf>,
    placed: Vec<Placed>,
    sources: Vec<Source>,
    /// datasets data.lock does not pin as data.toml asks.
    unlocked: Vec<String>,
    /// archives whose files this machine does not know, by where they would be in the project.
    not_unpacked: Vec<String>,
    /// downloaded files, as `<dataset>/<path>`, whose link is not written.
    no_link: Vec<String>,
    /// the datasets the chosen form never lists, by their kind, with why.
    left_out: Vec<LeftOut>,
    /// why each fetched dataset in `unlocked` is there.
    #[serde(skip)]
    stale: Vec<String>,
    /// the recipes in `unlocked`: those data.lock pins no result of yet.
    #[serde(skip)]
    unbuilt: Vec<String>,
    /// why each link of `no_link` is not written, but for imported files, which have none.
    #[serde(skip)]
    refused: Vec<String>,
    /// the datasets imported from a folder, which no link holds, each with its number of files.
    #[serde(skip)]
    imported: Vec<(String, u64)>,
}

/// a file in the data folder, as data.lock and the cache pin it.
#[derive(Serialize)]
struct Placed {
    dataset: String,
    /// below the project, with `/` between folders.
    path: String,
    size: u64,
    sha256: Sha256,
    /// the archive it unpacks from, by its path in the dataset, or none for a downloaded file.
    unpacked_from: Option<String>,
}

/// a dataset a form never lists, and why.
#[derive(Serialize)]
struct LeftOut {
    dataset: String,
    reason: Reason,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Reason {
    /// a recipe makes its files, so no link holds them.
    Recipe,
    /// its files were imported from a folder on this machine, so no link holds them.
    Imported,
    /// `place = false` keeps its files out of the data folder.
    NotPlaced,
}

/// a file downloaded for a dataset, as data.lock pins it.
#[derive(Serialize)]
struct Source {
    dataset: String,
    /// inside the dataset, as data.lock lists it.
    path: String,
    size: u64,
    sha256: Sha256,
    /// none when the link is not written.
    url: Option<String>,
    /// below the data folder, in the folder `path` gives its dataset, where aria2c puts it.
    #[serde(skip)]
    out: String,
}

pub(super) fn run(context: &Context, args: &args::Export) -> Result<Report, Error> {
    if args.output.as_deref() == Some(Path::new("-")) {
        return Err(Error::new(Kind::Usage, "-o names a file, not - for stdout")
            .help("leave -o out to print on stdout"));
    }
    let project = super::project::find(&context.output)?;
    let (_, manifest) = project.manifest()?;
    for name in &args.names {
        super::project::known(&manifest, name, "export")?;
    }
    let lock = if args.names.is_empty() {
        project.lock()?
    } else {
        project
            .lock_only(&|name| args.names.iter().any(|named| named == name))?
            .map(|(lock, _)| lock)
    }
    .unwrap_or_default();
    let mut report = inventory(&manifest, lock, &super::peek_cache()?, &args.names)?;
    report.left_out = left_out(&manifest, &args.names, args.format);
    let output = &context.output;
    if manifest.datasets.is_empty() {
        output.done("Ok", "data.toml has no datasets yet");
        return Ok(report);
    }
    if output.json() && args.output.is_none() {
        return Ok(report);
    }
    if let Err(error) = complete(&report, args.format) {
        return Err(with_report(error, &report));
    }
    let (text, count) = render(&report, args.format);
    let Some(path) = &args.output else {
        if !text.is_empty() {
            output.answer(text.trim_end_matches('\n'));
        }
        tell_imported(output, &report, args.format);
        tell_left_out(output, &report);
        return Ok(report);
    };
    // the last look for a stop: once the file is being written, the export ends as it would
    context.stop.check()?;
    context.stop.finish();
    match publish(path, &text, args.force) {
        Ok(file) => report.file = Some(file),
        Err(error) => return Err(with_report(error, &report)),
    }
    let what = match args.format {
        Format::Sha256 => crate::units::count(count, "file", "files"),
        Format::Links | Format::Aria2 => crate::units::count(count, "link", "links"),
    };
    output.done("Exported", &format!("{what} to {}", path.display()));
    tell_imported(output, &report, args.format);
    tell_left_out(output, &report);
    Ok(report)
}

/// says once how many files a list of links leaves out because they were imported from a
/// folder, which no link holds.
fn tell_imported(output: &crate::output::Output, report: &Report, format: Format) {
    if format == Format::Sha256 || report.imported.is_empty() {
        return;
    }
    let files: u64 = report.imported.iter().map(|(_, files)| files).sum();
    let names: Vec<&str> = report
        .imported
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let Some(datasets) = joined(&names) else {
        return;
    };
    let (they_were, them) = if files == 1 {
        ("it was", "it")
    } else {
        ("they were", "them")
    };
    output.warn_with(
        &format!(
            "the list leaves out {} of {datasets}: {they_were} imported from a folder on this machine, so no link holds {them}",
            crate::units::count(files, "file", "files")
        ),
        &format!("share {them} through a folder mirror, which `fetchloom push --to <folder>` fills"),
    );
}

/// the datasets `names`, or every dataset when none is named, that `format` never lists: a
/// recipe's result or an import has no link, and a dataset with `place = false` no file in the
/// data folder.
fn left_out(manifest: &Manifest, names: &[String], format: Format) -> Vec<LeftOut> {
    manifest
        .datasets
        .iter()
        .filter(|(name, _)| names.is_empty() || names.contains(name))
        .filter_map(|(name, entry)| {
            let reason = match format {
                Format::Sha256 if !entry.place => Reason::NotPlaced,
                Format::Links | Format::Aria2 if entry.recipe().is_some() => Reason::Recipe,
                Format::Links | Format::Aria2 if entry.is_import() => Reason::Imported,
                _ => return None,
            };
            Some(LeftOut {
                dataset: name.clone(),
                reason,
            })
        })
        .collect()
}

/// says on one line which recipes' results a list of links leaves out, or which datasets with
/// `place = false` a list of checksums does. imports have their own warning, with a count.
fn tell_left_out(output: &crate::output::Output, report: &Report) {
    let named = |reason: Reason| -> Vec<&str> {
        report
            .left_out
            .iter()
            .filter(|left| left.reason == reason)
            .map(|left| left.dataset.as_str())
            .collect()
    };
    let recipes = named(Reason::Recipe);
    if let Some(datasets) = joined(&recipes) {
        let makes = if recipes.len() == 1 {
            "a recipe makes its"
        } else {
            "recipes make their"
        };
        output.warn(&format!(
            "the list leaves out {datasets}: {makes} files, so no link holds them"
        ));
    }
    let unplaced = named(Reason::NotPlaced);
    if let Some(datasets) = joined(&unplaced) {
        let its = if unplaced.len() == 1 { "its" } else { "their" };
        output.warn(&format!(
            "the list leaves out {datasets}: place = false keeps {its} files out of the data folder"
        ));
    }
}

/// `names` as a sentence lists them, as `a, b and c`, or none when there are none.
fn joined(names: &[&str]) -> Option<String> {
    let (last, rest) = names.split_last()?;
    Some(if rest.is_empty() {
        (*last).to_owned()
    } else {
        format!("{} and {last}", rest.join(", "))
    })
}

/// `error` with what export found, so `--json` still lists it.
fn with_report(error: Error, report: &Report) -> Error {
    match serde_json::to_value(report) {
        Ok(value) => error.with_result(value),
        Err(_) => error,
    }
}

/// what the datasets `names`, or every dataset when none is named, pin: the files placed from
/// `lock`, archive members as the cache recorded them, and the files downloaded, with what is not
/// known here.
fn inventory(
    manifest: &Manifest,
    mut lock: Lock,
    cache: &Cache,
    names: &[String],
) -> Result<Report, Error> {
    let wanted = |name: &str| names.is_empty() || names.iter().any(|named| named == name);
    let mut report = Report {
        file: None,
        placed: Vec::new(),
        sources: Vec::new(),
        unlocked: Vec::new(),
        not_unpacked: Vec::new(),
        no_link: Vec::new(),
        left_out: Vec::new(),
        stale: Vec::new(),
        unbuilt: Vec::new(),
        refused: Vec::new(),
        imported: Vec::new(),
    };
    for (name, entry) in &manifest.datasets {
        if !wanted(name) {
            continue;
        }
        // sync places a recipe's result as data.lock pins it, however its recipe changed since,
        // so only one never built is missing
        if entry.recipe().is_some() {
            if lock.result(name).is_none() {
                report.unlocked.push(name.clone());
                report.unbuilt.push(name.clone());
            }
            continue;
        }
        let stale = match lock.get(name).map(|locked| locked.drift(entry)) {
            None => format!("{name} is not in data.lock yet"),
            Some(Some(lock::Drift::Spec)) => format!("{name} changed in data.toml"),
            Some(Some(lock::Drift::Extract)) => format!("{name} changed extract in data.toml"),
            Some(None) => continue,
        };
        report.unlocked.push(name.clone());
        report.stale.push(stale);
    }
    lock.datasets
        .retain(|locked| wanted(&locked.name) && !report.unlocked.contains(&locked.name));
    lock.derived.retain(|result| wanted(&result.name));
    let planned = plan::from_cache(manifest, &lock, cache)?;
    for target in &planned.targets {
        if let Some(reason) = name::escapes(&target.path) {
            return Err(unwritable(&target.path, reason));
        }
        report.placed.push(Placed {
            dataset: target.dataset.to_owned(),
            path: target.path.clone(),
            size: target.size,
            sha256: target.sha256,
            unpacked_from: target
                .file()
                .filter(|file| file.unpacks_to.is_some())
                .map(|file| file.path.clone()),
        });
    }
    report.placed.sort_by(|a, b| a.path.cmp(&b.path));
    report.not_unpacked = planned
        .unknown
        .iter()
        .map(|(dataset, file)| format!("{}/{}", plan::folder(manifest, dataset), file.path))
        .collect();
    sources(manifest, &lock, &mut report)?;
    Ok(report)
}

/// adds to `report` each file `lock` pins for a dataset, with its link, or why it has none.
fn sources(manifest: &Manifest, lock: &Lock, report: &mut Report) -> Result<(), Error> {
    for dataset in &lock.datasets {
        let folder = plan::subfolder(manifest, &dataset.name);
        let imported = manifest
            .datasets
            .get(&dataset.name)
            .is_some_and(Entry::is_import);
        if imported {
            report
                .imported
                .push((dataset.name.clone(), dataset.files.len() as u64));
        }
        for file in &dataset.files {
            let shown = format!("{}/{}", dataset.name, file.path);
            let out = folder.map_or_else(
                || file.path.clone(),
                |folder| format!("{folder}/{}", file.path),
            );
            if let Some(reason) = name::escapes(&out) {
                return Err(unwritable(&out, reason));
            }
            let url = match link(&file.url) {
                Ok(url) => Some(url),
                Err(reason) => {
                    // an import has no link by design, so its files are no fault of data.lock
                    if !imported {
                        report.refused.push(format!("{shown}: {reason}"));
                    }
                    report.no_link.push(shown);
                    None
                }
            };
            report.sources.push(Source {
                dataset: dataset.name.clone(),
                path: file.path.clone(),
                size: file.size,
                sha256: file.sha256,
                url,
                out,
            });
        }
    }
    Ok(())
}

/// `text`, a link data.lock gives a file, as export and list write it, or why it is not written:
/// fetchloom never pins a link with a login or a secret, nor one a line cannot hold, so such a
/// link was written by someone else.
pub(super) fn link(text: &str) -> Result<String, &'static str> {
    if text.contains(|character: char| character.is_whitespace() || name::is_control(character)) {
        return Err("holds a space or a control character");
    }
    let url = Url::parse(text).map_err(|_| "is not a link")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("is not an http or https link");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("holds a login");
    }
    if crate::net::secret(&url).is_some() {
        return Err("holds a secret");
    }
    Ok(url.into())
}

/// refuses a text answer that would leave out what `format` needs: a dataset not pinned, files
/// inside an archive this machine never unpacked, or a link that is not written.
fn complete(report: &Report, format: Format) -> Result<(), Error> {
    if !report.stale.is_empty() {
        let error = Error::new(Kind::Outdated, "data.lock is out of date with data.toml");
        return Err(report
            .stale
            .iter()
            .fold(error, |error, line| error.detail(line.clone()))
            .help("run `fetchloom lock` and commit data.lock, then export again"));
    }
    if !report.unbuilt.is_empty() {
        return Err(Error::new(
            Kind::BuildNeeded,
            format!(
                "data.lock pins no result of {} yet",
                report.unbuilt.join(", ")
            ),
        )
        .help(format!(
            "run `fetchloom build {}`, then export again",
            report.unbuilt.join(" ")
        )));
    }
    match format {
        Format::Sha256 if !report.not_unpacked.is_empty() => {
            let error = match report.not_unpacked.as_slice() {
                [one] => Error::new(
                    Kind::OfflineMissing,
                    format!("the files inside {one} are not known on this machine"),
                ),
                many => Error::new(
                    Kind::OfflineMissing,
                    format!(
                        "the files inside {} archives are not known on this machine",
                        crate::units::grouped(many.len() as u64)
                    ),
                )
                .detail(many.join(", ")),
            };
            Err(error.help("run `fetchloom sync` to unpack them here, then export again"))
        }
        Format::Links | Format::Aria2 if !report.refused.is_empty() => {
            let error = match report.refused.as_slice() {
                [one] => {
                    let (shown, reason) = one.split_once(": ").unwrap_or((one, ""));
                    Error::new(
                        Kind::ProjectInvalid,
                        format!(
                            "the link data.lock gives {shown} {reason}, so export will not write it"
                        ),
                    )
                }
                many => many.iter().fold(
                    Error::new(
                        Kind::ProjectInvalid,
                        format!(
                            "{} links in data.lock cannot be written",
                            crate::units::grouped(many.len() as u64)
                        ),
                    ),
                    |error, line| error.detail(line.clone()),
                ),
            };
            Err(error.help(lock::BROKEN))
        }
        _ => Ok(()),
    }
}

/// the answer in `format`, and how many files or links it names.
fn render(report: &Report, format: Format) -> (String, u64) {
    let mut text = String::new();
    let mut count = 0;
    match format {
        Format::Sha256 => {
            for file in &report.placed {
                let _ = writeln!(text, "{}  {}", file.sha256, file.path);
                count += 1;
            }
        }
        Format::Links | Format::Aria2 => {
            for source in &report.sources {
                let Some(url) = &source.url else { continue };
                if format == Format::Links {
                    let _ = writeln!(text, "{url} {}", source.sha256);
                } else {
                    let _ = writeln!(
                        text,
                        "{url}\n  out={}\n  checksum=sha-256={}",
                        source.out, source.sha256
                    );
                }
                count += 1;
            }
        }
    }
    (text, count)
}

/// writes `text` as the file at `path`: in full under a hidden name beside it, flushed, then
/// given the name without replacing anything there. with `force`, a file already there is moved
/// aside until the new one has its name. returns the file's absolute path.
fn publish(path: &Path, text: &str, force: bool) -> Result<PathBuf, Error> {
    let writing = || format!("could not write {}", path.display());
    let target = std::path::absolute(path).map_err(|err| Error::io(writing(), &err))?;
    let (Some(folder), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(Error::new(
            Kind::Usage,
            format!("{} is not a file name", path.display()),
        )
        .help("give -o a file name, such as data.sha256"));
    };
    match fs::symlink_metadata(&target) {
        Ok(found) if found.is_dir() => {
            return Err(Error::new(
                Kind::Usage,
                format!("{} is a folder, and -o names a file", path.display()),
            )
            .help("give -o a file name, such as data.sha256"));
        }
        Ok(_) if !force => return Err(taken(path)),
        _ => {}
    }
    let hidden = write_hidden(folder, &name.to_string_lossy(), text.as_bytes())
        .map_err(|err| Error::io(writing(), &err))?;
    let placer = Placer::in_named_folder(folder);
    let aside = if force {
        match placer.move_aside(&target) {
            Ok(aside) => Some(aside),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => {
                let _ = fs::remove_file(&hidden);
                return Err(Error::io(writing(), &err));
            }
        }
    } else {
        None
    };
    if let Err(err) = placer.move_back(&hidden, &target) {
        let _ = fs::remove_file(&hidden);
        let error = if err.kind() == io::ErrorKind::AlreadyExists {
            taken(path)
        } else {
            Error::io(writing(), &err)
        };
        // the file that was there gets its name back, or says where it is kept
        return Err(match aside {
            Some(aside) if placer.move_back(&aside, &target).is_err() => error.detail(format!(
                "the file that was there is kept as {}",
                aside.display()
            )),
            _ => error,
        });
    }
    // the old file goes only once the new one has its name
    if let Some(aside) = aside {
        let _ = placer.delete_file(&aside);
    }
    crate::project::sync_directory(folder).map_err(|err| {
        Error::io(
            format_args!(
                "wrote {}, but could not make sure it stays after a power cut",
                path.display()
            ),
            &err,
        )
    })?;
    Ok(target)
}

/// a new file in `folder` under a hidden name made from `name`, holding `bytes` flushed to disk,
/// and its path.
fn write_hidden(folder: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    loop {
        let hidden = folder.join(format!(".{name}.{:08x}.tmp", fastrand::u32(..)));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&hidden)
        {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        };
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        return match written {
            Ok(()) => Ok(hidden),
            Err(err) => {
                let _ = fs::remove_file(&hidden);
                Err(err)
            }
        };
    }
}

fn taken(path: &Path) -> Error {
    Error::new(
        Kind::LocalNotEmpty,
        format!("{} already exists", path.display()),
    )
    .help("add --force to replace it, or give -o another file name")
}

/// a path no line of an export may hold. fetchloom refuses such paths in data.lock and the cache,
/// so this means a record changed since.
fn unwritable(path: &str, reason: &str) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!("{path} cannot be exported because {reason}"),
    )
    .help(lock::BROKEN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_with_a_login_a_secret_or_a_control_character_is_not_written() {
        assert_eq!(
            link("https://example.org/data/a%20b.csv?download=1").as_deref(),
            Ok("https://example.org/data/a%20b.csv?download=1")
        );
        assert_eq!(
            link("https://example.org/caf\u{e9}.csv").as_deref(),
            Ok("https://example.org/caf%C3%A9.csv"),
            "a link is written in ascii"
        );
        for (text, reason) in [
            ("https://ada:pw@example.org/a.csv", "holds a login"),
            ("https://example.org/a.csv?token=t", "holds a secret"),
            (
                "https://bucket.s3.amazonaws.com/a.csv?X-Amz-Signature=s",
                "holds a secret",
            ),
            ("https://example.org/a.csv#access_token=t", "holds a secret"),
            (
                "https://example.org/a.csv\n  dir=/etc",
                "holds a space or a control character",
            ),
            (
                "https://example.org/a b.csv",
                "holds a space or a control character",
            ),
            (
                "https://example.org/\u{202e}vsc.exe",
                "holds a space or a control character",
            ),
            ("file:///etc/passwd", "is not an http or https link"),
            ("ftp://example.org/a.csv", "is not an http or https link"),
            ("not a link", "holds a space or a control character"),
            ("example.org/a.csv", "is not a link"),
        ] {
            assert_eq!(link(text), Err(reason), "{text:?}");
        }
    }
}
