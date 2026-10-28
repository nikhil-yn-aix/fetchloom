//! a pinned file whose recorded link is dead is looked up again through its source, since a
//! dataset id or a DOI usually outlives the links it once listed. the pinned hash still decides
//! whether the bytes found there are the right ones.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::Job;
use crate::cache::Cache;
use crate::error::{Error, Kind};
use crate::net::{self, Event, Fetch, Fetched, Net};
use crate::output::Output;
use crate::source::{self, Listing, Reference};

/// where to look for a locked file again: the version its dataset is pinned to, and the file's
/// path in that version's listing.
pub(in crate::cli) struct Again {
    pub(in crate::cli) resolved: String,
    pub(in crate::cli) path: String,
    /// the dataset is this one file, so a listing of one file holds it whatever its name.
    pub(in crate::cli) alone: bool,
}

/// a listing asked for again, or why asking failed.
type Asked = Result<Arc<Listing>, String>;

/// the listings asked for again in one run, so a dataset is resolved once however many of its
/// links died.
#[derive(Clone, Default)]
pub(super) struct Listings(Arc<Mutex<HashMap<String, Asked>>>);

impl Listings {
    async fn get(&self, net: &Net, resolved: &str) -> Asked {
        let mut known = self.0.lock().await;
        if let Some(listing) = known.get(resolved) {
            return listing.clone();
        }
        let listing = match Reference::parse(resolved) {
            Ok(parsed) => parsed
                .reference
                .resolve(net)
                .await
                .map(Arc::new)
                .map_err(|err| err.message),
            Err(err) => Err(err.message),
        };
        known.insert(resolved.to_owned(), listing.clone());
        listing
    }
}

/// downloads `job`'s file from where its source lists it now, when `failed` says the recorded
/// link is gone. any other failure, or one the source cannot help with, comes back as it was,
/// with a line saying what asking again found.
pub(super) async fn recover(
    net: &Net,
    cache: &Cache,
    output: &Output,
    listings: &Listings,
    job: &Job,
    failed: Error,
    on: &(dyn Fn(Event) -> Result<(), Error> + Sync),
) -> Result<Fetched, Error> {
    let Some(again) = job.again.as_ref() else {
        return Err(failed);
    };
    if failed.kind != Kind::ReferenceUnresolved {
        return Err(failed);
    }
    let listing = match listings.get(net, &again.resolved).await {
        Ok(listing) => listing,
        Err(reason) => {
            return Err(failed.detail(format!("asking {} again failed: {reason}", again.resolved)));
        }
    };
    let found = listing
        .files
        .iter()
        .find(|remote| remote.path == again.path)
        .or(match listing.files.as_slice() {
            [only] if again.alone => Some(only),
            _ => None,
        });
    let Some(remote) = found else {
        return Err(failed.detail(format!("{} no longer lists it", again.resolved)));
    };
    if remote.url == job.fetch.url {
        // a plain link is its own source, so asking it again says nothing new
        if matches!(listing.resolved, Reference::Url(_)) {
            return Err(failed);
        }
        return Err(failed.detail(format!("{} still lists the same link", again.resolved)));
    }
    let (from, divert) = source::fetch_plan(&remote.url);
    let fetch = Fetch {
        url: remote.url.clone(),
        from,
        divert,
        expect: job.fetch.expect,
        revalidate: false,
        unpack: false,
    };
    let fetched = net.download(&fetch, cache, on).await.map_err(|err| {
        err.detail(format!(
            "its recorded link is gone, so this came from where {} lists it now",
            again.resolved
        ))
    })?;
    output.warn(&format!(
        "{} is gone from its recorded link. {} still lists it, so it came from {}",
        job.label,
        again.resolved,
        net::host(&remote.url)
    ));
    Ok(fetched)
}
