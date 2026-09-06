//! Searching the registries that offer search, and what a name resolved to.

use std::sync::Arc;

use fetchloom_engine::credential::Credential;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::work::WorkCounter;

use crate::http::{HttpSource, Method, header};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Registry {
    HuggingFace,
    Kaggle,
    OpenMl,
    Zenodo,
    Figshare,
    Ckan,
    Dataverse,
    DataCite,
}

impl Registry {
    pub const ALL: [Self; 8] = [
        Self::HuggingFace,
        Self::Kaggle,
        Self::OpenMl,
        Self::Zenodo,
        Self::Figshare,
        Self::Ckan,
        Self::Dataverse,
        Self::DataCite,
    ];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::HuggingFace => "Hugging Face",
            Self::Kaggle => "Kaggle",
            Self::OpenMl => "OpenML",
            Self::Zenodo => "Zenodo",
            Self::Figshare => "Figshare",
            Self::Ckan => "CKAN",
            Self::Dataverse => "Dataverse",
            Self::DataCite => "DataCite",
        }
    }

    #[must_use]
    pub fn host(self) -> &'static str {
        match self {
            Self::HuggingFace => "huggingface.co",
            Self::Kaggle => "www.kaggle.com",
            Self::OpenMl => "www.openml.org",
            Self::Zenodo => "zenodo.org",
            Self::Figshare => "api.figshare.com",
            Self::Ckan => "data.humdata.org",
            Self::Dataverse => "dataverse.harvard.edu",
            Self::DataCite => "api.datacite.org",
        }
    }

    fn origin(self) -> &'static str {
        match self {
            Self::HuggingFace => "https://huggingface.co",
            Self::Kaggle => "https://www.kaggle.com",
            Self::OpenMl => "https://www.openml.org",
            Self::Zenodo => "https://zenodo.org",
            Self::Figshare => "https://api.figshare.com",
            Self::Ckan => "https://data.humdata.org",
            Self::Dataverse => "https://dataverse.harvard.edu",
            Self::DataCite => "https://api.datacite.org",
        }
    }

    fn asking(self, origin: &str, term: &str) -> String {
        let escaped = escaped(term);
        match self {
            Self::HuggingFace => format!("{origin}/api/datasets?search={escaped}&limit=20"),
            Self::Kaggle => format!("{origin}/api/v1/datasets/list?search={escaped}"),
            Self::OpenMl => format!("{origin}/api/v1/json/data/list/data_name/{escaped}/limit/20"),
            Self::Zenodo => format!("{origin}/api/records?q={escaped}&size=20"),
            Self::Figshare => format!("{origin}/v2/articles/search"),
            Self::Ckan => format!("{origin}/api/3/action/package_search?q={escaped}&rows=20"),
            Self::Dataverse => {
                format!("{origin}/api/search?q={escaped}&type=dataset&per_page=20")
            }
            Self::DataCite => format!("{origin}/dois?query={escaped}&page%5Bsize%5D=20"),
        }
    }

    fn posts(self) -> bool {
        self == Self::Figshare
    }

    fn is_no_match(self, status: u16) -> bool {
        self == Self::OpenMl && status == 412
    }
}

const HEXADECIMAL: [char; 16] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C', 'D', 'E', 'F',
];

fn escaped(term: &str) -> String {
    let mut written = String::new();
    for byte in term.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            written.push(char::from(byte));
        } else {
            written.push('%');
            written.push(HEXADECIMAL[usize::from(byte >> 4)]);
            written.push(HEXADECIMAL[usize::from(byte & 0x0f)]);
        }
    }
    written
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub registry: Registry,
    pub reference: String,
    pub name: String,
    pub title: String,
    pub size: Option<u64>,
}

fn malformed(endpoint: &str) -> Error {
    Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "name the reference instead of the name, because {} answered a search this build cannot read",
            SafeUrl::new(endpoint)
        ),
    )
    .with_source(endpoint)
}

pub struct Searcher {
    registry: Registry,
    http: HttpSource,
    origin: String,
}

impl Searcher {
    #[must_use]
    pub fn new(registry: Registry, limits: Limits, work: Arc<WorkCounter>) -> Self {
        Self {
            registry,
            http: HttpSource::new(limits, work),
            origin: registry.origin().to_owned(),
        }
    }

    #[must_use]
    pub fn reaching(
        registry: Registry,
        origin: impl Into<String>,
        limits: Limits,
        work: Arc<WorkCounter>,
    ) -> Self {
        Self {
            registry,
            http: HttpSource::new(limits, work),
            origin: origin.into(),
        }
    }

