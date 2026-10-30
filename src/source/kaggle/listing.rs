//! a dataset listed at one version: the view endpoint names the dataset and its newest version,
//! and the list endpoint sends its files page by page.

use std::collections::HashSet;

use serde::Deserialize;
use url::Url;

use super::{Dataset, HOST, is_part};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::source::{Listing, MOST_FILES, Reference, Remote};

/// the most files kaggle sends in one page, whatever page size is asked for.
const PAGE: usize = 200;

/// what the view endpoint says about a dataset.
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
}

/// one page of the list endpoint.
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
        Ok(Listing {
            unavailable: Vec::new(),
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
                err.help(format!(
                    "check the name at {}. a private dataset needs KAGGLE_API_TOKEN set to a token from https://www.kaggle.com/settings/api",
                    self.page()
                ))
            } else {
                err
            }
        })
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
                .help(format!("its newest version is {newest}")));
            }
            let page: Page = crate::source::json(self, &response)?;
            if !page.error_message.is_empty() {
                return Err(Error::new(
                    Kind::SourceInvalid,
                    format!("kaggle could not list {self}"),
                )
                .detail(format!("kaggle says: {}", page.error_message))
                .help(format!(
                    "run the same command again later. if it keeps happening, check the dataset at {}",
                    self.page()
                )));
            }
            for file in page.dataset_files {
                files.push(self.remote(&version, file)?);
            }
            if files.len() > MOST_FILES {
                return Err(Error::new(
                    Kind::SourceInvalid,
                    format!(
                        "{self} holds more than {MOST_FILES} files, more than fetchloom handles in one dataset"
                    ),
                )
                .help("pick a smaller dataset, or ask its owner to pack the files into archives"));
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

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("kaggle sent a listing of {self} that fetchloom cannot use: {reason}"),
        )
        .help("this usually means the service changed. please report it at https://github.com/nikhil-yn-aix/fetchloom/issues")
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
        assert_eq!(err.help.as_deref(), Some("its newest version is 2"));
    }

    #[test]
    fn a_dataset_kaggle_will_not_show_names_the_token_to_set() {
        // kaggle answers 403 alike for a private dataset and for one that does not exist
        let recorded = Recorded::default().on(
            &view("uciml/no-such-dataset"),
            403,
            fixture_bytes("kaggle/missing.json"),
        );
        let err = block_on(dataset("uciml/no-such-dataset").resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::AuthRequired);
        let help = err.help.unwrap();
        assert!(
            help.contains("https://www.kaggle.com/datasets/uciml/no-such-dataset")
                && help.contains("KAGGLE_API_TOKEN")
                && help.contains("https://www.kaggle.com/settings/api"),
            "{help}"
        );
        assert_eq!(recorded.asked().len(), 1);
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
