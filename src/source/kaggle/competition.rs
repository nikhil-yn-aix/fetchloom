//! a kaggle competition: its data files, listed page by page and downloaded one at a time, for a
//! signed-in account that accepted the competition's rules. competitions have no versions, so a
//! listing records what kaggle sends today and the downloaded bytes pin it.

use std::collections::HashSet;
use std::fmt;

use serde::Deserialize;
use url::Url;

use super::{HOST, is_part, malformed, parts, selected};
use crate::auth::Account;
use crate::error::{Error, Kind};
use crate::net::{Api, Response};
use crate::source::{Listing, MOST_FILES, Reference, Remote};

/// files asked for in one page, as for datasets. a smaller page from kaggle only means more
/// pages.
const PAGE: usize = 200;

/// a kaggle competition, by the name in its kaggle.com link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Competition {
    slug: String,
}

/// what kaggle says about a competition, which it answers without an account.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct About {
    /// the competition's kaggle.com link, which names it as kaggle spells it.
    #[serde(rename = "ref", default)]
    reference: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    license_name: String,
    /// whether the account asking accepted the rules. kaggle sends `false` without an account.
    user_has_entered: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    #[serde(default)]
    files: Vec<File>,
    #[serde(default)]
    next_page_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    name: String,
    #[serde(default)]
    total_bytes: u64,
}

/// the message in an error kaggle's API sends.
#[derive(Deserialize)]
struct Refusal {
    #[serde(default)]
    message: String,
}

impl Competition {
    /// reads the name after `kaggle:competitions/`, such as `titanic`.
    pub(super) fn parse(text: &str) -> Result<Competition, Error> {
        let slug = text.trim();
        if slug.contains('@') {
            return Err(malformed(
                &format!("kaggle:competitions/{slug}"),
                "kaggle:competitions/name, without @version: competitions have no versions",
            ));
        }
        if !is_name(slug) {
            return Err(malformed(
                &format!("kaggle:competitions/{slug}"),
                "kaggle:competitions/name, with the name from the competition's kaggle.com link",
            ));
        }
        Ok(Competition {
            slug: slug.to_owned(),
        })
    }

    /// a pasted kaggle.com competition link, under `/competitions/` or `/c/`, and the file in it
    /// when the link selects one.
    pub(super) fn from_link(url: &Url) -> Option<(Competition, Option<String>)> {
        match parts(url)?.as_slice() {
            ["competitions" | "c", slug, ..] if is_name(slug) => {
                let competition = Competition {
                    slug: (*slug).to_owned(),
                };
                Some((competition, selected(url)))
            }
            _ => None,
        }
    }

