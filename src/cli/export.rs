//! `fetchloom export`: the SHA-256 of placed files, or the link and SHA-256 of downloaded ones, as
//! `sha256sum -c` input, a list of links or an aria2c input file, asking no server; a citation of
//! each dataset, asking doi.org about each DOI; or a data availability statement. a text form is
//! complete or not written, but for datasets with nothing to write, which it names.

mod cite;

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
use crate::output::Warning;
use crate::place::Placer;
use crate::plan;
use crate::project::lock::{self, Lock, Locked, Waiting};
use crate::project::manifest::{Entry, Manifest};

#[derive(Default, Serialize)]
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
    /// for bibtex, csl and ris, what the citation of each dataset names.
    citations: Vec<cite::Cited>,
    /// why each dataset in `unlocked` is there.
    #[serde(skip)]
    stale: Vec<String>,
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
    /// its files were imported from a folder on this machine, so no link holds them.
    Imported,
    /// `fetchloom drop` freed its files on this machine.
    Dropped,
    /// its file list, which data.lock names, is not there, so none of its files is known.
    Waiting,
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
    // a citation and the statement need no SHA-256, which a dataset its source's MD5 pins lacks
    let checksums = !args.format.cites() && args.format != Format::Statement;
    if args.names.is_empty() {
        if checksums {
            lock.refuse_stated(manifest.datasets.keys().map(String::as_str))?;
        }
    } else {
        lock.refuse_waiting(args.names.iter().map(String::as_str))?;
        if checksums {
            lock.refuse_stated(args.names.iter().map(String::as_str))?;
        }
    }
    let waiting: Vec<Waiting> = lock.still_waiting().cloned().collect();
    let pins = pins(&lock, &manifest, args);
    let unfetched = lock.stated.clone();
    let dropped = project.dropped();
    // the data folder as this machine holds it: a dataset dropped here has no files there, and
    // one waiting for its file list has none known
    let mut held = manifest.clone();
    super::drop::left_out(&mut held, &dropped);
    held.datasets
        .retain(|name, _| !waiting.iter().any(|waiting| waiting.name == *name));
    let mut report = inventory(&held, lock, &super::peek_cache()?, &args.names)?;
    report.left_out = left_out(&manifest, (&dropped, &waiting), &args.names, args.format);
    let output = &context.output;
    super::project::tell_waiting(output, &waiting, &super::sync::Scope::All);
    if manifest.datasets.is_empty() {
        output.done("Ok", "data.toml has no datasets yet");
        return Ok(report);
    }
    if output.json() && args.output.is_none() {
        if args.format.cites() {
            report.citations = cite::cite(context, &cited(&manifest, &pins, &report), link_of)?;
        }
        return Ok(report);
    }
    if let Err(error) = complete(&report, args.format) {
        return Err(error.with_report(&report));
    }
    let (text, count) = match args.format {
        format if format.cites() => {
            report.citations = cite::cite(context, &cited(&manifest, &pins, &report), link_of)?;
            let text = cite::render(&report.citations, format);
            (text, report.citations.len() as u64)
        }
        Format::Statement => {
            let stated = stated(&manifest, (&pins, &unfetched), &report);
            (cite::statement(&stated), stated.len() as u64)
        }
        format => render(&report, format),
    };
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
        Err(error) => return Err(error.with_report(&report)),
    }
    let what = match args.format {
        Format::Sha256 => crate::units::count(count, "file", "files"),
        Format::Links | Format::Aria2 => crate::units::count(count, "link", "links"),
        Format::Bibtex | Format::Csl | Format::Ris => {
            crate::units::count(count, "citation", "citations")
        }
        Format::Statement => format!(
            "a statement of {}",
            crate::units::count(count, "dataset", "datasets")
        ),
    };
    output.done("Exported", &format!("{what} to {}", path.display()));
    tell_imported(output, &report, args.format);
    tell_left_out(output, &report);
    Ok(report)
}

