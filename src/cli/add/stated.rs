//! `add --no-sync`: a dataset pinned in data.lock by what its source states of each file, its size
//! and SHA-256 or MD5, so nothing downloads until a sync, maybe on another machine, does.

use std::sync::atomic::Ordering;
use std::time::SystemTime;

use super::super::project::{Report, committed, dry_run_done, ignore_data, register};
use super::super::sync::{self as engine, Loop};
use super::{AddProject, BeforeAdd, changed_meanwhile, manifest_changed_meanwhile, restore};
use crate::error::{Error, Kind};
use crate::project::lock::{self, Lock, Locked, Stated, StatedFile, Words};
use crate::project::manifest::{self, Entry, Manifest};
use crate::source::{Listing, Parsed, Remote};
use crate::units;

/// the most files an error names; it counts the rest.
const NAMED: usize = 5;

/// the error for `--no-sync` with files on this machine, which no source states anything of.
pub(super) fn refused_path(typed: &str) -> Error {
    Error::new(
        Kind::Usage,
        "--no-sync pins what a source states, and files on this machine have no source",
    )
    .help(format!(
        "run `{}` without --no-sync, which copies them into the cache",
        super::super::project::command(&format!("add {}", crate::suggested::shell_word(typed)))
    ))
}

/// the error for `--no-sync` with several links, which it adds one at a time.
pub(super) fn refused_several() -> Error {
    Error::new(Kind::Usage, "--no-sync adds one link at a time").help(format!(
        "run `{}` for each link",
        super::super::project::command("add <link> --no-sync")
    ))
}

/// `add --no-sync` of `present`, which data.toml has already: other settings need the person's
/// choice, as for any add. while data.lock pins it as data.toml asks there is nothing to do, a
/// pin under a name data.toml renamed included, which a sync carries without asking the source;
/// else it is pinned as a first add pins it, and data.toml stays as it is.
pub(super) fn again(
    work: &Loop<'_>,
    adding: &AddProject,
    old: Option<Lock>,
    present: &super::Present<'_>,
    (parsed, typed): (&Parsed, &str),
    dry_run: bool,
) -> Result<Report, Error> {
    let manifest = &adding.manifest;
    super::same_settings(manifest, present)?;
    let name = present.name;
    let entry = &manifest.datasets[name];
    let is_pinned = old.as_ref().is_some_and(|lock| pins(lock, name, entry));
    let renamed = if is_pinned {
        None
    } else {
        carried(manifest, old.as_ref(), name, entry)
    };
    if is_pinned || renamed.is_some() {
        let output = &work.context.output;
        match renamed {
            Some(old) => {
                output.done(
                    "Ok",
                    &format!("{name} is already in data.toml, and data.lock pins it as {old}"),
                );
                output.hint(&format!(
                    "`{}` keeps that pin under this name without asking the source",
                    super::super::project::command("sync")
                ));
            }
            None => output.done("Ok", &format!("{name} is already in data.toml")),
        }
        return Ok(Report {
            project: work.project.root.clone(),
            dry_run,
            ..Report::default()
        });
    }
    let stated = pinned(
        name,
        entry,
        present.listing,
        (&parsed.reference.shown(), typed),
    )?;
    let text = adding.text.as_deref().unwrap_or_default();
    write(
        work,
        adding,
        (text, manifest),
        old,
        stated,
        (false, dry_run),
    )
}

/// whether `lock` pins `name` as `entry` asks: by its bytes, which a changed `extract` leaves
/// standing, or by what its source states.
fn pins(lock: &Lock, name: &str, entry: &Entry) -> bool {
    lock.get(name)
        .is_some_and(|locked| locked.drift(entry) != Some(lock::Drift::Spec))
        || lock
            .stated
            .iter()
            .any(|stated| stated.name() == name && stated.pin.drift(entry).is_none())
}

/// the name data.lock pins `name` under, one data.toml no longer has, when a sync carries that
/// pin to `name` as `entry` asks, as status foretells it.
fn carried(manifest: &Manifest, old: Option<&Lock>, name: &str, entry: &Entry) -> Option<String> {
    let (lock, renamed) = super::super::sync::rename::forecast(manifest, old?.clone());
    let rename = renamed.into_iter().find(|rename| rename.new == name)?;
    pins(&lock, name, entry).then_some(rename.old)
}

