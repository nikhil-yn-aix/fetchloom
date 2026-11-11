//! a dataset listed at one version: the view endpoint names the dataset and its newest version,
//! and the list endpoint sends its files page by page.

use std::collections::HashSet;

use serde::Deserialize;
use url::Url;

use super::{Dataset, HOST, is_part};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::source::{Access, Listing, Reference, Remote};

/// the most files kaggle sends in one page, whatever page size is asked for.
const PAGE: usize = 200;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct View {
    /// `owner/name`, as kaggle spells it.
    #[serde(rename = "ref")]
    reference: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    license_name: String,
    current_version_number: u32,
    /// whether only accounts the owner allows may read the dataset.
    #[serde(default)]
    is_private: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    dataset_files: Vec<File>,
    #[serde(default)]
    error_message: String,
    #[serde(default)]
    next_page_token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    name: String,
    total_bytes: u64,
}

impl Dataset {
    /// lists the dataset at one version: the newest, unless this reference pins one.
    pub(in crate::source) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let view = self.view(api).await?;
        let newest = view.current_version_number;
        let version = self.version.unwrap_or(newest);
        let pinned = Dataset {
            version: Some(version),
            ..self.named(&view.reference)?
        };
        let files = pinned.files(api, version, newest).await?;
        let access = match view.is_private {
            Some(true) => Access::Login,
            Some(false) => Access::Public,
            None => Access::Unknown,
        };
        Ok(Listing {
            within: Vec::new(),
            access,
            unavailable: Vec::new(),
            warnings: Vec::new(),
            resolved: Reference::Kaggle(pinned),
            title: Some(view.title).filter(|title| !title.is_empty()),
            license: Some(view.license_name)
                .filter(|license| !license.is_empty() && license != "Unknown"),
            doi: None,
            files,
        })
    }

    async fn view(&self, api: &impl Api) -> Result<View, Error> {
        let response = api.get(&self.api("view")?).await?;
        crate::source::json(self, &response).map_err(|err| {
            if err.kind == Kind::AuthRequired {
                // kaggle answers 403 alike for a private dataset and for one that does not exist
                let signed_in = super::token::find(&crate::source::env).is_some();
                self.hidden(err, signed_in)
            } else {
                err
            }
        })
    }

    /// kaggle's refusal to show a dataset, which may not exist at all. signing in can only help
    /// when no key was sent.
    fn hidden(&self, mut err: Error, signed_in: bool) -> Error {
        let name = format!("{}/{}", self.owner, self.slug);
        if signed_in {
            err.message = format!("kaggle has no dataset {name}, or this account cannot see it");
            err.need = None;
            err.help(format!("check the name at {}", self.page()))
        } else {
            err.message = format!("kaggle has no dataset {name}, or it is private");
            err.help(format!(
                "check the name at {}. a private dataset needs an account that can read it: sign in with `fetchloom auth login kaggle`",
                self.page()
            ))
            .needs_or_missing(crate::auth::Account::Kaggle)
        }
    }

    /// every file at `version`, following the pages kaggle sends.
    async fn files(&self, api: &impl Api, version: u32, newest: u32) -> Result<Vec<Remote>, Error> {
        let version = version.to_string();
        let mut files = Vec::new();
        let mut tokens = HashSet::new();
        let mut token = String::new();
        loop {
            let response = api.get(&self.list(&version, &token)?).await?;
            if response.status == 404 && token.is_empty() {
                return Err(Error::new(
                    Kind::ReferenceUnresolved,
                    format!(
                        "kaggle:{}/{} has no version {version}",
                        self.owner, self.slug
                    ),
                )
                .help(format!(
                    "check the versions at {} and update the reference on the command line or in data.toml. the newest version is {newest}",
                    self.page()
                )));
            }
            let page: Page = crate::source::json(self, &response)?;
            if !page.error_message.is_empty() {
                return Err(self.unlisted(&page.error_message));
            }
            // each page then adds a file, so the file limit bounds the pages too
            if page.dataset_files.is_empty() && !page.next_page_token.is_empty() {
                return Err(self.invalid("the listing sent an empty page with more to come"));
            }
            for file in page.dataset_files {
                files.push(self.remote(&version, file)?);
            }
            if files.len() > crate::source::most_files() {
                return Err(crate::source::too_many(
                    self,
                    "select the files you need with --select",
                ));
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

    /// the page of files at `version` that `token` leads to, or the first page.
    fn list(&self, version: &str, token: &str) -> Result<Url, Error> {
        let mut url = self.api("list")?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("datasetVersionNumber", version)
                .append_pair("pageSize", &PAGE.to_string());
            if !token.is_empty() {
                query.append_pair("pageToken", token);
            }
        }
        Ok(url)
    }

    /// one file, downloaded on its own at this version.
    fn remote(&self, version: &str, file: File) -> Result<Remote, Error> {
        let mut url = self.api("download")?;
        url.path_segments_mut()
            .map_err(|()| self.invalid("the download URL has no path"))?
            .push(&file.name);
        // without raw, kaggle sends a large file zipped
        url.query_pairs_mut()
            .append_pair("datasetVersionNumber", version)
            .append_pair("raw", "true");
        Ok(Remote {
            path: file.name,
            size: Some(file.total_bytes),
            sha256: None,
            md5: None,
            url,
            named_by_server: false,
        })
    }

    /// this dataset under the name kaggle gives it, which may differ in case.
    fn named(&self, reference: &str) -> Result<Dataset, Error> {
        match reference.split_once('/') {
            Some((owner, slug)) if is_part(owner) && is_part(slug) => Ok(Dataset {
                owner: owner.to_owned(),
                slug: slug.to_owned(),
                version: None,
            }),
            _ => Err(self.invalid(&format!("it names the dataset \"{reference}\""))),
        }
    }

    /// an API URL about this dataset, such as `https://api.kaggle.com/v1/datasets/view/owner/name`.
    fn api(&self, action: &str) -> Result<Url, Error> {
        let text = format!(
            "https://{HOST}/v1/datasets/{action}/{}/{}",
            self.owner, self.slug
        );
        Url::parse(&text).map_err(|err| self.invalid(&format!("{text} is not a link: {err}")))
    }

    /// the dataset's page on kaggle.com.
    fn page(&self) -> String {
        format!(
            "https://www.kaggle.com/datasets/{}/{}",
            self.owner, self.slug
        )
    }

    /// the error for a listing kaggle answered with `said` instead of files. a dataset type its
    /// api does not serve says so on every try, so no retry helps.
    fn unlisted(&self, said: &str) -> Error {
        let detail = format!("kaggle says: {said}");
        if said
            .to_ascii_lowercase()
            .contains("not supported through the api")
        {
            return Error::new(
                Kind::ReferenceUnresolved,
                format!(
                    "kaggle:{}/{} cannot be fetched through kaggle's api",
                    self.owner, self.slug
                ),
            )
            .detail(detail)
            .help(format!(
                "open {} to see how kaggle offers it, or choose another dataset",
                self.page()
            ));
        }
        Error::new(Kind::SourceInvalid, format!("kaggle could not list {self}"))
            .detail(detail)
            .help(format!(
                "run the same command again later. if it keeps happening, check the dataset at {}",
                self.page()
            ))
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("kaggle sent a listing of {self} that fetchloom cannot use: {reason}"),
        )
        .help("report this response at https://github.com/nikhil-yn-aix/fetchloom/issues")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::net::{Net, Options};
    use crate::source::recorded::{Recorded, block_on, fixture_bytes};
    use crate::work::Work;

    const API: &str = "https://api.kaggle.com/v1/datasets";

    fn dataset(text: &str) -> Dataset {
        Dataset::parse(text).unwrap()
    }

    fn view(reference: &str) -> String {
        format!("{API}/view/{reference}")
    }

    /// the first page of files at a version.
    fn files(reference: &str, version: u32) -> String {
        format!("{API}/list/{reference}?datasetVersionNumber={version}&pageSize=200")
    }

    /// a recorded answer with one change made to it.
    fn changed(fixture: &str, change: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
        let mut value = serde_json::from_slice(&fixture_bytes(fixture)).unwrap();
        change(&mut value);
        serde_json::to_vec(&value).unwrap()
    }

    /// kaggle as it answered for uciml/iris, whose newest version is 2.
    fn iris() -> Recorded {
        Recorded::default()
            .fixture(&view("uciml/iris"), "kaggle/iris-view.json")
            .fixture(&files("uciml/iris", 2), "kaggle/iris-files.json")
    }

    /// the token that leads from the first page of siim-medical-images to the second.
    fn siim_token() -> String {
        let page: serde_json::Value =
            serde_json::from_slice(&fixture_bytes("kaggle/siim-files-1.json")).unwrap();
        page["nextPageToken"].as_str().unwrap().to_owned()
    }

    #[test]
    fn a_small_dataset_is_listed_at_its_newest_version() {
        let recorded = iris();
        let listing = block_on(dataset("uciml/iris").resolve(&recorded)).unwrap();
        assert_eq!(listing.resolved.to_string(), "kaggle:uciml/iris@2");
        assert_eq!(listing.title.as_deref(), Some("Iris Species"));
        assert_eq!(listing.license.as_deref(), Some("CC0: Public Domain"));
        assert_eq!(listing.doi, None);
        assert_eq!(listing.access, Access::Public);
        let file = |path: &str, size| Remote {
            path: path.to_owned(),
            size: Some(size),
            sha256: None,
            md5: None,
            url: Url::parse(&format!(
                "{API}/download/uciml/iris/{path}?datasetVersionNumber=2&raw=true"
            ))
            .unwrap(),
            named_by_server: false,
        };
        assert_eq!(
            listing.files,
            [file("Iris.csv", 5107), file("database.sqlite", 10240)]
        );
        assert_eq!(
            recorded.asked(),
            [view("uciml/iris"), files("uciml/iris", 2)]
        );
        assert_eq!(listing.files[0].expected_size(), Some(5107), "raw=true");
    }

    #[test]
    fn the_resolved_reference_lists_the_same_files_again() {
        let recorded = iris();
        let first = block_on(dataset("uciml/iris").resolve(&recorded)).unwrap();
        let again = block_on(first.resolved.resolve(&recorded)).unwrap();
        assert_eq!(again.resolved, first.resolved);
        assert_eq!(again.files, first.files);
        assert_eq!(
            recorded.asked()[2..],
            [view("uciml/iris"), files("uciml/iris", 2)]
        );
    }

    #[test]
    fn a_name_in_other_letters_resolves_to_the_name_kaggle_gives() {
        // kaggle answers UCIML/Iris with the same body as uciml/iris
        let recorded = iris().fixture(&view("UCIML/Iris"), "kaggle/iris-view.json");
        let listing = block_on(dataset("UCIML/Iris").resolve(&recorded)).unwrap();
        assert_eq!(listing.resolved.to_string(), "kaggle:uciml/iris@2");
        assert_eq!(
            recorded.asked(),
            [view("UCIML/Iris"), files("uciml/iris", 2)]
        );
    }

    #[test]
    fn a_listing_sent_in_pages_is_read_to_its_last_page() {
        let reference = "kmader/siim-medical-images";
        let first = files(reference, 6);
        let second = format!("{first}&pageToken={}", siim_token());
        // the first page is trimmed to 3 of the 200 files kaggle sent
        let recorded = Recorded::default()
            .fixture(&view(reference), "kaggle/siim-view.json")
            .fixture(&first, "kaggle/siim-files-1.json")
            .fixture(&second, "kaggle/siim-files-2.json");
        let listing = block_on(dataset(reference).resolve(&recorded)).unwrap();
        let paths: Vec<&str> = listing
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "dicom_dir/ID_0000_AGE_0060_CONTRAST_1_CT.dcm",
                "dicom_dir/ID_0001_AGE_0069_CONTRAST_1_CT.dcm",
                "dicom_dir/ID_0002_AGE_0074_CONTRAST_1_CT.dcm",
                "tiff_images/ID_0098_AGE_0061_CONTRAST_0_CT.tif",
                "tiff_images/ID_0099_AGE_0061_CONTRAST_0_CT.tif",
            ]
        );
        assert_eq!(
            listing.files[0].url.as_str(),
            format!(
                "{API}/download/kmader/siim-medical-images/dicom_dir%2FID_0000_AGE_0060_CONTRAST_1_CT.dcm?datasetVersionNumber=6&raw=true"
            )
        );
        assert_eq!(recorded.asked(), [view(reference), first, second]);
    }

    #[test]
    fn a_page_sent_twice_is_refused() {
        let reference = "kmader/siim-medical-images";
        let first = files(reference, 6);
        let second = format!("{first}&pageToken={}", siim_token());
        let recorded = Recorded::default()
            .fixture(&view(reference), "kaggle/siim-view.json")
            .fixture(&first, "kaggle/siim-files-1.json")
            .fixture(&second, "kaggle/siim-files-1.json");
        let err = block_on(dataset(reference).resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid);
    }

    #[test]
    fn an_empty_page_that_promises_more_is_refused() {
        let reference = "kmader/siim-medical-images";
        let first = files(reference, 6);
        let recorded = Recorded::default()
            .fixture(&view(reference), "kaggle/siim-view.json")
            .on(
                &first,
                200,
                r#"{"datasetFiles":[],"nextPageToken":"again"}"#,
            );
        let err = block_on(dataset(reference).resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert!(err.message.contains("empty page"), "{}", err.message);
    }

    #[test]
    fn file_names_are_one_encoded_url_segment() {
        let body = changed("kaggle/iris-files.json", |value| {
            value["datasetFiles"][0]["name"] = "data/a #1%?.csv".into();
        });
        let recorded = Recorded::default()
            .fixture(&view("uciml/iris"), "kaggle/iris-view.json")
            .on(&files("uciml/iris", 2), 200, body);
        let listing = block_on(dataset("uciml/iris").resolve(&recorded)).unwrap();
        assert_eq!(listing.files[0].path, "data/a #1%?.csv");
        assert_eq!(
            listing.files[0].url.as_str(),
            format!(
                "{API}/download/uciml/iris/data%2Fa%20%231%25%3F.csv?datasetVersionNumber=2&raw=true"
            )
        );
    }

    #[test]
    fn a_version_kaggle_does_not_have_names_the_newest() {
        let recorded = Recorded::default()
            .fixture(&view("uciml/iris"), "kaggle/iris-view.json")
            .on(
                &files("uciml/iris", 9),
                404,
                fixture_bytes("kaggle/no-version.json"),
            );
        let err = block_on(dataset("uciml/iris@9").resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::ReferenceUnresolved);
        assert_eq!(err.message, "kaggle:uciml/iris has no version 9");
        assert_eq!(
            err.help.as_deref(),
            Some(
                "check the versions at https://www.kaggle.com/datasets/uciml/iris and update the reference on the command line or in data.toml. the newest version is 2"
            )
        );
    }

    #[test]
    fn a_dataset_kaggle_will_not_show_names_its_page() {
        // kaggle answers 403 alike for a private dataset and for one that does not exist
        let recorded = Recorded::default().on(
            &view("uciml/no-such-dataset"),
            403,
            fixture_bytes("kaggle/missing.json"),
        );
        let err = block_on(dataset("uciml/no-such-dataset").resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::AuthRequired);
        // unit tests see no kaggle key, whatever the machine running them keeps
        assert_eq!(
            err.message,
            "kaggle has no dataset uciml/no-such-dataset, or it is private"
        );
        let help = err.help.unwrap();
        assert!(
            help.contains("https://www.kaggle.com/datasets/uciml/no-such-dataset"),
            "{help}"
        );
        assert_eq!(recorded.asked().len(), 1);
    }

    #[test]
    fn a_hidden_dataset_suggests_signing_in_only_without_a_key() {
        let refused = || crate::source::refused("www.kaggle.com", "kaggle:uciml/nope");
        let without = dataset("uciml/nope").hidden(refused(), false);
        assert_eq!(
            without.message,
            "kaggle has no dataset uciml/nope, or it is private"
        );
        assert!(
            without
                .help
                .unwrap()
                .contains("sign in with `fetchloom auth login kaggle`")
        );
        assert_eq!(
            without.need.map(|need| (need.account, need.maybe_missing)),
            Some((crate::auth::Account::Kaggle, true)),
            "the offer to sign in says first that the name may be wrong"
        );
        let with = dataset("uciml/nope").hidden(refused(), true);
        assert_eq!(
            with.message,
            "kaggle has no dataset uciml/nope, or this account cannot see it"
        );
        assert_eq!(
            with.help.as_deref(),
            Some("check the name at https://www.kaggle.com/datasets/uciml/nope")
        );
        assert!(with.need.is_none());
    }

    #[test]
    fn an_error_inside_a_listing_is_not_an_empty_dataset() {
        let body = changed("kaggle/iris-files.json", |value| {
            value["datasetFiles"] = serde_json::json!([]);
            value["errorMessage"] = "the files are not ready".into();
        });
        let recorded = Recorded::default()
            .fixture(&view("uciml/iris"), "kaggle/iris-view.json")
            .on(&files("uciml/iris", 2), 200, body);
        let err = block_on(dataset("uciml/iris").resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert!(
            err.details
                .iter()
                .any(|line| line.contains("the files are not ready")),
            "{:?}",
            err.details
        );
    }

    /// kaggle lists some datasets, such as epa/epa-historical-air-quality, but sends none of their
    /// files through its api, every time: no retry can help, so the help says where else to look.
    #[test]
    fn a_dataset_kaggle_will_not_send_through_its_api_is_not_retried() {
        let body = changed("kaggle/iris-files.json", |value| {
            value["datasetFiles"] = serde_json::json!([]);
            value["errorMessage"] = "This dataset type is not supported through the API.".into();
        });
        let recorded = Recorded::default()
            .fixture(&view("uciml/iris"), "kaggle/iris-view.json")
            .on(&files("uciml/iris", 2), 200, body);
        let err = block_on(dataset("uciml/iris").resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::ReferenceUnresolved);
        assert_eq!(
            err.message,
            "kaggle:uciml/iris cannot be fetched through kaggle's api"
        );
        assert_eq!(
            err.details,
            ["kaggle says: This dataset type is not supported through the API."]
        );
        assert_eq!(
            err.help.as_deref(),
            Some(
                "open https://www.kaggle.com/datasets/uciml/iris to see how kaggle offers it, or choose another dataset"
            )
        );
    }

    #[test]
    fn a_private_dataset_needs_an_account() {
        let body = changed("kaggle/iris-view.json", |value| {
            value["isPrivate"] = true.into();
        });
        let recorded = Recorded::default()
            .on(&view("uciml/iris"), 200, body)
            .fixture(&files("uciml/iris", 2), "kaggle/iris-files.json");
        let listing = block_on(dataset("uciml/iris").resolve(&recorded)).unwrap();
        assert_eq!(listing.access, Access::Login);
    }

    #[test]
    fn an_unknown_license_is_no_license() {
        let body = changed("kaggle/iris-view.json", |value| {
            value["licenseName"] = "Unknown".into();
        });
        let recorded = Recorded::default()
            .on(&view("uciml/iris"), 200, body)
            .fixture(&files("uciml/iris", 2), "kaggle/iris-files.json");
        let listing = block_on(dataset("uciml/iris").resolve(&recorded)).unwrap();
        assert_eq!(listing.license, None);
    }

    #[test]
    #[ignore = "live: talks to api.kaggle.com"]
    fn live_iris_resolves_to_a_version() {
        let work = Arc::new(Work::default());
        let options = Options {
            offline: crate::net::Offline::No,
            jobs: 4,
            credentials: Vec::new(),
            split: crate::net::Splitting::WhenSlow,
        };
        let net = Net::new(options, work.clone(), Arc::default(), Arc::new(|_| {}));
        let reference = Reference::parse("kaggle:uciml/iris").unwrap().reference;
        let listing = block_on(reference.resolve(&net)).unwrap();
        let Reference::Kaggle(pinned) = &listing.resolved else {
            panic!("{}", listing.resolved);
        };
        assert!(pinned.version.is_some_and(|version| version >= 2));
        assert!(
            listing
                .files
                .iter()
                .any(|file| file.path == "Iris.csv" && file.size == Some(5107))
        );
        assert_eq!(work.counts().requests, 2);
    }
}