fn tell_imported(output: &crate::output::Output, report: &Report, format: Format) {
    if format.cites() {
        let names: Vec<&str> = report
            .imported
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        cite::tell_imported(output, &names);
        return;
    }
    if matches!(format, Format::Sha256 | Format::Statement) || report.imported.is_empty() {
        return;
    }
    let files: u64 = report.imported.iter().map(|(_, files)| files).sum();
    let names: Vec<&str> = report
        .imported
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let datasets = crate::units::listed(&names, "and");
    let (they_were, them) = if files == 1 {
        ("it was", "it")
    } else {
        ("they were", "them")
    };
    output.warn_with(
        Warning::ExportLeftOut,
        &format!(
            "the list leaves out {} of {datasets}: {they_were} imported from a folder on this machine, so no link holds {them}",
            crate::units::count(files, "file", "files")
        ),
        &format!(
            "share {them} through a folder mirror, which `{}` fills",
            super::project::command("push --to <folder>")
        ),
    );
}

/// the datasets `names`, or every one when none is named, that `format` never lists, that
/// `dropped` here leaves out of the data folder, or that wait for their file list.
fn left_out(
    manifest: &Manifest,
    (dropped, waiting): (&std::collections::BTreeSet<String>, &[Waiting]),
    names: &[String],
    format: Format,
) -> Vec<LeftOut> {
    manifest
        .datasets
        .iter()
        .filter(|(name, _)| names.is_empty() || names.contains(name))
        .filter_map(|(name, entry)| {
            let reason = match format {
                _ if waiting.iter().any(|waiting| waiting.name == *name) => Reason::Waiting,
                Format::Sha256 if dropped.contains(name) => Reason::Dropped,
                Format::Links | Format::Aria2 | Format::Bibtex | Format::Csl | Format::Ris
                    if entry.is_import() =>
                {
                    Reason::Imported
                }
                _ => return None,
            };
            Some(LeftOut {
                dataset: name.clone(),
                reason,
            })
        })
        .collect()
}

/// imports are left to [`tell_imported`], whose warning counts their files.
fn tell_left_out(output: &crate::output::Output, report: &Report) {
    let named = |reason: Reason| -> Vec<&str> {
        report
            .left_out
            .iter()
            .filter(|left| left.reason == reason)
            .map(|left| left.dataset.as_str())
            .collect()
    };
    let dropped = named(Reason::Dropped);
    if !dropped.is_empty() {
        let datasets = crate::units::listed(&dropped, "and");
        let (is, its) = if dropped.len() == 1 {
            ("it is", "its")
        } else {
            ("they are", "their")
        };
        output.warn(
            Warning::ExportLeftOut,
            &format!(
                "the list leaves out {datasets}: {is} dropped on this machine, so {its} files are not in the data folder"
            ),
        );
    }
}

