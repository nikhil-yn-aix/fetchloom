//! The datasets a project file names, and where each one lands.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use fetchloom_engine::selection::Layout;

use serde::Deserialize;
use serde::de::{self, MapAccess, Visitor};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct DescribedDataset {
    #[serde(rename = "ref")]
    pub reference: String,
    pub output: Option<PathBuf>,
    #[serde(default)]
    pub select: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    pub layout: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatasetEntry(pub DescribedDataset);

impl<'de> Deserialize<'de> for DatasetEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EitherShape;

        impl<'de> Visitor<'de> for EitherShape {
            type Value = DatasetEntry;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a reference, or a table stating ref")
            }

            fn visit_str<E: de::Error>(self, reference: &str) -> Result<Self::Value, E> {
                Ok(DatasetEntry(DescribedDataset {
                    reference: reference.to_owned(),
                    ..DescribedDataset::default()
                }))
            }

            fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<Self::Value, M::Error> {
                DescribedDataset::deserialize(de::value::MapAccessDeserializer::new(map))
                    .map(DatasetEntry)
            }
        }

        deserializer.deserialize_any(EitherShape)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectDataset {
    pub name: String,
    pub reference: String,
    pub destination: PathBuf,
    pub select: Vec<String>,
    pub exclude: Vec<String>,
    pub layout: Option<Layout>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectError {
    Layout {
        name: String,
        stated: String,
        reason: String,
    },
    NoDatasets {
        path: PathBuf,
    },
    OneDestination {
        first: String,
        second: String,
        destination: PathBuf,
    },
    Unrepresentable {
        name: String,
        destination: PathBuf,
    },
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDatasets { path } => write!(
                f,
                "write a datasets table in {} naming what to fetch, because get with no reference \
                 fetches what a project file states and that one states none",
                path.display()
            ),
            Self::OneDestination {
                first,
                second,
                destination,
            } => write!(
                f,
                "give {first} and {second} destinations of their own, because both would write to \
                 {}",
                destination.display()
            ),
            Self::Layout {
                name,
                stated,
                reason,
            } => write!(
                f,
                "write a layout {name} can take, because {stated} is not one: {reason}"
            ),
            Self::Unrepresentable { name, destination } => write!(
                f,
                "give {name} a destination this platform can name, because {} has no absolute form \
                 here",
                destination.display()
            ),
        }
    }
}

impl std::error::Error for ProjectError {}

#[must_use]
pub fn without_navigation(path: &Path) -> PathBuf {
    let mut built = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !built.pop() {
                    built.push("..");
                }
            }
            other => built.push(other.as_os_str()),
        }
    }
    built
}

fn stated_layout(name: &str, stated: Option<&str>) -> Result<Option<Layout>, ProjectError> {
    let Some(stated) = stated else {
        return Ok(None);
    };
    stated
        .parse::<Layout>()
        .map(Some)
        .map_err(|reason| ProjectError::Layout {
            name: name.to_owned(),
            stated: stated.to_owned(),
            reason,
        })
}

