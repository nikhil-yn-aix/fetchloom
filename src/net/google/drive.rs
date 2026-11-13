//! which requests to drive's api carry a google access token: those about an item `drive_folders`
//! lists, or that google's own listing put inside one this run. a drive token reads every file its
//! account can see, so nothing data.toml or data.lock says decides what gets one.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use reqwest::header::HeaderMap;
use serde::Deserialize;
use url::Url;

use super::{Bearer, Net, insufficient_scope};
use crate::auth::google::{Credentials, DRIVE_READ};
use crate::auth::lease::Lease;
use crate::cache::{Cache, UrlRecord};
use crate::error::{Error, Kind};
use crate::net::download::{Fetch, Fetched, Listener};
use crate::net::failure::{self, Failure};
use crate::source::DriveId;
use crate::source::drive::{self as source, GUIDE, LOGIN, is_id};

/// what [`Net::drive_refused`] answers, in time.
pub(crate) type Refused<'a> =
    std::pin::Pin<Box<dyn Future<Output = Result<Private, Error>> + Send + 'a>>;

/// the host of google's apis, drive's among them.
pub(super) const API: &str = "www.googleapis.com";

/// what a request to drive's api is about: the file or folder it reads, or the folder whose
/// items it lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asked {
    id: String,
    listing: bool,
}

impl Asked {
    pub(super) fn id(&self) -> &str {
        &self.id
    }
}

/// whether drive's api may be asked about an item with a token, once drive refused it without
/// one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Private {
    /// `drive_folders` lists neither the item nor a folder google found it in.
    Unlisted,
    /// `drive_folders` lists items, but no google application default credentials were found.
    Unsigned,
    /// with a token of the credentials found.
    Ready,
}

/// the drive items listed and what one run learns of them.
#[derive(Default)]
pub(crate) struct Drive {
    listed: Vec<DriveId>,
    seen: Mutex<Seen>,
    /// the listed folders searched this run for an item drive refused, each with why its
    /// search failed, if it did.
    searched: tokio::sync::Mutex<Vec<(String, Option<Error>)>>,
    pub(super) token: Lease<Bearer>,
}

#[derive(Default)]
struct Seen {
    /// the items drive refused without a token this run.
    refused: HashSet<String>,
    /// the items google's listings put inside a listed folder this run: each with that folder,
    /// which drive refused, and its name.
    inside: HashMap<String, Inside>,
}

struct Inside {
    root: String,
    name: String,
}

impl Seen {
    /// the listed item, refused this run, that lets a request about `id` carry the token: `id`
    /// itself, or the folder google found it in.
    fn carrier(&self, listed: &[DriveId], id: &str) -> Option<String> {
        if listed.iter().any(|item| item.as_str() == id) && self.refused.contains(id) {
            return Some(id.to_owned());
        }
        self.inside
            .get(id)
            .filter(|inside| self.refused.contains(&inside.root))
            .map(|inside| inside.root.clone())
    }
}

impl Drive {
    pub(crate) fn new(listed: Vec<DriveId>) -> Drive {
        Drive {
            listed,
            ..Drive::default()
        }
    }

    fn seen(&self) -> MutexGuard<'_, Seen> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// whether `id` is listed, or google found it inside a listed folder this run.
    fn covers(&self, id: &str) -> bool {
        self.listed.iter().any(|item| item.as_str() == id) || self.seen().inside.contains_key(id)
    }

    /// whether a request about `id` may carry the token.
    pub(super) fn may_carry(&self, id: &str) -> bool {
        self.seen().carrier(&self.listed, id).is_some()
    }

    /// learns from a listing of the items in `parent`, asked with the token, which items the
    /// listed folder behind it holds.
    fn heard(&self, parent: &str, body: &[u8]) {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            files: Vec<Item>,
        }
        #[derive(Deserialize)]
        struct Item {
            id: String,
            #[serde(default)]
            name: String,
        }
        let Ok(page) = serde_json::from_slice::<Page>(body) else {
            return;
        };
        let mut seen = self.seen();
        let Some(root) = seen.carrier(&self.listed, parent) else {
            return;
        };
        for item in page.files.into_iter().filter(|item| is_id(&item.id)) {
            seen.inside.entry(item.id).or_insert_with(|| Inside {
                root: root.clone(),
                name: item.name,
            });
        }
    }
}

