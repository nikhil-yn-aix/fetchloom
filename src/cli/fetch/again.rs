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

type Asked = Result<Arc<Listing>, Error>;

/// the listings asked for again in one run, so a dataset is resolved once however many of its
/// links died.
#[derive(Clone, Default)]
pub(super) struct Listings(Arc<Mutex<HashMap<String, Asked>>>);

impl Listings {
    async fn get(&self, net: &impl net::Api, resolved: &str) -> Asked {
        let mut known = self.0.lock().await;
        if let Some(listing) = known.get(resolved) {
            return listing.clone();
        }
        let listing = match Reference::parse(resolved) {
            Ok(parsed) => parsed.reference.resolve(net).await.map(Arc::new),
            Err(err) => Err(err),
        };
        known.insert(resolved.to_owned(), listing.clone());
        listing
    }
}

/// downloads `job`'s file from where its source lists it now, when `failed` says the recorded
/// link is gone. lookup failures keep their own recovery advice; interruption stops the batch.
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
        Err(error) if error.kind == Kind::Interrupted => return Err(error),
        Err(error) => {
            return Err(error.detail(format!(
                "asking {} again failed after: {}",
                again.resolved, failed.message
            )));
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

    let fetch = Fetch {
        expect: job.fetch.expect,
        revalidate: false,
        unpack: false,
        ..source::fetch_plan(&remote.url)
    };
    let fetched = net.download(&fetch, cache, on).await.map_err(|err| {
        if err.kind == Kind::Interrupted {
            return err;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::{Level, Style};
    use crate::source::recorded::block_on;

    struct Failing(Error);

    impl net::Api for Failing {
        async fn get(&self, _url: &url::Url) -> Result<net::Response, Error> {
            Err(self.0.clone())
        }
    }

    fn context() -> crate::cli::Context {
        let mut context = crate::cli::Context::new(Output::new(
            Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        context.offline = net::Offline::Flag;
        context
    }

    #[test]
    fn refresh_failures_keep_their_kind_help_and_details() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path().join("cache")).unwrap();
        let context = context();
        let net = context.net();
        let resolved = "zenodo:3608990";
        let job = Job {
            fetch: Fetch::new(url::Url::parse("https://example.org/gone.csv").unwrap()),
            label: "iris/data.csv".to_owned(),
            dataset: Some("iris".to_owned()),
            again: Some(Again {
                resolved: resolved.to_owned(),
                path: "data.csv".to_owned(),
                alone: true,
            }),
        };
        for kind in [Kind::AuthRequired, Kind::SourceInvalid, Kind::Interrupted] {
            let source = Failing(
                Error::new(kind, "listing failed")
                    .detail("source-specific detail")
                    .help("source-specific recovery"),
            );
            block_on(async {
                let listings = Listings::default();
                assert!(listings.get(&source, resolved).await.is_err());
                for _ in 0..2 {
                    let error = recover(
                        &net,
                        &cache,
                        &context.output,
                        &listings,
                        &job,
                        Error::new(Kind::ReferenceUnresolved, "recorded URL returned 404"),
                        &|_| Ok(()),
                    )
                    .await
                    .unwrap_err();
                    assert_eq!(error.kind, kind);
                    assert_eq!(error.message, "listing failed");
                    assert_eq!(error.help.as_deref(), Some("source-specific recovery"));
                    assert!(
                        error.details.iter().any(|line| line == "source-specific detail")
                    );
                    if kind == Kind::Interrupted {
                        assert_eq!(error.kind.exit_code(), 130);
                        assert_eq!(error.details, ["source-specific detail"]);
                    }
                }
            });
        }
    }
}
