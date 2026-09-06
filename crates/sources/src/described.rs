//! The six providers described against the one listing shape.

use std::sync::Arc;

use fetchloom_engine::limits::Limits;
use fetchloom_engine::work::WorkCounter;

use crate::listing::{
    Described, DescribedSource, Field, Held, Identifying, Naming, Reaching, Record, Stated,
};

fn kaggle_records(record: &Record) -> String {
    format!(
        "{}/api/v1/datasets/list/{}",
        record.origin, record.identifier
    )
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "the shape Naming::Building takes, where a builder that reads a field the record may omit answers None"
)]
fn kaggle_file(record: &Record, name: &str, _file: &serde_json::Value) -> Option<String> {
    Some(format!(
        "{}/api/v1/datasets/download/{}/{name}",
        record.origin, record.identifier
    ))
}

static KAGGLE: Described = Described {
    scheme: "kaggle",
    reaching: Reaching::Fixed("https://www.kaggle.com"),
    identifying: Identifying::Segments(2),
    records: kaggle_records,
    held: Held::Each(Field(&["datasetFiles"])),
    naming: Naming::Building {
        name: Field(&["name"]),
        build: kaggle_file,
    },
    folder: None,
    sized: Field(&["totalBytes"]),
    stated: Stated::Nothing,
};

fn openml_records(record: &Record) -> String {
    format!("{}/api/v1/json/data/{}", record.origin, record.identifier)
}

static OPENML: Described = Described {
    scheme: "openml",
    reaching: Reaching::Fixed("https://www.openml.org"),
    identifying: Identifying::Segments(1),
    records: openml_records,
    held: Held::One(Field(&["data_set_description"])),
    naming: Naming::FromLocation {
        location: Field(&["url"]),
    },
    folder: None,
    sized: Field(&["file_size"]),
    stated: Stated::Nothing,
};

fn github_records(record: &Record) -> String {
    match &record.revision {
        Some(tag) => format!(
            "{}/repos/{}/releases/tags/{tag}",
            record.origin, record.identifier
        ),
        None => format!(
            "{}/repos/{}/releases/latest",
            record.origin, record.identifier
        ),
    }
}

static GITHUB: Described = Described {
    scheme: "github",
    reaching: Reaching::Fixed("https://api.github.com"),
    identifying: Identifying::Segments(2),
    records: github_records,
    held: Held::Each(Field(&["assets"])),
    naming: Naming::Both {
        name: Field(&["name"]),
        location: Field(&["browser_download_url"]),
    },
    folder: None,
    sized: Field(&["size"]),
    stated: Stated::Prefixed(Field(&["digest"])),
};

fn figshare_records(record: &Record) -> String {
    format!("{}/v2/articles/{}", record.origin, record.identifier)
}

static FIGSHARE: Described = Described {
    scheme: "figshare",
    reaching: Reaching::Fixed("https://api.figshare.com"),
    identifying: Identifying::Segments(1),
    records: figshare_records,
    held: Held::Each(Field(&["files"])),
    naming: Naming::Both {
        name: Field(&["name"]),
        location: Field(&["download_url"]),
    },
    folder: None,
    sized: Field(&["size"]),
    stated: Stated::Nothing,
};

fn ckan_records(record: &Record) -> String {
    format!(
        "{}/api/3/action/package_show?id={}",
        record.origin, record.identifier
    )
}

static CKAN: Described = Described {
    scheme: "ckan",
    reaching: Reaching::Named,
    identifying: Identifying::Segments(1),
    records: ckan_records,
    held: Held::Each(Field(&["result", "resources"])),
    naming: Naming::FromLocation {
        location: Field(&["url"]),
    },
    folder: None,
    sized: Field(&["size"]),
    stated: Stated::Prefixed(Field(&["hash"])),
};

fn dataverse_records(record: &Record) -> String {
    format!(
        "{}/api/datasets/:persistentId/versions/:latest/files?persistentId={}",
        record.origin, record.identifier
    )
}

fn dataverse_file(record: &Record, _name: &str, file: &serde_json::Value) -> Option<String> {
    let id = file.get("dataFile")?.get("id")?.as_u64()?;
    Some(format!("{}/api/access/datafile/{id}", record.origin))
}

static DATAVERSE: Described = Described {
    scheme: "dataverse",
    reaching: Reaching::Named,
    identifying: Identifying::Peeled,
    records: dataverse_records,
    held: Held::Each(Field(&["data"])),
    naming: Naming::Building {
        name: Field(&["dataFile", "filename"]),
        build: dataverse_file,
    },
    folder: Some(Field(&["directoryLabel"])),
    sized: Field(&["dataFile", "filesize"]),
    stated: Stated::Labelled {
        algorithm: Field(&["dataFile", "checksum", "type"]),
        value: Field(&["dataFile", "checksum", "value"]),
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Kaggle,
    OpenMl,
    GitHubReleases,
    Figshare,
    Ckan,
    Dataverse,
}

impl Provider {
    pub const ALL: [Self; 6] = [
        Self::Kaggle,
        Self::OpenMl,
        Self::GitHubReleases,
        Self::Figshare,
        Self::Ckan,
        Self::Dataverse,
    ];

    fn described(self) -> &'static Described {
        match self {
            Self::Kaggle => &KAGGLE,
            Self::OpenMl => &OPENML,
            Self::GitHubReleases => &GITHUB,
            Self::Figshare => &FIGSHARE,
            Self::Ckan => &CKAN,
            Self::Dataverse => &DATAVERSE,
        }
    }
}

#[must_use]
pub fn described(provider: Provider, limits: Limits, work: Arc<WorkCounter>) -> DescribedSource {
    DescribedSource::new(provider.described(), limits, work)
}

#[must_use]
pub fn described_reaching(
    provider: Provider,
    origin: impl Into<String>,
    limits: Limits,
    work: Arc<WorkCounter>,
) -> DescribedSource {
    DescribedSource::reaching(provider.described(), origin, limits, work)
}
