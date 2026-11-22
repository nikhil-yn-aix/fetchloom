//! pinned files and archive trees from the mirrors a run names, asked in order before sources. one
//! not there is skipped after one warning, and every file one gives is checked against its pin.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use super::Job;
use super::data_folder::DataFolder;
use crate::cache::Cache;
use crate::cli::Context;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::mirror::folder::{self, Gave, Missing, Seen};
use crate::mirror::{Location, Mirrors, Wanted, s3, web};
use crate::net::{Fetched, Net};
use crate::output::Warning;
use crate::project::lock::archives::Archive;
use crate::settings::{Origin, Settings};
use crate::stop::Stop;
use crate::units;
use crate::work::Work;

/// damaged copies one mirror's warnings name before they are counted instead.
const NAMED: usize = 5;

/// what a mirror answered for a copy that is not the pinned file.
const DAMAGED: &str = "a damaged copy";

/// what a mirror said of a file it does not hold.
const NOT_THERE: &str = "not there";

/// what an S3 mirror said of a file it refused, as S3 refuses a key that is not there to a reader
/// not allowed to list the bucket.
const UNLISTED: &str = "not there, or not readable without s3:ListBucket";

/// the mirrors a run asks, and what they answered, so no mirror is asked twice for one file and
/// one that is not there is asked nothing more.
#[derive(Default)]
pub(in crate::cli) struct Mirrored {
    pub(in crate::cli) mirrors: Mirrors,
    answers: Mutex<Answers>,
}

#[derive(Default)]
struct Answers {
    /// mirrors found there this run, by their place in the list.
    up: HashSet<usize>,
    /// mirrors this run skips, with what they answered.
    down: HashMap<usize, String>,
    /// what a mirror answered for a file or tree it did not give.
    lacked: HashMap<(usize, Sha256), String>,
    /// the link below which each S3 mirror found there keeps its files, in its bucket's region.
    bases: HashMap<usize, url::Url>,
    /// by pin, the folder mirror whose copy hashed to it, the copy as it was, and the file's name,
    /// which a run that frees the cache's file for that copy looks at again first.
    hashed: HashMap<Sha256, (usize, Seen, String)>,
}

/// why a mirror is skipped for the rest of the run: what to warn, the help, and its answer in
/// short. no warning for one a person chose to skip, as a web or S3 mirror while offline.
struct Skip {
    warning: Option<(String, String)>,
    said: String,
}

impl Mirrored {
    pub(in crate::cli) fn new(mirrors: Mirrors) -> Mirrored {
        Mirrored {
            mirrors,
            answers: Mutex::default(),
        }
    }

    fn answers(&self) -> std::sync::MutexGuard<'_, Answers> {
        self.answers.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// whether mirror `index` already said it lacks `digest`.
    fn lacks(&self, index: usize, digest: &Sha256) -> bool {
        self.answers().lacked.contains_key(&(index, *digest))
    }

    fn lacked(&self, index: usize, digest: Sha256, said: String) {
        self.answers().lacked.insert((index, digest), said);
    }

    /// notes that mirror `index`, looked at without taking anything, does not hold the file
    /// pinned to `digest`, in the words a sync's request there gives.
    pub(in crate::cli) fn not_there(&self, index: usize, digest: Sha256) {
        let said = match self.mirrors.list.get(index) {
            Some(Location::S3(_)) => UNLISTED,
            _ => NOT_THERE,
        };
        self.lacked(index, digest, said.to_owned());
    }

    /// notes that folder mirror `index` holds a copy of the file `label` that hashed to `digest`,
    /// as `seen` says.
    pub(in crate::cli) fn hashed(&self, index: usize, digest: Sha256, seen: Seen, label: String) {
        self.answers().hashed.insert(digest, (index, seen, label));
    }

    /// the folder mirror and the file's name, when the copy this run found hashed to `digest`
    /// changed since, by one look through the mirror's real folders.
    pub(in crate::cli) fn changed(&self, digest: &Sha256) -> Option<(std::path::PathBuf, String)> {
        let (index, seen, label) = self.answers().hashed.get(digest).cloned()?;
        let Some(Location::Folder(root)) = self.mirrors.list.get(index) else {
            return None;
        };
        (!folder::still(root, digest, &seen)).then(|| (root.clone(), label))
    }

    /// the first folder mirror that holds a damaged copy of the file pinned to `digest`, which a
    /// plain push leaves as it is when it has the pinned size.
    pub(in crate::cli) fn damaged_folder(
        &self,
        digest: Option<Sha256>,
    ) -> Option<std::path::PathBuf> {
        let digest = digest?;
        let answers = self.answers();
        self.mirrors
            .list
            .iter()
            .enumerate()
            .find_map(|(index, location)| match location {
                Location::Folder(root)
                    if answers
                        .lacked
                        .get(&(index, digest))
                        .is_some_and(|said| said == DAMAGED) =>
                {
                    Some(root.clone())
                }
                _ => None,
            })
    }

    /// the link the web or S3 mirror `index` at `location` keeps its files below, once
    /// [`Mirrored::usable`] found it there.
    pub(in crate::cli) fn base(&self, index: usize, location: &Location) -> Option<url::Url> {
        match location {
            Location::Web(base) => Some(base.clone()),
            Location::S3(_) => self.answers().bases.get(&index).cloned(),
            Location::Folder(_) => None,
        }
    }

