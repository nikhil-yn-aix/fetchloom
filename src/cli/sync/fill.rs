//! datasets `add --no-sync` pinned by what their sources state, as relocking meets them. a sync
//! downloads each one it covers, checks every stated digest and size, and pins it whole as if its
//! source listed it again; every other run carries it as it is. one data.toml changed or no longer
//! has, or an upgrade asks for, goes the way of any changed pin.

use std::collections::HashSet;

use url::Url;

use super::draft::{self, Unit};
use super::{Given, Loop, Upgrade};
use crate::cli::fetch::{Again, Job};
use crate::error::{Error, Kind};
use crate::net::{Expect, Fetch};
use crate::project::lock::{Lock, Locked, Stated, Waiting};
use crate::project::manifest::Manifest;
use crate::source::{self, Listing, Reference, Remote};

/// the entries relocking leaves as they are, which go back into the lock it makes.
#[derive(Default)]
pub(super) struct Carried {
    stated: Vec<Stated>,
    /// the datasets waiting for their file lists, which relocking never resolves.
    waiting: Vec<Waiting>,
    /// those waiting whose pins data.toml no longer asks for, which go.
    gone: Vec<Waiting>,
    /// when each dataset this run pins whole was pinned by its source's digests, by name: the data
    /// stays the same, so its date does too.
    dates: Vec<(String, String)>,
}

impl Carried {
    /// puts the carried entries into `lock`, and each filled pin's date back, giving the entries
    /// waiting for their lists that go.
    pub(super) fn back(self, lock: &mut Lock) -> Vec<Waiting> {
        for (name, date) in self.dates {
            if let Ok(index) = lock
                .datasets
                .binary_search_by(|locked| locked.name.as_str().cmp(&name))
            {
                lock.datasets[index].timestamp = date;
            }
        }
        lock.stated = self.stated;
        lock.waiting = self.waiting;
        self.gone
    }
}

impl Loop<'_> {
    /// readies `old` for relocking. a dataset its source's digests pin that this run fills becomes
    /// a listing in `given`, its files first downloaded and checked when `download`, else as its
    /// source states them, for a dry run. one data.toml changed or no longer has, or an upgrade
    /// asks for, is relocked as any changed pin is. the rest are carried as they are.
    pub(super) async fn stated(
        &self,
        manifest: &Manifest,
        old: Option<Lock>,
        upgrade: &Upgrade,
        mut given: Vec<Given>,
        download: bool,
    ) -> Result<(Option<Lock>, Vec<Given>, Carried), Error> {
        let mut carried = Carried::default();
        let Some(mut old) = old else {
            return Ok((None, given, carried));
        };
        // a dataset waiting for its list keeps its entry while data.toml still asks for that pin,
        // as one outside the run's scope does; otherwise the entry goes, as a changed pin goes,
        // and its name is relocked as one data.lock does not pin
        let (kept, gone): (Vec<Waiting>, Vec<Waiting>) = std::mem::take(&mut old.waiting)
            .into_iter()
            .partition(|waiting| {
                !self.scope.includes(&waiting.name)
                    || waiting.answers(manifest.datasets.get(&waiting.name))
            });
        old.waiting.clone_from(&kept);
        carried.waiting = kept;
        carried.gone = gone;
        let named: HashSet<String> = given
            .iter()
            .map(|given| match given {
                Given::Listing(name, _) => name.clone(),
                Given::Locked(locked) => locked.name.clone(),
            })
            .collect();
        let mut filling = Vec::new();
        for stated in std::mem::take(&mut old.stated) {
            let name = stated.name().to_owned();
            if !self.scope.includes(&name) {
                carried.stated.push(stated);
                continue;
            }
            let current = manifest
                .datasets
                .get(&name)
                .filter(|entry| stated.pin.drift(entry).is_none());
            let changed = current.is_none() || upgrade.includes(&name) || named.contains(&name);
            if !changed && !self.fills {
                carried.stated.push(stated);
                continue;
            }
            // a pin with no files and a spec no entry has, so relocking replaces or drops it
            // whole and tells its change as of any pin
            old.datasets.push(Locked {
                spec: String::new(),
                ..stated.pin.clone()
            });
            if !changed {
                filling.push(stated);
            }
        }
        old.datasets.sort_by(|a, b| a.name.cmp(&b.name));
        old.stated.clone_from(&carried.stated);
        if !filling.is_empty() {
            let mut listings = filling
                .iter()
                .map(listing)
                .collect::<Result<Vec<_>, Error>>()?;
            if download {
                self.fill(manifest, &filling, &mut listings).await?;
            }
            for (stated, listing) in filling.into_iter().zip(listings) {
                let name = stated.name().to_owned();
                carried.dates.push((name.clone(), stated.pin.timestamp));
                given.push(Given::Listing(name, listing));
            }
        }
        Ok((Some(old), given, carried))
    }

    /// downloads every file of `filling`, each checked against the size and digests its source
    /// states, and gives each file of `listings` the SHA-256 its bytes have.
    async fn fill(
        &self,
        manifest: &Manifest,
        filling: &[Stated],
        listings: &mut [Listing],
    ) -> Result<(), Error> {
        let mut jobs = Vec::new();
        for (stated, listing) in filling.iter().zip(listings.iter()) {
            let extract = manifest
                .datasets
                .get(stated.name())
                .is_some_and(|entry| entry.source.extract);
            jobs.extend(self::jobs(stated, listing, extract));
        }
        let fetched = self.download(jobs).await?;
        let files = listings.iter_mut().flat_map(|listing| &mut listing.files);
        for (remote, fetched) in files.zip(&fetched) {
            remote.sha256 = Some(fetched.digest);
        }
        Ok(())
    }
}