/// what a request to drive's api is about, when it is one a token may go with: over https to
/// www.googleapis.com, reading one item by `files.get`, or listing a folder's items by
/// `files.list` with the one query fetchloom asks, and no other parameter that could widen it.
pub(crate) fn locate(url: &Url) -> Option<Asked> {
    if url.scheme() != "https" || url.host_str() != Some(API) || url.port().is_some() {
        return None;
    }
    let mut segments = url.path_segments()?;
    if (segments.next()?, segments.next()?, segments.next()?) != ("drive", "v3", "files") {
        return None;
    }
    let id = segments.next();
    if segments.next().is_some() {
        return None;
    }
    let mut query = None;
    for (name, value) in url.query_pairs() {
        match (&*name, id) {
            ("q", None) if query.is_none() => query = Some(value.into_owned()),
            ("fields" | "supportsAllDrives", _)
            | ("pageSize" | "pageToken" | "includeItemsFromAllDrives", None) => {}
            ("alt", Some(_)) if value == "media" => {}
            _ => return None,
        }
    }
    if let Some(id) = id {
        return is_id(id).then(|| Asked {
            id: id.to_owned(),
            listing: false,
        });
    }
    let parent = query?
        .strip_prefix('\'')?
        .strip_suffix("' in parents and trashed = false")?
        .to_owned();
    is_id(&parent).then_some(Asked {
        id: parent,
        listing: true,
    })
}

/// the link drive's api sends a file's bytes from.
pub(crate) fn media(id: &str) -> Result<Url, Error> {
    let mut url = Url::parse(&format!("https://{API}/drive/v3/files/{id}")).map_err(|err| {
        Error::new(
            Kind::SourceInvalid,
            format!("could not make a drive link: {err}"),
        )
    })?;
    url.query_pairs_mut()
        .append_pair("alt", "media")
        .append_pair("supportsAllDrives", "true");
    Ok(url)
}

/// how a request to drive's api went out, kept on its answer so a refusal can say why and a
/// listing can teach what a folder holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Answered {
    asked: Asked,
    /// who the token was of, as messages name them, or none when it went without one.
    identity: Option<String>,
    /// the service account's email, which an owner shares with.
    account: Option<String>,
    /// whether drive's refusal says the token lacks the scope that reads drive.
    lacks_scope: bool,
}

impl Answered {
    pub(in crate::net) fn bytes(&self) -> usize {
        self.asked.id.len()
            + self.identity.as_ref().map_or(0, String::len)
            + self.account.as_ref().map_or(0, String::len)
    }

    pub(super) fn new(asked: &Asked, bearer: Option<&Bearer>, headers: &HeaderMap) -> Answered {
        Answered {
            asked: asked.clone(),
            identity: bearer.map(|bearer| bearer.identity.to_string()),
            account: bearer.and_then(|bearer| bearer.account.as_deref().map(str::to_owned)),
            lacks_scope: insufficient_scope(headers),
        }
    }

    /// how a request about `id` went out with the token of a service account of `account`.
    #[cfg(test)]
    pub(crate) fn service_account(id: &str, account: &str) -> Answered {
        Answered {
            asked: Asked {
                id: id.to_owned(),
                listing: false,
            },
            identity: Some(format!("the service account {account}")),
            account: Some(account.to_owned()),
            lacks_scope: false,
        }
    }

    /// the item as a browser opens it, file or folder.
    fn link(&self) -> String {
        format!("https://drive.google.com/open?id={}", self.asked.id)
    }

    /// the failure for an answer of `status` and `body` that is not the item: one another try
    /// may get past for a rate limit, else [`Answered::error`].
    pub(in crate::net) fn failure(&self, status: u16, body: &[u8]) -> Failure {
        if Said::read(body).limited() {
            return failure::transient(format!(
                "google drive asked to slow down for {}",
                self.link()
            ));
        }
        Failure::Final(self.error(status, body))
    }