/// the pin of `name`, which `listing` resolved and `entry` chooses files of, by what its source
/// states of each file. a file whose source states no size and SHA-256 or MD5 is refused, as is
/// a dataset of more files than data.lock keeps inline: only their bytes could pin them.
pub(super) fn pinned(
    name: &str,
    entry: &Entry,
    listing: &Listing,
    (shown, typed): (&str, &str),
) -> Result<Stated, Error> {
    let kept = engine::filter(shown, entry, listing)?;
    let source = &entry.source;
    let unstated: Vec<&str> = kept
        .iter()
        .filter(|remote| stated(remote).is_none())
        .map(|remote| remote.path.as_str())
        .collect();
    let instead = || {
        format!(
            "run `{}` without --no-sync where the data can download",
            super::super::project::command(&format!("add {}", crate::suggested::shell_word(typed)))
        )
    };
    if !unstated.is_empty() {
        let what = units::count(unstated.len() as u64, "file", "files");
        return Err(Error::new(
            Kind::Usage,
            format!(
                "{shown} states no size with a SHA-256 or MD5 for {what}: {}",
                units::some_of(&unstated, NAMED)
            ),
        )
        .help(format!("pinning them needs their bytes: {}", instead())));
    }
    let most = lock::lists::inline();
    if kept.len() > most {
        return Err(Error::new(
            Kind::Usage,
            format!(
                "{shown} lists {} files, more than the {} data.lock keeps of a dataset not downloaded yet",
                units::grouped(kept.len() as u64),
                units::grouped(most as u64)
            ),
        )
        .help(format!("{}, or choose fewer files with --select", instead())));
    }
    let mut files: Vec<StatedFile> = kept.iter().filter_map(stated).collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let pin = Locked {
        name: name.to_owned(),
        reference: source.reference.clone(),
        spec: lock::spec(source),
        extract: source.extract,
        web_page: source.web_page,
        resolved: listing.resolved.to_string(),
        title: listing.title.clone(),
        license: listing.license.clone(),
        doi: listing.doi.clone(),
        access: listing.access,
        timestamp: units::timestamp(SystemTime::now()),
        files: Vec::new(),
    };
    Ok(Stated {
        written: Words::of(&pin),
        pin,
        files,
    })
}

/// `remote` as its source states it, when that pins it: a size its bytes must have, and a SHA-256
/// or an MD5.
fn stated(remote: &Remote) -> Option<StatedFile> {
    let size = remote.expected_size()?;
    if remote.named_by_server || (remote.sha256.is_none() && remote.md5.is_none()) {
        return None;
    }
    Some(StatedFile {
        path: remote.path.clone(),
        size,
        sha256: remote.sha256,
        md5: remote.md5,
        url: remote.url.to_string(),
    })
}

/// the line saying that MD5 alone checks the first download of the files of `stated` its source
/// states no SHA-256 of, and that this sync records the SHA-256 every later one checks; none when
/// every file has one.
fn md5_alone(stated: &Stated) -> Option<&'static str> {
    let unstated = stated
        .files
        .iter()
        .filter(|file| file.sha256.is_none())
        .count();
    match unstated {
        0 => None,
        all if all == stated.files.len() => Some(
            "the first sync checks these files by MD5 alone, then records their SHA-256 for every later sync",
        ),
        _ => Some(
            "the first sync checks each file with no stated SHA-256 by MD5 alone, then records its SHA-256 for every later sync",
        ),
    }
}

/// `old` with `stated` in place of any entry of its name.
fn with(old: Option<Lock>, stated: Stated) -> Lock {
    let mut lock = old.unwrap_or_default();
    let name = stated.name().to_owned();
    lock.datasets.retain(|locked| locked.name != name);
    lock.stated.retain(|other| other.name() != name);
    lock.stated.push(stated);
    lock.stated.sort_by(|a, b| a.name().cmp(b.name()));
    lock
}

/// the line naming what the pin of `stated` holds, as a first pin's line does.
fn facts(work: &Loop<'_>, stated: &Stated) -> String {
    let mut facts = vec![stated.pin.resolved.clone()];
    facts.extend(
        stated
            .pin
            .title
            .as_deref()
            .map(|title| work.context.output.clip(title, 50)),
    );
    facts.push(units::count(stated.files.len() as u64, "file", "files"));
    facts.push(units::size(stated.bytes()));
    format!(
        "{} in {} by its source's {}: {}",
        stated.name(),
        lock::FILE,
        stated.digests(),
        facts.join(" \u{b7} ")
    )
}