fn one_destination_key(path: &Path) -> String {
    let text = without_navigation(path).to_string_lossy().into_owned();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

/// # Errors
/// `ProjectError::NoDatasets` when the file names none,
/// `ProjectError::OneDestination` when two entries would write to one place,
/// and `ProjectError::Unrepresentable` when a destination has no absolute form
/// on this platform.
pub fn datasets_of(
    beside: &Path,
    named: &BTreeMap<String, DatasetEntry>,
    project_file: &Path,
) -> Result<Vec<ProjectDataset>, ProjectError> {
    if named.is_empty() {
        return Err(ProjectError::NoDatasets {
            path: project_file.to_path_buf(),
        });
    }
    let mut taken: BTreeMap<String, String> = BTreeMap::new();
    let mut datasets = Vec::with_capacity(named.len());
    for (name, DatasetEntry(described)) in named {
        let stated = described
            .output
            .clone()
            .unwrap_or_else(|| PathBuf::from(name));
        let destination = std::path::absolute(beside.join(&stated)).map_err(|_| {
            ProjectError::Unrepresentable {
                name: name.clone(),
                destination: stated.clone(),
            }
        })?;
        let key = one_destination_key(&destination);
        if let Some(first) = taken.get(&key) {
            return Err(ProjectError::OneDestination {
                first: first.clone(),
                second: name.clone(),
                destination: without_navigation(&destination),
            });
        }
        taken.insert(key, name.clone());
        datasets.push(ProjectDataset {
            name: name.clone(),
            reference: described.reference.clone(),
            destination: without_navigation(&destination),
            select: described.select.clone(),
            exclude: described.exclude.clone(),
            layout: stated_layout(name, described.layout.as_deref())?,
        });
    }
    Ok(datasets)
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a failure to build or read the input is the assertion"
    )]

    use super::{DatasetEntry, ProjectError, datasets_of, without_navigation};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn parsed(text: &str) -> BTreeMap<String, DatasetEntry> {
        toml::from_str(text).expect("the table parses")
    }

    #[test]
    fn a_bare_string_is_the_reference_and_the_name_is_the_destination() {
        let named = parsed("corpus = \"https://lab.edu/corpus.zip\"");
        let datasets = datasets_of(
            Path::new("/work"),
            &named,
            Path::new("/work/fetchloom.toml"),
        )
        .unwrap();
        assert_eq!(datasets.len(), 1);
        assert_eq!(datasets[0].reference, "https://lab.edu/corpus.zip");
        assert_eq!(
            datasets[0].destination,
            without_navigation(&std::path::absolute(PathBuf::from("/work/corpus")).unwrap())
        );
    }

    #[test]
    fn a_table_states_its_own_output_and_selection() {
        let named = parsed(
            "[corpus]\nref = \"https://lab.edu/corpus.zip\"\noutput = \"data/corpus\"\nselect = [\"train/*\"]\n",
        );
        let datasets = datasets_of(
            Path::new("/work"),
            &named,
            Path::new("/work/fetchloom.toml"),
        )
        .unwrap();
        assert_eq!(datasets[0].select, vec!["train/*".to_owned()]);
        assert!(
            datasets[0].destination.ends_with("data/corpus")
                || datasets[0].destination.ends_with("data\\corpus"),
            "{}",
            datasets[0].destination.display()
        );
    }

    #[test]
    fn an_unknown_key_inside_a_table_names_the_key() {
        let refused = toml::from_str::<BTreeMap<String, DatasetEntry>>(
            "[corpus]\nref = \"https://lab.edu/c.zip\"\nselct = [\"train/*\"]\n",
        )
        .expect_err("an unknown key is refused");
        assert!(refused.to_string().contains("selct"), "{refused}");
    }

    #[test]
    fn a_table_with_no_ref_is_refused() {
        let refused =
            toml::from_str::<BTreeMap<String, DatasetEntry>>("[corpus]\noutput = \"here\"\n")
                .expect_err("a table with no ref is refused");
        assert!(refused.to_string().contains("ref"), "{refused}");
    }

    #[test]
    fn two_entries_writing_to_one_place_name_both() {
        let named = parsed(
            "a = { ref = \"https://lab.edu/a.zip\", output = \"out\" }\nb = { ref = \"https://lab.edu/b.zip\", output = \"./out\" }\n",
        );
        let refused = datasets_of(
            Path::new("/work"),
            &named,
            Path::new("/work/fetchloom.toml"),
        )
        .expect_err("two entries writing to one place are refused");
        let ProjectError::OneDestination { first, second, .. } = &refused else {
            panic!("{refused}");
        };
        assert_eq!((first.as_str(), second.as_str()), ("a", "b"));
    }

    #[test]
    fn a_destination_climbing_back_to_the_same_place_is_one_destination() {
        let named = parsed(
            "a = { ref = \"https://lab.edu/a.zip\", output = \"out\" }\nb = { ref = \"https://lab.edu/b.zip\", output = \"nested/../out\" }\n",
        );
        assert!(
            datasets_of(
                Path::new("/work"),
                &named,
                Path::new("/work/fetchloom.toml")
            )
            .is_err(),
            "two spellings of one destination were taken for two destinations"
        );
    }

    #[test]
    fn a_layout_no_run_can_take_is_refused_before_anything_is_fetched() {
        let named = parsed(
            "a = { ref = \"https://lab.edu/a.zip\" }
b = { ref = \"https://lab.edu/b.zip\", layout = \"sideways\" }
",
        );
        let refused = datasets_of(
            Path::new("/work"),
            &named,
            Path::new("/work/fetchloom.toml"),
        )
        .expect_err("a layout no run can take is refused");
        let ProjectError::Layout { name, stated, .. } = &refused else {
            panic!("{refused}");
        };
        assert_eq!((name.as_str(), stated.as_str()), ("b", "sideways"));
    }

    #[test]
    fn a_file_naming_no_dataset_says_so() {
        let refused = datasets_of(
            Path::new("/work"),
            &BTreeMap::new(),
            Path::new("/work/fetchloom.toml"),
        )
        .expect_err("a file naming no dataset is refused");
        assert!(matches!(refused, ProjectError::NoDatasets { .. }));
    }
}