    /// the error for an answer of `status` and `body` that is not the item, saying which
    /// account asked and what would let it read the item.
    pub(crate) fn error(&self, status: u16, body: &[u8]) -> Error {
        let said = Said::read(body);
        let link = self.link();
        let Some(identity) = &self.identity else {
            return Error::new(
                Kind::AuthRequired,
                format!("google drive's api refused {link} without a token"),
            )
            .detail(said.shown(status));
        };
        let reason = said.reason.as_deref().unwrap_or_default();
        if said.limited() {
            return Error::new(
                Kind::NetworkRateLimited,
                format!("google drive asked to slow down for {link}"),
            )
            .detail(said.shown(status))
            .help("run the same command again in a few minutes");
        }
        let unscoped = self.lacks_scope
            || matches!(
                reason,
                "insufficientPermissions" | "ACCESS_TOKEN_SCOPE_INSUFFICIENT"
            )
            || said.message.as_deref().is_some_and(|message| {
                message
                    .to_ascii_lowercase()
                    .contains("insufficient authentication scopes")
            });
        match status {
            403 if unscoped => unscoped_error(identity, &link),
            403 if matches!(reason, "accessNotConfigured" | "SERVICE_DISABLED") => {
                turned_off(identity).detail(said.shown(status))
            }
            401 => Error::new(
                Kind::AuthRequired,
                format!("google drive refused the token of {identity} for {link}"),
            )
            .detail(said.shown(status))
            .help(format!(
                "sign in again: {LOGIN}, or use another service account key. how: {GUIDE}"
            )),
            403 | 404 => self.unshared(identity, &link),
            status => Error::new(
                Kind::NetworkStatus,
                format!("google drive answered {status} for {link}"),
            )
            .detail(said.shown(status)),
        }
    }

    /// the error for an item drive says `identity` cannot read, as it says of one missing too.
    fn unshared(&self, identity: &str, link: &str) -> Error {
        let error = Error::new(
            Kind::AuthRequired,
            format!("google drive has no file or folder {link} that {identity} can read"),
        );
        match &self.account {
            Some(email) => error.help(format!(
                "share it with {email} as a viewer, then run the same command again"
            )),
            None => error.help(
                "ask its owner to share it with the google account you signed in with, or sign in with one it is shared with",
            ),
        }
    }
}

/// the error for a token of `identity` without a scope that reads drive.
fn unscoped_error(identity: &str, link: &str) -> Error {
    Error::new(
        Kind::AuthRequired,
        format!(
            "google drive refused {identity} for {link}: its token lacks the drive.readonly scope"
        ),
    )
    .help(format!(
        "sign in again with an oauth client of your own, since google keeps gcloud's own from drive: {LOGIN}. how: {GUIDE}"
    ))
}

/// the error for drive's api turned off in the project of `identity`.
fn turned_off(identity: &str) -> Error {
    Error::new(
        Kind::AuthRequired,
        format!("google drive's api is turned off in the google cloud project of {identity}"),
    )
    .help("turn on the google drive api for that project at https://console.cloud.google.com/apis/library/drive.googleapis.com, wait a minute, then run the same command again")
}

/// google's `refused` answer to `credentials` asking for `scope`: a drive login signs in again
/// through an oauth client of one's own, since gcloud's own client gets no drive scope.
pub(super) fn relogin(refused: Error, credentials: &Credentials, scope: &str) -> Error {
    if scope != DRIVE_READ
        || refused.kind != Kind::AuthRequired
        || credentials.described().1.is_some()
    {
        return refused;
    }
    refused.help(format!(
        "sign in again with an oauth client of your own, as drive needs: {LOGIN}. how: {GUIDE}"
    ))
}

/// what a google json error says: its first reason, and its message.
#[derive(Default)]
struct Said {
    reason: Option<String>,
    message: Option<String>,
}

impl Said {
    fn read(body: &[u8]) -> Said {
        #[derive(Deserialize)]
        struct Wrapper {
            error: Inner,
        }
        #[derive(Deserialize)]
        struct Inner {
            message: Option<String>,
            #[serde(default)]
            errors: Vec<Reason>,
            #[serde(default)]
            details: Vec<Reason>,
        }
        #[derive(Deserialize)]
        struct Reason {
            reason: Option<String>,
        }
        let Ok(wrapper) = serde_json::from_slice::<Wrapper>(body) else {
            return Said::default();
        };
        let error = wrapper.error;
        Said {
            reason: error
                .errors
                .iter()
                .chain(&error.details)
                .find_map(|reason| reason.reason.clone()),
            message: error.message,
        }
    }

    /// whether google asks to slow down, which a later try may get past.
    fn limited(&self) -> bool {
        matches!(
            self.reason.as_deref(),
            Some("rateLimitExceeded" | "userRateLimitExceeded")
        )
    }

    /// what google said, as a detail line, its words escaped and cut short.
    fn shown(&self, status: u16) -> String {
        match &self.message {
            Some(message) => format!("google said: {}", crate::auth::google::plain(message)),
            None => format!("google drive's api answered {status}"),
        }
    }
}