    /// # Errors
    /// `network.status` when the registry refuses or rate limits the search,
    /// the kinds a request gives, and `reference.unresolved` when the answer is
    /// not the shape the registry states.
    pub fn search(&self, term: &str, credential: Option<&Credential>) -> Result<Vec<Found>, Error> {
        let endpoint = self.registry.asking(&self.origin, term);
        let (answer, _served) = if self.registry.posts() {
            self.http.send_json(
                &endpoint,
                &format!("{{\"search_for\":{},\"page_size\":20}}", quoted(term)),
                credential,
            )?
        } else {
            self.http.send(Method::Get, &endpoint, None, credential)?
        };
        let status = answer.status().as_u16();
        if self.registry.is_no_match(status) {
            return Ok(Vec::new());
        }
        if !(200..300).contains(&status) {
            return Err(crate::http::status_failure(
                &endpoint,
                status,
                header(&answer, "retry-after").as_deref(),
                credential,
            ));
        }
        let limits = self.http.limits();
        let body = answer
            .into_body()
            .into_with_config()
            .limit(limits.listing_bytes)
            .read_to_string()
            .map_err(|reason| crate::http::transport_failure(&endpoint, &reason))?;
        let document: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| malformed(&endpoint))?;
        self.read(&document, &endpoint)
    }

    fn read(&self, document: &serde_json::Value, endpoint: &str) -> Result<Vec<Found>, Error> {
        let records = self.records(document).ok_or_else(|| malformed(endpoint))?;
        Ok(records
            .iter()
            .filter_map(|record| self.found(record))
            .collect())
    }

    fn records<'a>(&self, document: &'a serde_json::Value) -> Option<Vec<&'a serde_json::Value>> {
        let held = match self.registry {
            Registry::HuggingFace | Registry::Kaggle | Registry::Figshare => document.as_array()?,
            Registry::OpenMl => document.get("data")?.get("dataset")?.as_array()?,
            Registry::Zenodo => document.get("hits")?.get("hits")?.as_array()?,
            Registry::Ckan => document.get("result")?.get("results")?.as_array()?,
            Registry::Dataverse => document.get("data")?.get("items")?.as_array()?,
            Registry::DataCite => document.get("data")?.as_array()?,
        };
        Some(held.iter().collect())
    }

    fn host_segment(&self) -> String {
        self.origin
            .split_once("://")
            .map_or(self.origin.as_str(), |(_, rest)| rest)
            .trim_end_matches('/')
            .to_owned()
    }

    fn found(&self, record: &serde_json::Value) -> Option<Found> {
        let text = |named: &str| record.get(named)?.as_str().map(str::to_owned);
        let number = |named: &str| record.get(named)?.as_u64();
        let (reference, name, title) = match self.registry {
            Registry::HuggingFace => {
                let identifier = text("id")?;
                let name = identifier.rsplit('/').next()?.to_owned();
                (
                    format!("hf:datasets/{identifier}"),
                    name.clone(),
                    text("title").unwrap_or(name),
                )
            }
            Registry::Kaggle => {
                let identifier = text("ref")?;
                let name = identifier.rsplit('/').next()?.to_owned();
                (
                    format!("kaggle:{identifier}"),
                    name.clone(),
                    text("title").unwrap_or(name),
                )
            }
            Registry::OpenMl => {
                let name = text("name")?;
                (
                    format!("openml:{}", number("did")?),
                    name.clone(),
                    name.clone(),
                )
            }
            Registry::Zenodo => {
                let doi = text("doi").or_else(|| {
                    record
                        .get("metadata")
                        .and_then(|held| held.get("doi"))
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                })?;
                let title = text("title").unwrap_or_else(|| doi.clone());
                (format!("zenodo:{doi}"), title.clone(), title)
            }
            Registry::Figshare => {
                let title = text("title")?;
                (format!("figshare:{}", number("id")?), title.clone(), title)
            }
            Registry::Ckan => {
                let name = text("name")?;
                (
                    format!("ckan:{}/{name}", self.host_segment()),
                    name.clone(),
                    text("title").unwrap_or(name),
                )
            }
            Registry::Dataverse => {
                let identifier = text("global_id")?;
                let title = text("name").unwrap_or_else(|| identifier.clone());
                (
                    format!("dataverse:{}/{identifier}", self.host_segment()),
                    title.clone(),
                    title,
                )
            }
            Registry::DataCite => {
                let attributes = record.get("attributes")?;
                let doi = attributes.get("doi")?.as_str()?.to_owned();
                let landing = attributes.get("url")?.as_str()?;
                let title = attributes
                    .get("titles")
                    .and_then(|held| held.get(0))
                    .and_then(|held| held.get("title"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&doi)
                    .to_owned();
                (crate::doi::routed(&doi, landing)?, title.clone(), title)
            }
        };
        Some(Found {
            registry: self.registry,
            reference,
            name,
            title,
            size: number("totalBytes").or_else(|| number("size")),
        })
    }
}

fn quoted(term: &str) -> String {
    let mut written = String::from("\"");
    for letter in term.chars() {
        match letter {
            '"' => written.push_str("\\\""),
            '\\' => written.push_str("\\\\"),
            control if control.is_control() => {
                let point = control as u32;
                written.push_str("\\u");
                for shift in [12_u32, 8, 4, 0] {
                    let nibble = usize::try_from((point >> shift) & 0x0f).unwrap_or_default();
                    written.push(HEXADECIMAL[nibble].to_ascii_lowercase());
                }
            }
            other => written.push(other),
        }
    }
    written.push('"');
    written
}

#[cfg(test)]
mod tests {
    use super::{escaped, quoted};

    #[test]
    fn a_term_carrying_a_separator_cannot_add_a_parameter_of_its_own() {
        assert_eq!(escaped("a b&limit=1#"), "a%20b%26limit%3D1%23");
        assert_eq!(escaped("ham10000"), "ham10000");
        assert_eq!(escaped("a/b"), "a%2Fb");
    }

    #[test]
    fn a_term_carrying_a_quote_cannot_close_the_document_it_is_written_into() {
        assert_eq!(quoted("a\"b"), "\"a\\\"b\"");
        assert_eq!(quoted("a\\b"), "\"a\\\\b\"");
        assert_eq!(quoted("a\nb"), "\"a\\u000ab\"");
    }
}