/// what the datasets `names`, or every one when none is named, pin, and what is not known here.
fn inventory(
    manifest: &Manifest,
    mut lock: Lock,
    cache: &Cache,
    names: &[String],
) -> Result<Report, Error> {
    let wanted = |name: &str| names.is_empty() || names.iter().any(|named| named == name);
    let mut report = Report::default();
    for (name, entry) in &manifest.datasets {
        if !wanted(name) {
            continue;
        }
        let pinned = lock
            .get(name)
            .or_else(|| lock.stated(name).map(|stated| &stated.pin));
        let stale = match pinned.map(|locked| locked.drift(entry)) {
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
                .file
                .unpacks_to
                .is_some()
                .then(|| target.file.path.clone()),
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

/// the pins of the datasets `args` names, or of every one, that a citation or a statement names,
/// which the inventory does not keep.
fn pins(lock: &Lock, manifest: &Manifest, args: &args::Export) -> Vec<Locked> {
    if !args.format.cites() && args.format != Format::Statement {
        return Vec::new();
    }
    let wanted = |name: &str| args.names.is_empty() || args.names.iter().any(|named| named == name);
    let stated = lock.stated.iter().map(|stated| &stated.pin);
    let mut pins: Vec<Locked> = lock
        .datasets
        .iter()
        .chain(stated)
        .filter(|locked| wanted(&locked.name) && manifest.datasets.contains_key(&locked.name))
        .cloned()
        .collect();
    pins.sort_by(|a, b| a.name.cmp(&b.name));
    pins
}

/// the pins of `pins` a citation names: those `report` does not leave out, but for imports, which
/// no source holds.
fn cited(manifest: &Manifest, pins: &[Locked], report: &Report) -> Vec<Locked> {
    pins.iter()
        .filter(|locked| !report.unlocked.contains(&locked.name) && !imported(manifest, locked))
        .cloned()
        .collect()
}

/// whether `locked` is a dataset data.toml imports from a folder, which no source holds.
fn imported(manifest: &Manifest, locked: &Locked) -> bool {
    manifest
        .datasets
        .get(&locked.name)
        .is_some_and(Entry::is_import)
}

/// the link a reader can open for `locked` where it has no DOI: the link it was pinned from, when
/// that is a web link export would write.
fn link_of(locked: &Locked) -> Option<String> {
    match crate::source::Reference::parse(&locked.resolved)
        .ok()?
        .reference
    {
        crate::source::Reference::Url(url) => link(url.as_str()).ok(),
        _ => None,
    }
}

/// each of `pins` that `report` does not leave out, with its files and their SHA-256, for a data
/// availability statement.
fn stated<'a>(
    manifest: &Manifest,
    (pins, unfetched): (&'a [Locked], &'a [lock::Stated]),
    report: &'a Report,
) -> Vec<cite::Stated<'a>> {
    pins.iter()
        .filter(|locked| !report.unlocked.contains(&locked.name))
        .map(|locked| {
            let unfetched = unfetched.iter().find(|stated| stated.name() == locked.name);
            let (files, by_source) = if let Some(stated) = unfetched {
                let files = stated.files.iter();
                let files = files.map(|file| (file.path.as_str(), stated_checksum(file)));
                (files.collect(), Some(stated.digests()))
            } else {
                let files = locked.files.iter();
                let files = files.map(|file| (file.path.as_str(), file.sha256.to_string()));
                (files.collect(), None)
            };
            cite::Stated {
                locked,
                imported: imported(manifest, locked),
                link: link_of(locked),
                files,
                by_source,
            }
        })
        .collect()
}

/// what pins `file` while no sync downloaded it: the SHA-256 its source states, else its MD5.
/// data.lock gives each such file one or both.
fn stated_checksum(file: &lock::StatedFile) -> String {
    match (file.sha256, file.md5) {
        (Some(sha256), _) => sha256.to_string(),
        (None, Some(md5)) => md5.to_string(),
        (None, None) => String::new(),
    }
}

/// `text` as export and list write a link, or why not: fetchloom never pins a link with a login
/// or a secret, or one a line cannot hold, so such a link was written by someone else.
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
    if crate::shown::secret(&url).is_some() {
        return Err("holds a secret");
    }
    Ok(url.into())
}

/// refuses a text answer that would leave out what `format` needs.
fn complete(report: &Report, format: Format) -> Result<(), Error> {
    if !report.stale.is_empty() {
        let error = Error::new(Kind::Outdated, "data.lock is out of date with data.toml");
        return Err(report
            .stale
            .iter()
            .fold(error, |error, line| error.detail(line.clone()))
            .help(format!(
                "run `{}` and commit data.lock, then export again",
                super::project::command("lock")
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
            Err(error.help(format!(
                "run `{}` to unpack them here, then export again",
                super::project::command("sync")
            )))
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
            Err(error.help(lock::recovery()))
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
        // cited and stated apart, from data.lock and doi.org
        Format::Bibtex | Format::Csl | Format::Ris | Format::Statement => {}
    }
    (text, count)
}

/// writes `text` in full under a hidden name beside `path`, flushed, then names it without
/// replacing anything; with `force`, a file there is moved aside until the new one has its name.
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
    crate::volume::sync_directory(folder).map_err(|err| {
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
    .help(lock::recovery())
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