/// what the source of `stated` listed, as data.lock keeps it: the version, its facts and each
/// file with the size and digests it states.
fn listing(stated: &Stated) -> Result<Listing, Error> {
    let name = stated.name();
    let resolved = Reference::parse(&stated.pin.resolved)
        .map_err(|error| broken(name, &error.message))?
        .reference;
    let files = stated
        .files
        .iter()
        .map(|file| {
            let url = Url::parse(&file.url)
                .map_err(|err| broken(name, &format!("the link of {} is {err}", file.path)))?;
            Ok(Remote {
                path: file.path.clone(),
                size: Some(file.size),
                sha256: file.sha256,
                md5: file.md5,
                url,
                named_by_server: false,
                etag: None,
                blob: None,
            })
        })
        .collect::<Result<_, Error>>()?;
    Ok(Listing {
        resolved,
        title: stated.written.title.clone(),
        license: stated.written.license.clone(),
        doi: stated.written.doi.clone(),
        access: stated.pin.access,
        files,
        unavailable: Vec::new(),
        warnings: Vec::new(),
        within: Vec::new(),
    })
}

/// the downloads of the files of `stated`, as `listing` gives them, each expected at the size and
/// digests its source states; an archive that unpacks says so, as relocking's own downloads do.
fn jobs(stated: &Stated, listing: &Listing, extract: bool) -> Vec<Job> {
    let name = stated.name();
    let found = draft::archives(listing.files.iter().map(|file| file.path.as_str()), extract);
    let whole: HashSet<usize> = found
        .unpack
        .iter()
        .filter_map(|unit| match unit {
            Unit::Whole(index) => Some(*index),
            Unit::Split(_) => None,
        })
        .collect();
    listing
        .files
        .iter()
        .enumerate()
        .map(|(index, remote)| Job {
            fetch: Fetch {
                expect: Expect {
                    sha256: remote.sha256,
                    md5: remote.md5,
                    size: remote.size,
                },
                revalidate: false,
                unpack: whole.contains(&index),
                ..source::fetch_plan(&remote.url)
            },
            label: format!("{name}/{}", remote.path),
            dataset: Some(name.to_owned()),
            again: Some(Again {
                resolved: stated.pin.resolved.clone(),
                path: remote.path.clone(),
                alone: listing.files.len() == 1,
                changed: stated.pin.timestamp.clone(),
            }),
        })
        .collect()
}

/// the error for the pin of `name`, not downloaded yet, that data.lock gives so it cannot be used.
fn broken(name: &str, why: &str) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!(
            "data.lock gives {name}, which is not downloaded yet, a pin fetchloom cannot use: {why}"
        ),
    )
    .help(format!(
        "restore data.lock from git, or run `{}`, which resolves it again and may pin newer data",
        crate::cli::project::command(&format!("lock --upgrade {name}"))
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// the listing a sync fills a pin from carries its title as data.lock writes it, not the
    /// escaped text a reader shows, so the whole pin keeps the bytes `add` would write.
    #[test]
    fn a_fill_lists_the_words_as_data_lock_writes_them() {
        let text = "version = 1\n\n[[dataset]]\nname = \"dv\"\nref = \"zenodo:1\"\nspec = \"sha256:22\"\nresolved = \"zenodo:1\"\ntitle = \"QA \\u001B]0;x\\u0007 set\"\nlocked = \"2026-11-15T07:00:00Z\"\ndownloaded = false\nfiles = [\n  { path = \"a.csv\", size = 1, md5 = \"0cc175b9c0f1b6a831c399e269772661\", url = \"https://zenodo.org/api/records/1/files/a.csv/content\" },\n]\n";
        let lock = Lock::parse(text).unwrap();
        let stated = &lock.stated[0];
        assert_eq!(
            stated.pin.title.as_deref(),
            Some("QA \\u{1b}]0;x\\u{7} set")
        );
        assert_eq!(
            listing(stated).unwrap().title.as_deref(),
            Some("QA \u{1b}]0;x\u{7} set")
        );
    }
}