    /// what each mirror said of the file pinned to `digest`, in order, or that none can be asked
    /// for a new file, whose SHA-256 is not known yet.
    fn said(&self, digest: Option<&Sha256>) -> String {
        let Some(digest) = digest else {
            return "new, so no mirror can be asked for it".to_owned();
        };
        let answers = self.answers();
        self.mirrors
            .list
            .iter()
            .enumerate()
            .map(|(index, location)| {
                let said = answers
                    .lacked
                    .get(&(index, *digest))
                    .or_else(|| answers.down.get(&index))
                    .map_or("not asked", String::as_str);
                format!("{said} on {location}")
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// skips mirror `index` for the rest of the run, warning once, though it was found there.
    fn skip(&self, context: &Context, index: usize, skip: Skip) {
        let mut answers = self.answers();
        answers.up.remove(&index);
        if answers.down.contains_key(&index) {
            return;
        }
        if let Some((warning, help)) = &skip.warning {
            context
                .output
                .warn_with(Warning::MirrorSkipped, warning, help);
        }
        answers.down.insert(index, skip.said);
    }

    /// whether mirror `index` may be asked: its marker was found, once a run. one without it is
    /// told once and skipped, a web or S3 mirror is never asked offline, and a stop while asking
    /// fails.
    async fn usable(
        &self,
        context: &Context,
        net: &Net,
        index: usize,
        location: &Location,
    ) -> Result<bool, Error> {
        {
            let answers = self.answers();
            if answers.up.contains(&index) {
                return Ok(true);
            }
            if answers.down.contains_key(&index) {
                return Ok(false);
            }
        }
        let skip = match location {
            Location::Folder(root) => folder_missing(location, folder::probe(root).err()),
            Location::Web(_) | Location::S3(_) if net.offline().enabled() => Some(Skip {
                warning: None,
                said: "not asked offline".to_owned(),
            }),
            Location::Web(base) => web_missing(location, web::probe(net, base).await?),
            Location::S3(top) => match s3::probe(net, top).await? {
                s3::Look::There(base) => {
                    self.answers().bases.insert(index, base);
                    None
                }
                look => bucket_missing(location, top, look),
            },
        };
        match skip {
            None => {
                self.answers().up.insert(index);
                Ok(true)
            }
            Some(skip) => {
                self.skip(context, index, skip);
                Ok(false)
            }
        }
    }

    /// how mirror `index` is asked whether it holds a file, as [`fill`] asks it for one: a web or
    /// S3 mirror below the link [`Mirrored::up`] found it at, a bucket's in the region S3 named.
    pub(in crate::cli) fn look(&self, index: usize) -> Look<'_> {
        let location = &self.mirrors.list[index];
        let base = match location {
            Location::Folder(root) => return Look::Folder(root),
            Location::Web(base) => base.clone(),
            Location::S3(top) => self.base(index, location).unwrap_or_else(|| top.link(None)),
        };
        Look::Ask(Box::new(move |want| web::object(&base, want)))
    }

    /// the mirrors this run may ask, by their place in its list, each found or skipped once a run
    /// as sync finds it. fails at a stop.
    pub(in crate::cli) async fn up(
        &self,
        context: &Context,
        net: &Net,
    ) -> Result<Vec<usize>, Error> {
        let mut up = Vec::new();
        for (index, location) in self.mirrors.list.iter().enumerate() {
            if self.usable(context, net, index, location).await? {
                up.push(index);
            }
        }
        Ok(up)
    }
}

/// how a mirror is asked whether it holds a pinned file at its size, taking nothing of it.
pub(in crate::cli) enum Look<'m> {
    /// a folder mirror, whose copy is looked at unread.
    Folder(&'m std::path::Path),
    /// a mirror asked over the network, by the request this makes for a file's object.
    Ask(Box<dyn Fn(&Wanted) -> crate::net::Fetch + Send + Sync + 'm>),
}

/// why the folder mirror at `location` is skipped, when its look found it `missing`.
fn folder_missing(location: &Location, missing: Option<Missing>) -> Option<Skip> {
    let (warning, help, said) = match missing? {
        Missing::Folder => (
            format!("the mirror {location} is not there, so this run skips it"),
            "mount it, or check the mirrors setting and --mirror".to_owned(),
            "not there".to_owned(),
        ),
        Missing::Marker => (
            format!("the mirror {location} holds no fetchloom-mirror file, so this run skips it"),
            format!("check that {location} is a mirror's top folder, and that its disk is mounted"),
            "no fetchloom-mirror file".to_owned(),
        ),
        Missing::Unreadable(err) => (
            format!(
                "could not read the mirror {location}: {}, so this run skips it",
                crate::error::said(&err)
            ),
            "check that this account may read it".to_owned(),
            crate::error::said(&err),
        ),
    };
    Some(Skip {
        warning: Some((warning, help)),
        said,
    })
}

/// why the web mirror at `location` is skipped, after its marker was asked for.
fn web_missing(location: &Location, look: web::Look) -> Option<Skip> {
    let (warning, help, said) = match look {
        web::Look::There => return None,
        web::Look::NoMarker => (
            format!("the mirror {location} holds no fetchloom-mirror file, so this run skips it"),
            "check that the address names a mirror's top folder".to_owned(),
            "no fetchloom-mirror file".to_owned(),
        ),
        web::Look::Answered(status, signed) => (
            format!("the mirror {location} answered {status}, so this run skips it"),
            refused_help(location, status, signed.as_ref()),
            format!("answered {status}"),
        ),
        web::Look::Failed(error) => {
            let (warning, help) = skip_warning(location, &error, || {
                format!(
                    "could not reach the mirror {location}: {}, so this run skips it",
                    error.message
                )
            });
            (warning, help, error.message)
        }
    };
    Some(Skip {
        warning: Some((warning, help)),
        said,
    })
}

/// the warning and help for the mirror at `location`, skipped once a request to it ended with
/// `error`; `otherwise` words one no answer came from. a mirror that answered is never told to
/// check its address or called unreachable: one that asked to wait is busy, one that refused
/// names its sign-in, and a server error points at whoever keeps it.
fn skip_warning(
    location: &Location,
    error: &Error,
    otherwise: impl FnOnce() -> String,
) -> (String, String) {
    match error.kind {
        Kind::NetworkRateLimited => (
            format!(
                "the mirror {location} is busy: {}, so this run skips it",
                error.message
            ),
            "try again later, or ask whoever keeps the mirror".to_owned(),
        ),
        Kind::AuthRequired => (
            format!(
                "the mirror {location} refused access: {}, so this run skips it",
                error.message
            ),
            refused_help(location, 401, None),
        ),
        Kind::NetworkStatus => (
            format!(
                "the mirror {location} failed: {}, so this run skips it",
                error.message
            ),
            "ask whoever keeps the mirror, or try again later".to_owned(),
        ),
        _ => (
            otherwise(),
            "check the address, and that this machine can reach it".to_owned(),
        ),
    }
}

/// what a mirror's failure to give a file says of the file, when it says the mirror lacks it
/// rather than that the mirror failed: a 404, or on S3 a refusal, since S3 answers 403 for a key
/// that is not there to a reader not allowed to list the bucket.
fn lacking(location: &Location, error: &Error) -> Option<String> {
    match (error.kind, location) {
        (Kind::ReferenceUnresolved, _) => Some(NOT_THERE.to_owned()),
        (Kind::AuthRequired, Location::S3(_)) => Some(UNLISTED.to_owned()),
        _ => None,
    }
}

/// why the S3 mirror at `location`, `top`, is skipped, after its marker was asked for.
fn bucket_missing(location: &Location, top: &s3::Top, look: s3::Look) -> Option<Skip> {
    let (warning, help, said) = match look {
        s3::Look::There(_) => return None,
        s3::Look::NoMarker => (
            format!("the mirror {location} holds no fetchloom-mirror file, so this run skips it"),
            "check that the prefix names a mirror's top".to_owned(),
            "no fetchloom-mirror file".to_owned(),
        ),
        s3::Look::NoBucket => (
            format!("the bucket of the mirror {location} does not exist, so this run skips it"),
            "check the bucket's name: names are lowercase and exact".to_owned(),
            "no such bucket".to_owned(),
        ),
        s3::Look::Refused(keyed) => (
            format!("the mirror {location} refused to be read, so this run skips it"),
            s3::Look::refused_help(top, keyed.as_ref()),
            "refused".to_owned(),
        ),
        s3::Look::Answered(status) => (
            format!("the mirror {location} answered {status}, so this run skips it"),
            "try again later, or ask whoever keeps the mirror".to_owned(),
            format!("answered {status}"),
        ),
        s3::Look::Failed(error) => {
            let (warning, help) = skip_warning(location, &error, || {
                format!(
                    "could not reach the mirror {location}: {}, so this run skips it",
                    error.message
                )
            });
            (warning, help, error.message)
        }
    };
    Some(Skip {
        warning: Some((warning, help)),
        said,
    })
}

/// what to do about a web mirror that answered `status` instead of a file, after `signed`, what
/// a saved credential sent with the request, when one went.
fn refused_help(
    location: &Location,
    status: u16,
    signed: Option<&crate::net::site::Sent>,
) -> String {
    match (status, location, signed) {
        (401 | 403, Location::Web(_), Some(signed)) => signed.refused_here(),
        (401 | 403, Location::Web(base), None) => format!(
            "if it needs a login, sign in with `{}`",
            crate::mirror::sign_in(base)
        ),
        _ => "ask whoever keeps the mirror, or check its address".to_owned(),
    }
}

/// the mirrors this run asks: those `given` with `--mirror`, then the settings' list. `usage` for a
/// `--mirror` that is no folder, web address or S3 bucket, or `mirrors_only` with no mirror to ask.
pub(in crate::cli) fn read(
    given: &[String],
    settings: &Settings,
    hf_endpoint: Option<&crate::net::Endpoint>,
) -> Result<Mirrors, Error> {
    let mut flagged = Vec::with_capacity(given.len());
    for text in given {
        let location = Location::parse(text, None).map_err(|refusal| {
            Error::new(
                Kind::Usage,
                format!(
                    "--mirror {} is no mirror: {}",
                    crate::shown::mask_typed(text),
                    refusal.reason
                ),
            )
            .help(refusal.instead)
        })?;
        flagged.push(location);
    }
    let only = settings.mirrors_only;
    let flagged = at_endpoints(flagged)?;
    let listed = at_endpoints(settings.mirrors.value.clone())?;
    let mirrors = Mirrors::new(flagged, &listed, only.value);
    // a mirror of huggingface.co a person set is a mirror they named
    if mirrors.only && mirrors.list.is_empty() && hf_endpoint.is_none() {
        let variable = crate::settings::variable("mirrors_only");
        let (named, undo) = if only.origin == Origin::File {
            (
                "mirrors_only in the settings file".to_owned(),
                "turn it off with `fetchloom settings unset mirrors_only`".to_owned(),
            )
        } else {
            (variable.to_owned(), format!("unset {variable}"))
        };
        return Err(Error::new(
            Kind::Usage,
            format!("{named} asks for mirrors alone, but no mirror is set"),
        )
        .help(format!(
            "name one with `fetchloom settings set mirrors <folder>` or FETCHLOOM_MIRRORS, give --mirror, or {undo}"
        )));
    }
    Ok(mirrors)
}

/// `locations`, with each S3 mirror written as `s3://bucket` at the service the AWS settings send
/// S3 requests to, as the aws cli takes it, once they name one; `usage` for an endpoint setting
/// that is no link.
fn at_endpoints(locations: Vec<Location>) -> Result<Vec<Location>, Error> {
    let mut endpoint: Option<Option<url::Origin>> = None;
    let mut homed = Vec::with_capacity(locations.len());
    for location in locations {
        let Location::S3(top) = location else {
            homed.push(location);
            continue;
        };
        if top.bucket().endpoint().is_some() {
            homed.push(Location::S3(top));
            continue;
        }
        let found = match &endpoint {
            Some(found) => found.clone(),
            None => endpoint
                .insert(crate::auth::aws::s3_endpoint(&crate::machine::env)?)
                .clone(),
        };
        homed.push(Location::S3(top.at(found.as_ref())));
    }
    Ok(homed)
}

/// whether a request for `url` goes to the mirror of huggingface.co `HF_ENDPOINT` names, which
/// mirror-only mode asks as it asks the mirrors a person named.
pub(in crate::cli) fn at_hf_endpoint(context: &Context, url: &url::Url) -> bool {
    context
        .hf_endpoint
        .as_ref()
        .is_some_and(|endpoint| endpoint.rewrite(url).is_some())
}

/// whether `reference` resolves at the mirror of huggingface.co `HF_ENDPOINT` names, as a
/// hugging face repository does, which mirror-only mode lets it.
pub(in crate::cli) fn resolves_at_hf_endpoint(
    context: &Context,
    reference: &crate::source::Reference,
) -> bool {
    context.hf_endpoint.is_some() && matches!(reference, crate::source::Reference::HuggingFace(_))
}

/// the error for what only a source gives, `what`, while `mirrors_only` lets fetchloom ask its
/// mirrors alone.
pub(in crate::cli) fn refused(context: &Context, what: &str) -> Error {
    refusal(
        context,
        what,
        "ask whoever keeps the mirrors to add what is missing",
    )
}

/// [`refused`], with `remedy` as what to do short of asking sources too.
fn refusal(context: &Context, what: &str, remedy: &str) -> Error {
    let off = if context.settings.mirrors_only.origin == Origin::File {
        "run `fetchloom settings unset mirrors_only`"
    } else {
        "unset FETCHLOOM_MIRRORS_ONLY"
    };
    Error::new(
        Kind::NetworkRefused,
        format!("{what}, and mirrors_only lets fetchloom ask its mirrors alone"),
    )
    .help(format!("{remedy}, or {off} to ask sources too"))
}

/// refuses, in mirror-only mode, to download `files` from their sources, with `network.refused`
/// naming each with what each mirror said of it. a file without a SHA-256 is new, so no mirror can
/// be asked for it.
pub(in crate::cli) fn sources_allowed(
    context: &Context,
    files: &[(String, Option<Sha256>)],
) -> Result<(), Error> {
    if !context.mirrors.mirrors.only || files.is_empty() {
        return Ok(());
    }
    let new = files.iter().filter(|(_, digest)| digest.is_none()).count() as u64;
    let pinned = files.len() as u64 - new;
    let named = format!(
        "{} data.lock names {} on no mirror",
        units::count(pinned, "file", "files"),
        if pinned == 1 { "is" } else { "are" }
    );
    let unknown = format!(
        "{} {} no SHA-256 yet",
        units::count(new, "new file", "new files"),
        if new == 1 { "has" } else { "have" }
    );
    let error = if new == 0 {
        refused(context, &named)
    } else if pinned == 0 {
        let (it, its) = if new == 1 {
            ("it", "its source")
        } else {
            ("them", "their sources")
        };
        refusal(
            context,
            &format!("{unknown}, so only {its} can give {it}"),
            &format!("pin {it} on a machine that may ask {its}, then push {it} to a mirror"),
        )
    } else {
        refusal(
            context,
            &format!("{named} and {unknown}"),
            "ask whoever keeps the mirrors to add what is missing, and pin new files on a machine that may ask their sources",
        )
    };
    Err(lacked(context, files)
        .into_iter()
        .fold(error, Error::detail))
}

/// a line for each of the first `files` a run lacks, by its name and with what each mirror
/// said of it, when it has mirrors, and how many more there are.
pub(in crate::cli) fn lacked(context: &Context, files: &[(String, Option<Sha256>)]) -> Vec<String> {
    let mut lines: Vec<String> = files
        .iter()
        .take(super::NAMED)
        .map(|(label, digest)| {
            if context.mirrors.mirrors.list.is_empty() {
                label.clone()
            } else {
                format!("{label}: {}", context.mirrors.said(digest.as_ref()))
            }
        })
        .collect();
    if files.len() > super::NAMED {
        lines.push(format!(
            "and {} more",
            units::grouped((files.len() - super::NAMED) as u64)
        ));
    }
    lines
}

/// the pinned files among `jobs` that the cache lacks, as a mirror is asked for them.
pub(in crate::cli) fn pinned(jobs: &[Job], cache: &Cache) -> Vec<Wanted> {
    jobs.iter()
        .filter_map(|job| {
            let digest = job.fetch.expect.sha256?;
            let size = job.fetch.expect.size?;
            cache.find(&digest, Some(size)).is_none().then(|| Wanted {
                digest,
                size,
                label: job.label.clone(),
                link: Some(job.fetch.url.to_string()),
            })
        })
        .collect()
}

/// puts into the cache what the data folder, then the reuse folders, then each mirror in order hold
/// of the files `wanted` lists; a damaged copy on a mirror is told and left for a later one or the
/// source. `wanted` runs only when one may hold something, since listing what the cache lacks
/// reads every file. fails at a stop, a cache write, or `unconfirmed` past what a person agreed to
/// download.
pub(in crate::cli) async fn fill(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    folder: &DataFolder,
    wanted: impl FnOnce() -> Vec<Wanted>,
) -> Result<(), Error> {
    let mirrored = &context.mirrors;
    if mirrored.mirrors.list.is_empty() && folder.is_empty() && context.reuse.is_empty() {
        return Ok(());
    }
    let mut seen = HashSet::new();
    let wanted: Vec<Wanted> = wanted()
        .into_iter()
        .filter(|want| seen.insert(want.digest))
        .collect();
    let wanted = tokio::task::block_in_place(|| folder.take(context, cache, wanted))?;
    let mut wanted = tokio::task::block_in_place(|| context.reuse.take(context, cache, wanted))?;
    for (index, location) in mirrored.mirrors.list.iter().enumerate() {
        let ask: Vec<Wanted> = wanted
            .iter()
            .filter(|want| !mirrored.lacks(index, &want.digest))
            .cloned()
            .collect();
        if ask.is_empty() || !mirrored.usable(context, net, index, location).await? {
            continue;
        }
        let stored = match (location, mirrored.base(index, location)) {
            (Location::Folder(root), _) => {
                from_folder(context, cache, index, location, root, ask).await?
            }
            (Location::Web(_) | Location::S3(_), Some(base)) => {
                from_web(context, net, cache, index, location, &base, ask).await?
            }
            (Location::Web(_) | Location::S3(_), None) => continue,
        };
        wanted.retain(|want| !stored.contains(&want.digest));
        if wanted.is_empty() {
            break;
        }
    }
    Ok(())
}

/// copies `ask` from the folder mirror `index` at `root`, and says which files it stored.
async fn from_folder(
    context: &Context,
    cache: &Cache,
    index: usize,
    location: &Location,
    root: &std::path::Path,
    ask: Vec<Wanted>,
) -> Result<HashSet<Sha256>, Error> {
    let mirrored = &context.mirrors;
    let (asked, gave) = copy_from_folder(context, cache, root, ask).await?;
    let mut copied = (0, 0);
    let mut stored = HashSet::new();
    let mut damaged = Vec::new();
    for (want, gave) in asked.into_iter().zip(gave) {
        match gave {
            Gave::Stored(object) => {
                if object.new {
                    context.work.wrote(&object.digest);
                }
                Work::add(&context.work.mirror_objects, 1);
                Work::add(&context.work.mirror_bytes, object.size);
                copied = (copied.0 + 1, copied.1 + object.size);
                stored.insert(want.digest);
            }
            Gave::Absent => mirrored.lacked(index, want.digest, NOT_THERE.to_owned()),
            Gave::Damaged => {
                mirrored.lacked(index, want.digest, DAMAGED.to_owned());
                damaged.push(want.label);
            }
            Gave::Unreadable(err) => {
                let said = crate::error::said(&err);
                mirrored.lacked(index, want.digest, format!("unreadable: {said}"));
                damaged.push(format!("{} ({said})", want.label));
            }
        }
    }
    tell_damaged(context, location, &damaged);
    if copied.0 > 0 {
        context.output.step(
            "Copied",
            &format!(
                "{} \u{b7} {} from {location}",
                units::count(copied.0, "file", "files"),
                units::size(copied.1)
            ),
        );
    }
    Ok(stored)
}

/// downloads `ask` from the web or S3 mirror `index` at `base` as any download, question
/// included, and says which files it stored. a mirror that refuses or fails is skipped for the rest
/// of the run.
async fn from_web(
    context: &Context,
    net: &Arc<Net>,
    cache: &Cache,
    index: usize,
    location: &Location,
    base: &url::Url,
    ask: Vec<Wanted>,
) -> Result<HashSet<Sha256>, Error> {
    let mirrored = &context.mirrors;
    let total = ask
        .iter()
        .map(|want| want.size)
        .fold(0, u64::saturating_add);
    crate::cli::confirm(
        &context.output,
        context.yes,
        context.settings.ask_above.value,
        &units::count(ask.len() as u64, "file", "files"),
        crate::net::estimate::Size::Bytes(total),
        Some((cache.root(), total)),
    )?;
    let jobs = ask
        .iter()
        .map(|want| Job {
            fetch: web::object(base, want),
            label: want.label.clone(),
            dataset: None,
            named_by_caller: false,
            again: None,
        })
        .collect();
    let started = Instant::now();
    // a mirror is asked only for objects the cache lacks
    let cached = vec![false; ask.len()];
    let results = super::apart(context, net, cache, (jobs, cached), None).await?;
    let elapsed = context
        .output
        .took(Fetched::transfer(results.iter().flatten(), started));
    let mut stored = HashSet::new();
    let mut fetched = Vec::new();
    let mut damaged = Vec::new();
    let mut trouble = None;
    for (want, result) in ask.into_iter().zip(results) {
        let error = match result {
            Ok(found) => {
                Work::add(&context.work.mirror_objects, 1);
                Work::add(&context.work.mirror_bytes, found.size);
                fetched.push((found.size, found.moved));
                stored.insert(want.digest);
                continue;
            }
            Err(error) => error,
        };
        if let Some(said) = lacking(location, &error) {
            mirrored.lacked(index, want.digest, said);
            continue;
        }
        match error.kind {
            Kind::IntegrityMismatch => {
                mirrored.lacked(index, want.digest, DAMAGED.to_owned());
                damaged.push(want.label);
            }
            Kind::LocalIo => return Err(error),
            _ => {
                mirrored.lacked(index, want.digest, error.message.clone());
                trouble.get_or_insert(error);
            }
        }
    }
    tell_damaged(context, location, &damaged);
    if !fetched.is_empty() {
        context.output.step(
            "Downloaded",
            &format!(
                "{} \u{b7} {} from {location}",
                units::count(fetched.len() as u64, "file", "files"),
                units::downloaded(fetched, elapsed)
            ),
        );
    }
    if let Some(error) = trouble {
        let warning = skip_warning(location, &error, || {
            format!(
                "the mirror {location} failed: {}, so this run skips it",
                error.message
            )
        });
        mirrored.skip(
            context,
            index,
            Skip {
                warning: Some(warning),
                said: error.message,
            },
        );
    }
    Ok(stored)
}

/// the archives among `unknown` whose trees no mirror gave. a tree a mirror gives that hashes to
/// data.lock's pin becomes the record of what its archive unpacks to, so its files can come one by
/// one without the archive. fails at a stop or a cache write.
pub(in crate::cli) async fn trees<'f>(
    context: &Context,
    net: &Net,
    cache: &Cache,
    unknown: Vec<(String, Archive<'f>)>,
) -> Result<Vec<(String, Archive<'f>)>, Error> {
    let mirrored = &context.mirrors;
    if mirrored.mirrors.list.is_empty() {
        return Ok(unknown);
    }
    let mut rest = Vec::with_capacity(unknown.len());
    for (dataset, archive) in unknown {
        let found = match archive.file.unpacks_to {
            Some(tree) => tree_from_mirrors(context, net, cache, &dataset, &archive, tree).await?,
            None => false,
        };
        if !found {
            rest.push((dataset, archive));
        }
    }
    Ok(rest)
}

/// what a mirror gave when asked for a tree.
enum Got {
    Bytes(Vec<u8>),
    Absent,
    /// an S3 mirror's refusal, which a reader not allowed to list the bucket gets for a key that
    /// is not there.
    Unlisted,
    Damaged,
    /// the mirror failed, with what it answered, if anything.
    Trouble(Error),
}

/// asks each mirror in order for the tree `tree` of `archive` in `dataset`, recording the first
/// that matches under the archive's key, a split archive's from its parts. says whether one did.
async fn tree_from_mirrors(
    context: &Context,
    net: &Net,
    cache: &Cache,
    dataset: &str,
    archive: &Archive<'_>,
    tree: Sha256,
) -> Result<bool, Error> {
    let file = archive.file;
    let mirrored = &context.mirrors;
    let label = format!("the list of files {dataset}/{} holds", file.path);
    for (index, location) in mirrored.mirrors.list.iter().enumerate() {
        if mirrored.lacks(index, &tree) || !mirrored.usable(context, net, index, location).await? {
            continue;
        }
        let got = match location {
            Location::Folder(root) => {
                let (root, stop) = (root.clone(), context.stop.clone());
                match blocking(move || folder::read_tree(&root, &tree, &stop)).await {
                    Ok(folder::Tree::Bytes(bytes)) => Got::Bytes(bytes),
                    Ok(folder::Tree::Absent) => Got::Absent,
                    Ok(folder::Tree::Damaged) => Got::Damaged,
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(Stop::stopped());
                    }
                    Err(err) => Got::Trouble(Error::new(Kind::LocalIo, crate::error::said(&err))),
                }
            }
            Location::Web(_) | Location::S3(_) => {
                let Some(base) = mirrored.base(index, location) else {
                    continue;
                };
                match web::read_tree(net, &base, &tree).await? {
                    web::Tree::Bytes(bytes) => Got::Bytes(bytes),
                    web::Tree::Absent => Got::Absent,
                    web::Tree::Answered(403) if matches!(location, Location::S3(_)) => {
                        Got::Unlisted
                    }
                    web::Tree::Answered(status) => {
                        let kind = match status {
                            401 | 403 => Kind::AuthRequired,
                            _ => Kind::NetworkStatus,
                        };
                        Got::Trouble(Error::new(kind, format!("answered {status}")))
                    }
                    web::Tree::Failed(error) => Got::Trouble(error),
                }
            }
        };
        let bytes = match got {
            Got::Bytes(bytes) => bytes,
            Got::Absent => {
                mirrored.lacked(index, tree, NOT_THERE.to_owned());
                continue;
            }
            Got::Unlisted => {
                mirrored.lacked(index, tree, UNLISTED.to_owned());
                continue;
            }
            Got::Damaged => Vec::new(),
            Got::Trouble(error) => {
                let failed = || {
                    format!(
                        "the mirror {location} failed: {}, so this run skips it",
                        error.message
                    )
                };
                let warning = match location {
                    Location::Folder(_) => (
                        failed(),
                        "check the mirror, and that this machine can reach it".to_owned(),
                    ),
                    Location::Web(_) | Location::S3(_) => skip_warning(location, &error, failed),
                };
                mirrored.lacked(index, tree, error.message.clone());
                mirrored.skip(
                    context,
                    index,
                    Skip {
                        warning: Some(warning),
                        said: error.message,
                    },
                );
                continue;
            }
        };
        let recorded = !bytes.is_empty()
            && cache
                .record_tree_bytes(&archive.key(), &tree, &bytes)
                .map_err(|err| {
                    Error::io(
                        format_args!("could not record what {dataset}/{} holds", file.path),
                        &err,
                    )
                })?;
        if recorded {
            return Ok(true);
        }
        mirrored.lacked(index, tree, DAMAGED.to_owned());
        tell_damaged(context, location, std::slice::from_ref(&label));
    }
    Ok(false)
}

/// copies `ask` from the folder mirror at `root` on blocking threads, and returns it with what
/// the mirror gave for each.
async fn copy_from_folder(
    context: &Context,
    cache: &Cache,
    root: &std::path::Path,
    ask: Vec<Wanted>,
) -> Result<(Vec<Wanted>, Vec<Gave>), Error> {
    let (cache, root, threads, stop) = (
        cache.clone(),
        root.to_path_buf(),
        context.threads,
        context.stop.clone(),
    );
    let shown = root.display().to_string();
    blocking(move || {
        let gave = folder::copy_all(&cache, &root, &ask, threads, &stop)?;
        Ok((ask, gave))
    })
    .await
    .map_err(|err| {
        if err.kind() == std::io::ErrorKind::Interrupted {
            Stop::stopped()
        } else {
            Error::io(
                format_args!("could not copy from the mirror {shown} into the cache"),
                &err,
            )
        }
    })
}

/// runs `work` on a thread that may wait on the disk, so the network's workers never do.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> std::io::Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|err| Err(std::io::Error::other(err)))
}

/// warns about the damaged copies `location` holds: the first few by name, the rest counted.
fn tell_damaged(context: &Context, location: &Location, labels: &[String]) {
    let help = match location {
        Location::Folder(root) => format!(
            "whoever keeps the mirror replaces it with `fetchloom push --verify --to {}`",
            crate::suggested::shell_word(&root.display().to_string())
        ),
        Location::Web(_) | Location::S3(_) => {
            "ask whoever keeps the mirror to replace it".to_owned()
        }
    };
    for label in labels.iter().take(NAMED) {
        context.output.warn_with(
            Warning::MirrorDamaged,
            &format!("the mirror {location} holds a damaged copy of {label}, so it was not used"),
            &help,
        );
    }
    if labels.len() > NAMED {
        context.output.warn(
            Warning::MirrorDamaged,
            &format!(
                "the mirror {location} holds {} more damaged copies, so they were not used",
                units::grouped((labels.len() - NAMED) as u64)
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::output::{Level, Output, Style};
    use crate::source::recorded::block_on;

    /// a web mirror that refuses says how to sign in to its host; another answer points at whoever
    /// keeps the mirror.
    #[test]
    fn a_refusing_web_mirror_names_its_sign_in() {
        let web = Location::Web(url::Url::parse("https://mirror.lab.example/data/").unwrap());
        for status in [401, 403] {
            assert_eq!(
                refused_help(&web, status, None),
                "if it needs a login, sign in with `fetchloom auth login mirror.lab.example`"
            );
        }
        assert_eq!(
            refused_help(&web, 500, None),
            "ask whoever keeps the mirror, or check its address"
        );
    }

    /// listing what the cache lacks looks at every file, so a run with nowhere to ask never does.
    #[test]
    fn a_run_without_a_mirror_lists_nothing_to_ask_for() {
        let context = Context::new(Output::new(
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
        ));
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let net = Arc::new(context.net());
        let listed = AtomicBool::new(false);
        block_on(fill(&context, &net, &cache, &DataFolder::default(), || {
            listed.store(true, Ordering::Relaxed);
            Vec::new()
        }))
        .unwrap();
        assert!(!listed.load(Ordering::Relaxed));
    }

    /// a mirror whose objects refuse a run says how to sign in to it, by host and port, a server
    /// error points at whoever keeps it, and only a mirror no one reached blames the address.
    #[test]
    fn a_failed_mirror_is_told_by_how_it_failed() {
        let web = Location::Web(url::Url::parse("http://127.0.0.1:34271/m/").unwrap());
        let refused = Error::new(
            Kind::AuthRequired,
            "127.0.0.1 refused access to http://127.0.0.1:34271/m/objects/96/ee (401)",
        );
        let (warning, help) = skip_warning(&web, &refused, || "not this".to_owned());
        assert_eq!(
            warning,
            "the mirror http://127.0.0.1:34271/m refused access: 127.0.0.1 refused access to http://127.0.0.1:34271/m/objects/96/ee (401), so this run skips it"
        );
        assert_eq!(
            help,
            "if it needs a login, sign in with `fetchloom auth login 127.0.0.1:34271`"
        );
        let failed = Error::new(Kind::NetworkStatus, "127.0.0.1 answered 503");
        let (_, help) = skip_warning(&web, &failed, || "failed".to_owned());
        assert_eq!(help, "ask whoever keeps the mirror, or try again later");
        let gone = Error::new(Kind::NetworkUnreachable, "nothing listens there");
        let (_, help) = skip_warning(&web, &gone, || "failed".to_owned());
        assert_eq!(
            help,
            "check the address, and that this machine can reach it"
        );
    }

    /// S3 answers 403, not 404, for a key a mirror lacks when its reader may not list the bucket,
    /// so on an S3 mirror that answered at all, a refusal is a file not there, or not readable,
    /// and the mirror is still asked for the rest.
    #[test]
    fn an_s3_mirror_s_refusal_of_one_file_is_a_file_it_lacks() {
        let bucket = Location::parse("s3://lab-data/fetchloom", None).unwrap();
        let web = Location::Web(url::Url::parse("https://mirror.lab.example/").unwrap());
        let refused = Error::new(Kind::AuthRequired, "refused without a key");
        assert_eq!(
            lacking(&bucket, &refused),
            Some("not there, or not readable without s3:ListBucket".to_owned())
        );
        assert_eq!(lacking(&web, &refused), None);
        let gone = Error::new(Kind::ReferenceUnresolved, "404");
        assert_eq!(lacking(&web, &gone), Some("not there".to_owned()));
    }

    /// a mirror skipped once it answered, as when its objects fail, is asked nothing more that
    /// run: no later phase tries it again first, with its retries, before the next mirror.
    #[test]
    fn a_mirror_skipped_after_it_answered_is_asked_nothing_more() {
        let context = Context::new(Output::new(
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
        ));
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(
            folder.path().join(crate::mirror::MARKER),
            "fetchloom mirror 1\n",
        )
        .unwrap();
        let lab = Location::Folder(folder.path().to_path_buf());
        let mirrored = Mirrored::new(Mirrors::new(Vec::new(), std::slice::from_ref(&lab), false));
        let net = context.net();
        assert!(block_on(mirrored.usable(&context, &net, 0, &lab)).unwrap());
        let said = "failed: answered 500".to_owned();
        let skip = Skip {
            warning: None,
            said: said.clone(),
        };
        mirrored.skip(&context, 0, skip);
        assert!(!block_on(mirrored.usable(&context, &net, 0, &lab)).unwrap());
        assert_eq!(
            mirrored.said(Some(&crate::digest::sha256(b"x"))),
            format!("{said} on {lab}")
        );
    }

    /// pinned files no mirror had and unhashed new ones are refused together, counted apart.
    #[test]
    fn mirror_only_mode_tells_pinned_files_from_new_ones() {
        let mut context = Context::new(Output::new(
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
        ));
        let lab = Location::Folder(std::path::PathBuf::from("/mnt/lab"));
        context.mirrors = Mirrored::new(Mirrors::new(Vec::new(), &[lab], true));
        let pinned = (
            "iris/iris.csv".to_owned(),
            Some(crate::digest::sha256(b"iris")),
        );
        let new = ("iris2/iris.csv".to_owned(), None);
        let error = sources_allowed(&context, &[pinned, new]).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkRefused);
        assert_eq!(
            error.message,
            "1 file data.lock names is on no mirror and 1 new file has no SHA-256 yet, and mirrors_only lets fetchloom ask its mirrors alone"
        );
        assert_eq!(
            error.details,
            [
                "iris/iris.csv: not asked on /mnt/lab",
                "iris2/iris.csv: new, so no mirror can be asked for it"
            ]
        );
        // mirrors_only set in the settings file is turned off with the command that edits it
        context.settings.mirrors_only.origin = Origin::File;
        let error = refused(&context, "1 file is on no mirror");
        assert_eq!(
            error.help.as_deref(),
            Some(
                "ask whoever keeps the mirrors to add what is missing, or run `fetchloom settings unset mirrors_only` to ask sources too"
            )
        );
    }
}