    /// lists the competition's data files as kaggle sends them now.
    pub(in crate::source) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let about = self.about(api).await?;
        let named = self.named(&about.reference)?;
        let files = named.files(api).await?;
        // kaggle refuses the downloads of an account that has not accepted the rules, so stop
        // before them
        if about.user_has_entered == Some(false) {
            return Err(named.rules(None));
        }
        Ok(Listing {
            unavailable: Vec::new(),
            resolved: Reference::KaggleCompetition(named),
            title: Some(about.title).filter(|title| !title.is_empty()),
            license: Some(about.license_name)
                .filter(|license| !license.is_empty() && license != "Unknown"),
            doi: None,
            files,
        })
    }

    async fn about(&self, api: &impl Api) -> Result<About, Error> {
        let response = api.get(&self.api("get")?).await?;
        match response.status {
            401 => Err(self.sign_in(&response)),
            403 => Err(self.rules(Some(&response))),
            _ => crate::source::json(self, &response),
        }
    }

    /// every data file, following the pages kaggle sends.
    async fn files(&self, api: &impl Api) -> Result<Vec<Remote>, Error> {
        let mut files = Vec::new();
        let mut tokens = HashSet::new();
        let mut token = String::new();
        loop {
            let response = api.get(&self.list(&token)?).await?;
            match response.status {
                401 => return Err(self.sign_in(&response)),
                403 => return Err(self.rules(Some(&response))),
                _ => {}
            }
            let page: Page = crate::source::json(self, &response)?;
            for file in page.files {
                files.push(self.remote(file)?);
            }
            if files.len() > MOST_FILES {
                return Err(Error::new(
                    Kind::SourceInvalid,
                    format!(
                        "{self} holds more than {MOST_FILES} files, more than fetchloom handles in one dataset"
                    ),
                )
                .help(format!("download the competition with the kaggle CLI: kaggle competitions download -c {}", self.slug)));
            }
            if page.next_page_token.is_empty() {
                return Ok(files);
            }
            if !tokens.insert(page.next_page_token.clone()) {
                return Err(self.invalid("the listing sent the same page twice"));
            }
            token = page.next_page_token;
        }
    }

    /// the page of files that `token` leads to, or the first page.
    fn list(&self, token: &str) -> Result<Url, Error> {
        let mut url = self.api("data/list")?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("pageSize", &PAGE.to_string());
            if !token.is_empty() {
                query.append_pair("pageToken", token);
            }
        }
        Ok(url)
    }

    /// one data file, downloaded on its own.
    fn remote(&self, file: File) -> Result<Remote, Error> {
        let mut url = self.api("data/download")?;
        url.path_segments_mut()
            .map_err(|()| self.invalid("the download URL has no path"))?
            .push(&file.name);
        Ok(Remote {
            path: file.name,
            size: Some(file.total_bytes),
            sha256: None,
            md5: None,
            url,
            named_by_server: false,
        })
    }

    /// this competition under the name in the link kaggle gives it, which may differ in case.
    fn named(&self, reference: &str) -> Result<Competition, Error> {
        if reference.is_empty() {
            return Ok(self.clone());
        }
        Url::parse(reference)
            .ok()
            .as_ref()
            .and_then(Competition::from_link)
            .map(|(competition, _)| competition)
            .ok_or_else(|| self.invalid(&format!("it names the competition \"{reference}\"")))
    }

    /// an API URL about this competition, such as
    /// `https://api.kaggle.com/v1/competitions/data/list/titanic`.
    fn api(&self, action: &str) -> Result<Url, Error> {
        let text = format!("https://{HOST}/v1/competitions/{action}/{}", self.slug);
        Url::parse(&text).map_err(|err| self.invalid(&format!("{text} is not a link: {err}")))
    }

    /// kaggle's answer without an account, or with a key it does not take.
    fn sign_in(&self, response: &Response) -> Error {
        said(
            Error::new(
                Kind::AuthRequired,
                format!("{self} needs a signed-in kaggle account"),
            )
            .help("sign in with `fetchloom auth login kaggle`, then run the same command again")
            .needs(Account::Kaggle, None),
            response,
        )
    }

    /// the refusal for an account that has not accepted the competition's rules.
    fn rules(&self, response: Option<&Response>) -> Error {
        let page = format!("https://www.kaggle.com/competitions/{}/rules", self.slug);
        let error = Error::new(
            Kind::AuthRequired,
            format!("{self} needs its rules accepted before kaggle sends its data"),
        )
        .help(format!(
            "accept the rules at {page} while signed in to kaggle, and verify a phone number if kaggle asks, then run the same command again"
        ))
        .needs(Account::Kaggle, Some(page));
        match response {
            Some(response) => said(error, response),
            None => error,
        }
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("kaggle sent a listing of {self} that fetchloom cannot use: {reason}"),
        )
        .help("report this response at https://github.com/nikhil-yn-aix/fetchloom/issues")
    }
}

/// a competition name, which becomes a segment of API URLs: `.` and `..` would move them.
fn is_name(text: &str) -> bool {
    is_part(text) && text.chars().any(|c| c != '.')
}

/// the error with kaggle's own words added, when its answer has any.
fn said(error: Error, response: &Response) -> Error {
    match response.json::<Refusal>() {
        Ok(refusal) if !refusal.message.is_empty() => {
            error.detail(format!("kaggle says: {}", refusal.message))
        }
        _ => error,
    }
}

