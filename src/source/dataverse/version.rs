//! published dataverse versions and the original files their checksums describe.

use serde::Deserialize;
use serde_json::Value;

use super::Dataset;
use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::source::{Listing, Reference, Remote};

/// a native API answer for a dataset or one of its versions.
#[derive(Deserialize)]
pub(super) struct Answer {
    status: String,
    data: Data,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Data {
    Dataset {
        #[serde(rename = "latestVersion")]
        latest_version: Version,
    },
    Version(Version),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Version {
    dataset_persistent_id: String,
    #[serde(rename = "versionNumber")]
    major: u32,
    #[serde(rename = "versionMinorNumber")]
    minor: u32,
    #[serde(rename = "versionState")]
    state: String,
    metadata_blocks: Metadata,
    license: Option<License>,
    files: Vec<File>,
}

#[derive(Deserialize)]
struct Metadata {
    citation: Option<Citation>,
}

#[derive(Deserialize)]
struct Citation {
    fields: Vec<Field>,
}

#[derive(Deserialize)]
struct Field {
    #[serde(rename = "typeName")]
    name: String,
    value: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct License {
    name: String,
    rights_identifier: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    directory_label: Option<String>,
    restricted: bool,
    #[serde(rename = "dataFile")]
    data: DataFile,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DataFile {
    id: u64,
    filename: String,
    filesize: Option<u64>,
    original_file_name: Option<String>,
    original_file_size: Option<u64>,
    #[serde(default)]
    tabular_data: bool,
    checksum: Option<Checksum>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Checksum {
    #[serde(rename = "MD5")]
    Md5 { value: Md5 },
    #[serde(rename = "SHA-256", alias = "SHA256")]
    Sha256 { value: Sha256 },
    #[serde(other)]
    Other,
}

impl Answer {
    /// lists the published version without requesting file bodies.
    pub(super) fn listing(self, dataset: &Dataset) -> Result<Listing, Error> {
        if self.status != "OK" {
            return Err(dataset.invalid("the API did not return an OK answer"));
        }
        let (Data::Dataset {
            latest_version: version,
        }
        | Data::Version(version)) = self.data;
        version.listing(dataset)
    }
}

impl Version {
    fn listing(self, dataset: &Dataset) -> Result<Listing, Error> {
        let number = format!("{}.{}", self.major, self.minor);
        self.check(dataset, &number)?;
        let title = self.title(dataset)?;
        let files = self
            .files
            .into_iter()
            .map(|file| file.remote(dataset))
            .collect::<Result<_, _>>()?;
        let doi = self
            .dataset_persistent_id
            .split_once(':')
            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("doi"))
            .map(|(_, doi)| doi.to_owned());
        Ok(Listing {
            resolved: Reference::Dataverse(Dataset {
                host: dataset.host.clone(),
                pid: self.dataset_persistent_id,
                version: Some(number.clone()),
            }),
            title,
            version: Some(number),
            license: self.license.map(|license| {
                license
                    .rights_identifier
                    .filter(|id| !id.is_empty())
                    .unwrap_or(license.name)
            }),
            doi,
            files,
        })
    }

    fn check(&self, dataset: &Dataset, number: &str) -> Result<(), Error> {
        if self.state != "RELEASED" || self.major == 0 {
            return Err(dataset.invalid("the version is not published"));
        }
        if !self
            .dataset_persistent_id
            .eq_ignore_ascii_case(&dataset.pid)
        {
            return Err(dataset.invalid("the persistent id does not match"));
        }
        if let Some(requested) = &dataset.version {
            let requested = if requested.contains('.') {
                requested.clone()
            } else {
                format!("{requested}.0")
            };
            if !requested.starts_with(':') && requested != number {
                return Err(dataset.invalid("the version does not match"));
            }
        }
        let restricted: Vec<_> = self
            .files
            .iter()
            .filter(|file| file.restricted)
            .map(|file| format!("\"{}\"", file.path()))
            .collect();
        if !restricted.is_empty() {
            return Err(Error::new(
                Kind::AuthRequired,
                format!("{dataset} restricts access to {}", restricted.join(", ")),
            )
            .help("request a public copy of the listed files from the dataset's owner"));
        }
        Ok(())
    }

    fn title(&self, dataset: &Dataset) -> Result<Option<String>, Error> {
        self.metadata_blocks
            .citation
            .as_ref()
            .and_then(|citation| citation.fields.iter().find(|field| field.name == "title"))
            .map(|field| {
                field
                    .value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| dataset.invalid("the title is not text"))
            })
            .transpose()
    }
}

impl File {
    fn path(&self) -> String {
        let name = self
            .data
            .original_file_name
            .as_deref()
            .unwrap_or(&self.data.filename);
        match self
            .directory_label
            .as_deref()
            .filter(|folder| !folder.is_empty())
        {
            Some(folder) => format!("{folder}/{name}"),
            None => name.to_owned(),
        }
    }

    fn remote(self, dataset: &Dataset) -> Result<Remote, Error> {
        let path = self.path();
        let size = if self.data.tabular_data || self.data.original_file_name.is_some() {
            match (&self.data.original_file_name, self.data.original_file_size) {
                (Some(_), Some(size)) => Some(size),
                _ => {
                    return Err(dataset.invalid(&format!(
                        "the original name or size for \"{path}\" is missing"
                    )));
                }
            }
        } else {
            self.data.filesize
        };
        let (sha256, md5) = match self.data.checksum {
            Some(Checksum::Md5 { value }) => (None, Some(value)),
            Some(Checksum::Sha256 { value }) => (Some(value), None),
            Some(Checksum::Other) | None => (None, None),
        };
        Ok(Remote {
            path,
            size,
            sha256,
            md5,
            url: dataset.file_url(self.data.id)?,
            named_by_server: false,
        })
    }
}