/// writes data.lock with `lock`, then data.toml as `updated` when the add changes it, once
/// neither changed since the add read them. data.lock goes first, as any add writes it: a stop
/// between leaves a pin data.toml does not name yet, never a dataset in data.toml without its
/// pin. a failure puts back what the add wrote.
fn commit(
    work: &Loop<'_>,
    adding: &AddProject,
    lock: &Lock,
    updated: Option<&str>,
    name: &str,
) -> Result<(), Error> {
    let project = work.project;
    let text = lock.render();
    let changed = adding.changed();
    if !changed.is_empty() {
        return Err(changed_meanwhile(&changed));
    }
    let mut saved = BeforeAdd::save(
        project,
        (adding.text.as_deref(), adding.lock_text.as_deref()),
    )?;
    // the recovery copies are flushed to disk, which takes a while, so it looks again right
    // before the first write
    let changed = adding.changed();
    if !changed.is_empty() {
        return Err(changed_meanwhile(&changed));
    }
    work.context.stop.keep_at_quit();
    let written = (|| -> Result<(), Error> {
        saved.writing(lock::FILE, text.as_bytes());
        work.context
            .wrote_lists(&project.root, project.write_lists(lock)?);
        project.write(lock::FILE, text.as_bytes())?;
        if let Some(updated) = updated {
            if adding.differs(manifest::FILE, adding.text.as_deref()) {
                return Err(manifest_changed_meanwhile());
            }
            saved.writing(manifest::FILE, updated.as_bytes());
            project.write(manifest::FILE, updated.as_bytes())?;
        }
        Ok(())
    })();
    if let Err(error) = written {
        let error = restore(project, &mut saved, &[name.to_owned()], error);
        super::unwrite_lists(work.context, project);
        if saved.keep {
            adding.unrestored.store(true, Ordering::Relaxed);
        }
        return Err(error);
    }
    Ok(())
}

/// writes `updated`, data.toml with the new dataset, and data.lock with `stated`, or with
/// `--dry-run` says it would. data.toml that has the dataset already stays as it is. nothing
/// downloads; a failure puts back what the add wrote.
pub(super) fn write(
    work: &Loop<'_>,
    adding: &AddProject,
    (updated, manifest): (&str, &Manifest),
    old: Option<Lock>,
    stated: Stated,
    (starting, dry_run): (bool, bool),
) -> Result<Report, Error> {
    let output = &work.context.output;
    let project = work.project;
    let name = stated.name().to_owned();
    let unchecked = md5_alone(&stated);
    let again = adding.manifest.datasets.contains_key(&name);
    let datasets = if again {
        Vec::new()
    } else {
        vec![name.clone()]
    };
    if dry_run {
        output.step("Would pin", &facts(work, &stated));
        if let Some(line) = unchecked {
            output.item(line);
        }
        for file in &stated.files {
            output.detail(&format!("{name}/{}", file.path));
        }
        super::super::project::preview_ignore(work, manifest)?;
        if !again {
            output.step(
                "Would add",
                &format!("{name} to data.toml, downloading nothing"),
            );
        }
        dry_run_done(output, None);
        return Ok(Report {
            project: project.root.clone(),
            datasets,
            lock_changed: true,
            dry_run: true,
            ..Report::default()
        });
    }
    let said = facts(work, &stated);
    let lock = with(old, stated);
    commit(work, adding, &lock, (!again).then_some(updated), &name)?;
    work.context.stop.finish();
    // the list of a pin this one replaced goes, as with any write of data.lock
    project.sweep_lists(&lock);
    register(work);
    if starting {
        output.step(
            "Started",
            &format!("a project in {}", units::path(&project.root)),
        );
    }
    output.step("Pinned", &said);
    if let Some(line) = unchecked {
        output.item(line);
    }
    ignore_data(work, manifest, starting || adding.begins())?;
    if again {
        output.done(
            "Ok",
            &format!("{name} is already in data.toml, nothing downloaded"),
        );
    } else {
        output.done("Added", &format!("{name} to data.toml, nothing downloaded"));
    }
    output.next(&format!(
        "commit {}; where the data can download, `{}` downloads and checks it",
        committed(project),
        super::super::project::command(&format!("sync {name}"))
    ));
    Ok(Report {
        project: project.root.clone(),
        datasets,
        lock_changed: true,
        ..Report::default()
    })
}
