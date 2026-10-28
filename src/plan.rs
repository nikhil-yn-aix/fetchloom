//! where every locked file lands in the project, checked for clashes before anything is written.
//! names are compared the way a case-insensitive disk compares them, so a lock that places
//! cleanly here places cleanly on windows and macos too.

use std::collections::{HashMap, HashSet};

use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::name;
use crate::project::lock::Lock;
use crate::project::manifest::Manifest;

/// one file a lock places.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) dataset: String,
    /// the path in the project, with `/` between folders.
    pub(crate) path: String,
    pub(crate) sha256: Sha256,
    pub(crate) size: u64,
    /// a writable copy, for tools that write into their inputs.
    pub(crate) copy: bool,
}

/// the folder a dataset lands in, relative to the project.
pub(crate) fn folder(manifest: &Manifest, name: &str) -> String {
    let inner = manifest
        .datasets
        .get(name)
        .and_then(|entry| entry.path.as_deref())
        .unwrap_or(name);
    format!("{}/{inner}", manifest.data_dir)
}

/// every file the lock places for the datasets data.toml lists, or the first clash.
pub(crate) fn targets(manifest: &Manifest, lock: &Lock) -> Result<Vec<Target>, Error> {
    let mut targets = Vec::new();
    let mut folders = Vec::new();
    for dataset in &lock.datasets {
        let Some(entry) = manifest.datasets.get(&dataset.name) else {
            continue;
        };
        let folder = folder(manifest, &dataset.name);
        for file in &dataset.files {
            targets.push(Target {
                dataset: dataset.name.clone(),
                path: format!("{folder}/{}", file.path),
                sha256: file.sha256,
                size: file.size,
                copy: entry.copy,
            });
        }
        folders.push((folder, dataset.name.as_str()));
    }
    overlapping(&folders)?;
    clashing(&targets)?;
    Ok(targets)
}

/// how a case-insensitive disk sees a name.
fn key(path: &str) -> String {
    path.to_lowercase()
}

fn overlapping(folders: &[(String, &str)]) -> Result<(), Error> {
    for (index, (a, name_a)) in folders.iter().enumerate() {
        for (b, name_b) in &folders[index + 1..] {
            let (a_key, b_key) = (key(a), key(b));
            let inside = a_key == b_key
                || b_key.starts_with(&format!("{a_key}/"))
                || a_key.starts_with(&format!("{b_key}/"));
            if inside {
                return Err(Error::new(
                    Kind::ProjectInvalid,
                    format!(
                        "datasets {name_a} and {name_b} would share files: {a} and {b} overlap"
                    ),
                )
                .help("give one of them another path in data.toml"));
            }
        }
    }
    Ok(())
}

fn clashing(targets: &[Target]) -> Result<(), Error> {
    let mut files: HashMap<String, &Target> = HashMap::with_capacity(targets.len());
    let mut folders: HashMap<String, &Target> = HashMap::new();
    for target in targets {
        if let Some(reason) = name::unstorable(&target.path) {
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!("{} cannot be stored on this machine: {reason}", target.path),
            )
            .help(format!(
                "leave it out with an exclude pattern in datasets.{}",
                target.dataset
            )));
        }
        let path_key = key(&target.path);
        if let Some(other) = files.insert(path_key.clone(), target) {
            return Err(clash(&other.path, &target.path));
        }
        let mut parent = path_key.as_str();
        while let Some((folder, _)) = parent.rsplit_once('/') {
            folders.entry(folder.to_owned()).or_insert(target);
            parent = folder;
        }
    }
    let file_keys: HashSet<&String> = files.keys().collect();
    for (folder, inside) in &folders {
        if file_keys.contains(folder) {
            let file = files[folder];
            return Err(Error::new(
                Kind::ProjectInvalid,
                format!(
                    "{} is a file, but {} needs a folder there",
                    file.path, inside.path
                ),
            )
            .help(format!(
                "leave one of them out with an exclude pattern in datasets.{}",
                inside.dataset
            )));
        }
    }
    Ok(())
}

fn clash(first: &str, second: &str) -> Error {
    let message = if first == second {
        format!("two files would land on {first}")
    } else {
        format!("{first} and {second} would be the same file on a disk that ignores case")
    };
    Error::new(Kind::ProjectInvalid, message)
        .help("leave one of them out with an exclude pattern in data.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest;
    use crate::project::lock::{Locked, LockedFile};

    fn manifest(text: &str) -> Manifest {
        Manifest::parse(text).unwrap()
    }

    fn lock(datasets: &[(&str, &[&str])]) -> Lock {
        Lock {
            datasets: datasets
                .iter()
                .map(|(name, paths)| Locked {
                    name: (*name).to_owned(),
                    reference: "https://example.org/x".to_owned(),
                    spec: String::new(),
                    resolved: String::new(),
                    title: None,
                    license: None,
                    doi: None,
                    timestamp: String::new(),
                    files: paths
                        .iter()
                        .map(|path| LockedFile {
                            path: (*path).to_owned(),
                            size: 1,
                            sha256: digest::sha256(path.as_bytes()),
                            md5: None,
                            url: String::new(),
                            unpacks_to: None,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    const TWO: &str = "[datasets.a]\nref = \"x\"\n[datasets.b]\nref = \"y\"\npath = \"elsewhere/b\"\ncopy = true\n";

    #[test]
    fn files_land_in_their_dataset_folder() {
        let targets = targets(
            &manifest(TWO),
            &lock(&[("a", &["x.csv", "sub/y.csv"]), ("b", &["z.csv"])]),
        )
        .unwrap();
        let paths: Vec<&str> = targets.iter().map(|target| target.path.as_str()).collect();
        assert_eq!(
            paths,
            ["data/a/x.csv", "data/a/sub/y.csv", "data/elsewhere/b/z.csv"]
        );
        assert!(targets[2].copy && !targets[0].copy);
    }

    #[test]
    fn a_locked_dataset_missing_from_data_toml_places_nothing() {
        let only_a = manifest("[datasets.a]\nref = \"x\"\n");
        assert_eq!(
            targets(&only_a, &lock(&[("a", &["x"]), ("gone", &["y"])]))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn clashes_are_refused_before_anything_is_written() {
        let one = manifest("[datasets.a]\nref = \"x\"\n");
        assert!(
            targets(&one, &lock(&[("a", &["Readme.md", "README.md"])])).is_err(),
            "case-insensitive disks see one file"
        );
        assert!(
            targets(&one, &lock(&[("a", &["x", "x/y"])])).is_err(),
            "a file where a folder must be"
        );
        let nested =
            manifest("[datasets.a]\nref = \"x\"\n[datasets.b]\nref = \"y\"\npath = \"A/inner\"\n");
        assert!(
            targets(&nested, &lock(&[("a", &["x"]), ("b", &["y"])])).is_err(),
            "one dataset inside another"
        );
        assert_eq!(
            targets(&one, &lock(&[("a", &["2020-01-01T10:00:00.csv"])])).is_err(),
            cfg!(windows)
        );
    }
}