impl Net {
    /// learns that drive refused `id` without a token, and says whether drive's api may be asked
    /// about it with one: when `drive_folders` lists it, or a listed folder drive also refused
    /// holds it, as listing each such folder with the token, at most once a run, tells.
    ///
    /// # Errors
    ///
    /// what reading the credentials found wrong, a stop, or why a listed folder could not be
    /// searched, when none was found to hold the item.
    pub(crate) fn drive_refused<'a>(&'a self, id: &'a str) -> Refused<'a> {
        // boxed and named, since searching a listed folder asks this again for the folder
        Box::pin(self.private(id))
    }

    async fn private(&self, id: &str) -> Result<Private, Error> {
        let drive = &self.google.drive;
        if drive.listed.is_empty() {
            return Ok(Private::Unlisted);
        }
        drive.seen().refused.insert(id.to_owned());
        if self.google.credentials()?.is_none() {
            return Ok(Private::Unsigned);
        }
        if !drive.covers(id) {
            let mut searched = drive.searched.lock().await;
            for root in &drive.listed {
                if drive.covers(id) {
                    break;
                }
                if searched.iter().any(|(done, _)| done == root.as_str()) {
                    continue;
                }
                let found = source::search(self, root).await;
                if let Err(error) = &found
                    && error.kind == Kind::Interrupted
                {
                    return Err(error.clone());
                }
                searched.push((root.as_str().to_owned(), found.err()));
            }
            if !drive.covers(id)
                && let Some(error) = searched.iter().find_map(|(_, error)| error.clone())
            {
                return Err(error);
            }
        }
        Ok(if drive.may_carry(id) {
            Private::Ready
        } else {
            Private::Unlisted
        })
    }

    /// downloads a drive file known by its `uc` link from drive's download host, and once that is
    /// refused, from drive's api with a token, when `drive_folders` covers it. a file whose listed
    /// folder drive refused this run goes to drive's api from the start.
    pub(in crate::net) async fn drive_download(
        &self,
        fetch: &Fetch,
        cache: &Cache,
        known: Option<UrlRecord>,
        on: Listener<'_>,
    ) -> Result<Fetched, Error> {
        let Some(id) = source::file_id(&fetch.url).filter(|_| !self.google.drive.listed.is_empty())
        else {
            return self.claim_and_download(fetch, cache, known, on).await;
        };
        if self.google.drive.may_carry(&id) {
            return self.private_download(fetch, &id, cache, on).await;
        }
        let refused = match self.claim_and_download(fetch, cache, known, on).await {
            Err(error) if error.kind == Kind::AuthRequired => error,
            done => return done,
        };
        match self.drive_refused(&id).await? {
            Private::Ready => self.private_download(fetch, &id, cache, on).await,
            Private::Unsigned => Err(source::unsigned(refused)),
            Private::Unlisted => Err(refused),
        }
    }

    /// downloads drive file `id` from drive's api, named as google's listing or its own entry
    /// names it.
    async fn private_download(
        &self,
        fetch: &Fetch,
        id: &str,
        cache: &Cache,
        on: Listener<'_>,
    ) -> Result<Fetched, Error> {
        let heard = self
            .google
            .drive
            .seen()
            .inside
            .get(id)
            .map(|inside| inside.name.clone());
        let name = match heard {
            Some(name) => name,
            None => Box::pin(source::file(self, id)).await?.name,
        };
        let private = Fetch {
            url: fetch.url.clone(),
            from: media(id)?,
            divert: None,
            expired: None,
            expect: fetch.expect,
            revalidate: false,
            held: None,
            unpack: fetch.unpack,
            mirror: fetch.mirror,
            page: fetch.page,
        };
        let mut fetched = self.claim_and_download(&private, cache, None, on).await?;
        if fetched.name.is_none() && !name.is_empty() {
            fetched.name = Some(name);
        }
        Ok(fetched)
    }

    /// learns from an answer of drive's api, asked as `answered` says: what a listing, asked
    /// with the token, says its folder holds. a rate limit is a failure another try may get past.
    pub(in crate::net) fn drive_heard(
        &self,
        answered: &Answered,
        status: u16,
        body: &[u8],
    ) -> Result<(), Failure> {
        if status == 200 && answered.asked.listing && answered.identity.is_some() {
            self.google.drive.heard(&answered.asked.id, body);
        }
        if status == 403 && Said::read(body).limited() {
            return Err(answered.failure(status, body));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