impl fmt::Display for Competition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "kaggle:competitions/{}", self.slug)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::glob::Glob;
    use crate::net::{Net, Options};
    use crate::source::recorded::{Recorded, block_on, fixture_bytes};
    use crate::work::Work;

    const API: &str = "https://api.kaggle.com/v1/competitions";

    fn competition(name: &str) -> Competition {
        Competition::parse(name).unwrap()
    }

    fn about(name: &str) -> String {
        format!("{API}/get/{name}")
    }

    /// the first page of a competition's files.
    fn files(name: &str) -> String {
        format!("{API}/data/list/{name}?pageSize=200")
    }

    /// kaggle's answer about titanic, recorded without an account, as a signed-in account that
    /// accepted the rules would get it.
    fn entered() -> Vec<u8> {
        let mut value: serde_json::Value =
            serde_json::from_slice(&fixture_bytes("kaggle/titanic-competition.json")).unwrap();
        value["userHasEntered"] = true.into();
        serde_json::to_vec(&value).unwrap()
    }

    /// kaggle for an account that accepted titanic's rules. listing a competition's files needs
    /// an account, so the listings follow the kaggle SDK's `ApiListDataFilesResponse` with the
    /// sizes kaggle states for titanic's files.
    fn titanic() -> Recorded {
        Recorded::default()
            .on(&about("titanic"), 200, entered())
            .fixture(&files("titanic"), "kaggle/titanic-files.json")
    }

    fn download(path: &str) -> Url {
        Url::parse(&format!("{API}/data/download/titanic/{path}")).unwrap()
    }

    fn remote(path: &str, size: u64) -> Remote {
        Remote {
            path: path.to_owned(),
            size: Some(size),
            sha256: None,
            md5: None,
            url: download(path),
            named_by_server: false,
        }
    }

    fn paths(listing: &Listing) -> Vec<&str> {
        listing
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect()
    }

    #[test]
    fn a_competition_lists_its_data_files_with_their_sizes() {
        let recorded = titanic();
        let listing = block_on(competition("titanic").resolve(&recorded)).unwrap();
        assert_eq!(listing.resolved.to_string(), "kaggle:competitions/titanic");
        assert_eq!(
            listing.title.as_deref(),
            Some("Titanic - Machine Learning from Disaster")
        );
        assert_eq!(
            listing.license.as_deref(),
            Some("Subject to Competition Rules")
        );
        assert_eq!(listing.doi, None);
        assert_eq!(
            listing.files,
            [
                remote("gender_submission.csv", 3258),
                remote("test.csv", 28629),
                remote("train.csv", 61194),
            ]
        );
        assert_eq!(recorded.asked(), [about("titanic"), files("titanic")]);
        let again = block_on(listing.resolved.resolve(&recorded)).unwrap();
        assert_eq!(again.files, listing.files);
    }

    #[test]
    fn a_name_in_other_letters_resolves_to_the_name_kaggle_gives() {
        let recorded = titanic().on(&about("Titanic"), 200, entered());
        let listing = block_on(competition("Titanic").resolve(&recorded)).unwrap();
        assert_eq!(listing.resolved.to_string(), "kaggle:competitions/titanic");
        assert_eq!(recorded.asked(), [about("Titanic"), files("titanic")]);
    }

    #[test]
    fn a_listing_sent_in_pages_is_read_to_its_last_page() {
        let token = "Cgh0ZXN0LmNzdg";
        let second = format!("{}&pageToken={token}", files("titanic"));
        let recorded = Recorded::default()
            .on(&about("titanic"), 200, entered())
            .fixture(&files("titanic"), "kaggle/titanic-files-1.json")
            .fixture(&second, "kaggle/titanic-files-2.json");
        let listing = block_on(competition("titanic").resolve(&recorded)).unwrap();
        assert_eq!(
            paths(&listing),
            ["gender_submission.csv", "test.csv", "train.csv"]
        );
        assert_eq!(
            recorded.asked(),
            [about("titanic"), files("titanic"), second.clone()]
        );

        let looping = Recorded::default()
            .on(&about("titanic"), 200, entered())
            .fixture(&files("titanic"), "kaggle/titanic-files-1.json")
            .fixture(&second, "kaggle/titanic-files-1.json");
        let err = block_on(competition("titanic").resolve(&looping)).unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid);
    }

    #[test]
    fn file_paths_are_one_encoded_url_segment() {
        let mut value: serde_json::Value =
            serde_json::from_slice(&fixture_bytes("kaggle/titanic-files.json")).unwrap();
        value["files"][0]["name"] = "train/images/a #1%?.png".into();
        let recorded = Recorded::default()
            .on(&about("titanic"), 200, entered())
            .on(&files("titanic"), 200, serde_json::to_vec(&value).unwrap());
        let listing = block_on(competition("titanic").resolve(&recorded)).unwrap();
        assert_eq!(listing.files[0].path, "train/images/a #1%?.png");
        assert_eq!(
            listing.files[0].url.as_str(),
            format!("{API}/data/download/titanic/train%2Fimages%2Fa%20%231%25%3F.png")
        );
    }

    #[test]
    fn select_and_exclude_match_the_listed_paths() {
        let listing = block_on(competition("titanic").resolve(&titanic())).unwrap();
        let select = [Glob::new("*.csv").unwrap()];
        let exclude = [Glob::new("gender_*").unwrap()];
        let kept: Vec<&str> = listing
            .paths()
            .filter(|path| select.iter().any(|glob| glob.matches(path)))
            .filter(|path| !exclude.iter().any(|glob| glob.matches(path)))
            .collect();
        assert_eq!(kept, ["test.csv", "train.csv"]);
        let only = Glob::new("train.csv").unwrap();
        let picked: Vec<&Remote> = listing
            .files
            .iter()
            .filter(|file| only.matches(&file.path))
            .collect();
        assert_eq!(picked, [&remote("train.csv", 61194)]);
    }

    #[test]
    fn without_an_account_kaggle_answers_401_and_the_error_names_the_sign_in() {
        // recorded from api.kaggle.com without credentials
        let recorded = Recorded::default()
            .fixture(&about("titanic"), "kaggle/titanic-competition.json")
            .on(
                &files("titanic"),
                401,
                fixture_bytes("kaggle/unauthenticated.json"),
            );
        let err = block_on(competition("titanic").resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::AuthRequired);
        assert_eq!(err.kind.name(), "auth.required");
        assert_eq!(
            err.message,
            "kaggle:competitions/titanic needs a signed-in kaggle account"
        );
        assert!(
            err.help
                .as_deref()
                .unwrap()
                .contains("fetchloom auth login kaggle"),
            "{:?}",
            err.help
        );
        assert_eq!(err.details, ["kaggle says: Unauthenticated"]);
        let need = err.need.unwrap();
        assert_eq!(
            (need.account, need.terms),
            (crate::auth::Account::Kaggle, None)
        );
    }

    #[test]
    fn rules_not_accepted_name_the_rules_page() {
        let rules = "https://www.kaggle.com/competitions/titanic/rules";
        let check = |err: Error, said: bool| {
            assert_eq!(err.kind, Kind::AuthRequired);
            let help = err.help.unwrap();
            assert!(
                help.contains(rules) && help.contains("accept the rules") && help.contains("phone"),
                "{help}"
            );
            assert_eq!(
                err.details
                    .iter()
                    .any(|line| line.contains("accept this competition's rules")),
                said,
                "{:?}",
                err.details
            );
            let need = err.need.unwrap();
            assert_eq!(need.account, crate::auth::Account::Kaggle);
            assert_eq!(need.terms.as_deref(), Some(rules));
        };
        let refused = Recorded::default()
            .on(&about("titanic"), 200, entered())
            .on(
                &files("titanic"),
                403,
                fixture_bytes("kaggle/rules-not-accepted.json"),
            );
        check(
            block_on(competition("titanic").resolve(&refused)).unwrap_err(),
            true,
        );
        // an account that has not entered may still see the list, but not download the files
        let listed = Recorded::default()
            .fixture(&about("titanic"), "kaggle/titanic-competition.json")
            .fixture(&files("titanic"), "kaggle/titanic-files.json");
        check(
            block_on(competition("titanic").resolve(&listed)).unwrap_err(),
            false,
        );
    }

    #[test]
    fn a_competition_kaggle_does_not_have_is_unresolved() {
        // recorded from api.kaggle.com
        let recorded = Recorded::default().on(
            &about("no-such-competition"),
            404,
            fixture_bytes("kaggle/no-competition.json"),
        );
        let err = block_on(competition("no-such-competition").resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::ReferenceUnresolved);
        assert!(
            err.message
                .contains("kaggle:competitions/no-such-competition")
        );
        assert_eq!(recorded.asked().len(), 1);
    }

    #[test]
    fn a_link_kaggle_gives_that_names_no_competition_is_refused() {
        let mut value: serde_json::Value = serde_json::from_slice(&entered()).unwrap();
        value["ref"] = "https://www.kaggle.com/datasets/uciml/iris".into();
        let recorded =
            Recorded::default().on(&about("titanic"), 200, serde_json::to_vec(&value).unwrap());
        let err = block_on(competition("titanic").resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid);
    }

    #[test]
    #[ignore = "live: talks to api.kaggle.com with a kaggle account that accepted titanic's rules"]
    fn live_titanic_lists_its_files() {
        if super::super::token::find(&crate::auth::env).is_none() {
            eprintln!("skipped: no KAGGLE_API_TOKEN, access_token or kaggle.json on this machine");
            return;
        }
        let work = Arc::new(Work::default());
        let options = Options {
            offline: crate::net::Offline::No,
            jobs: 4,
            credentials: crate::source::kaggle_credentials(&crate::auth::env),
            split: crate::net::Splitting::WhenSlow,
        };
        let net = Net::new(options, work.clone(), Arc::default(), Arc::new(|_| {}));
        let reference = Reference::parse("kaggle:competitions/titanic")
            .unwrap()
            .reference;
        let listing = block_on(reference.resolve(&net)).unwrap();
        assert_eq!(listing.resolved.to_string(), "kaggle:competitions/titanic");
        for (path, size) in [
            ("train.csv", 61194),
            ("test.csv", 28629),
            ("gender_submission.csv", 3258),
        ] {
            assert!(
                listing
                    .files
                    .iter()
                    .any(|file| file.path == path && file.size == Some(size)),
                "{path}: {:?}",
                listing.files
            );
        }
        assert_eq!(work.counts().requests, 2);
        let train = listing
            .files
            .iter()
            .find(|file| file.path == "train.csv")
            .unwrap();
        let response = block_on(net.get(&train.url)).unwrap();
        assert_eq!(response.status, 200, "{}", response.url);
        assert_eq!(response.body.len(), 61194);
        assert!(response.body.starts_with(b"PassengerId,"));
    }
}
