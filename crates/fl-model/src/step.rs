//! Step keys: what a step's result depends on, hashed so a result is reused only when nothing
//! it depends on changed.

use std::collections::{BTreeMap, BTreeSet};

use crate::canonical::Encoder;
use crate::digest::Digest;
use crate::manifest::Run;
use crate::name::Name;
use crate::path::DataPath;

/// Everything a step's key covers. Time, host and outputs are never part of it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StepKey {
    /// The command as written.
    pub run: Run,
    /// The `shell` key as written.
    pub shell: Option<String>,
    /// The `workdir` key as written.
    pub workdir: Option<String>,
    /// The `foreach` key as written.
    pub foreach: Option<String>,
    /// The inputs with their hashes; the order they were declared in never matters.
    pub inputs: BTreeSet<StepInput>,
    /// The variables named in `env` and their values, `None` when unset, which differs from empty.
    pub env: BTreeMap<String, Option<String>>,
    /// For a `foreach` run, the path of the item and its hash.
    pub item: Option<(DataPath, Digest)>,
}

/// One input of a step and the hash that stands for its content.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum StepInput {
    /// A dataset and its tree hash.
    Dataset {
        /// The dataset's name.
        name: Name,
        /// Its tree hash.
        tree: Digest,
    },
    /// Another step and the tree hash of its output.
    Step {
        /// The step's name.
        name: Name,
        /// The tree hash of its output.
        tree: Digest,
    },
    /// A project file and its content hash.
    File {
        /// The path relative to the project root.
        path: DataPath,
        /// Its content hash.
        hash: Digest,
    },
}

impl StepKey {
    /// The key: BLAKE3 under the context `fetchloom step key v1` over every field, each tagged,
    /// inputs and variables in sorted order.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let mut encoder = Encoder::new("fetchloom step key v1");
        encoder.u64(1);
        match &self.run {
            Run::Argv(argv) => {
                encoder.u64(0).u64(argv.len() as u64);
                for arg in argv {
                    encoder.str(arg);
                }
            }
            Run::Shell(command) => {
                encoder.u64(1).str(command);
            }
        }
        for (tag, field) in [(2, &self.shell), (3, &self.workdir), (4, &self.foreach)] {
            encoder.u64(tag);
            optional(&mut encoder, field.as_deref());
        }
        encoder.u64(5).u64(self.inputs.len() as u64);
        for input in &self.inputs {
            let (kind, text, hash) = match input {
                StepInput::Dataset { name, tree } => (0, name.as_str(), tree),
                StepInput::Step { name, tree } => (1, name.as_str(), tree),
                StepInput::File { path, hash } => (2, path.as_str(), hash),
            };
            encoder.u64(kind).str(text);
            digest(&mut encoder, hash);
        }
        encoder.u64(6).u64(self.env.len() as u64);
        for (name, value) in &self.env {
            encoder.str(name);
            optional(&mut encoder, value.as_deref());
        }
        encoder.u64(7);
        match &self.item {
            None => {
                encoder.u64(0);
            }
            Some((path, hash)) => {
                encoder.u64(1).str(path.as_str());
                digest(&mut encoder, hash);
            }
        }
        Digest::Blake3(encoder.finish())
    }
}

fn optional(encoder: &mut Encoder, value: Option<&str>) {
    match value {
        None => encoder.u64(0),
        Some(text) => encoder.u64(1).str(text),
    };
}

fn digest(encoder: &mut Encoder, digest: &Digest) {
    encoder
        .str(digest.algorithm().name())
        .bytes(digest.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> Name {
        Name::new(text).unwrap()
    }

    fn path(text: &str) -> DataPath {
        DataPath::new(text).unwrap()
    }

    fn base() -> StepKey {
        StepKey {
            run: Run::Argv(vec!["python".to_owned(), "filter.py".to_owned()]),
            shell: None,
            workdir: None,
            foreach: Some("eeg:sub-*/".to_owned()),
            inputs: [
                StepInput::Dataset {
                    name: name("eeg"),
                    tree: Digest::Blake3([1; 32]),
                },
                StepInput::File {
                    path: path("filter.py"),
                    hash: Digest::Blake3([2; 32]),
                },
            ]
            .into(),
            env: [("BAND".to_owned(), Some("8-30".to_owned()))].into(),
            item: Some((path("sub-01"), Digest::Blake3([3; 32]))),
        }
    }

    #[test]
    fn is_pinned() {
        assert_eq!(
            base().digest().to_string(),
            "blake3:804cbeb9c2ef5aabc7c7ae064b4a36da766f9e376c144b51fea6401bdc3b10e8"
        );
    }

    #[test]
    fn changes_with_every_field() {
        let changes: Vec<fn(&mut StepKey)> = vec![
            |key| key.run = Run::Shell("python filter.py".to_owned()),
            |key| key.run = Run::Argv(vec!["python filter.py".to_owned()]),
            |key| {
                key.run = Run::Argv(vec![
                    "python".to_owned(),
                    "filter.py".to_owned(),
                    String::new(),
                ]);
            },
            |key| key.shell = Some("bash".to_owned()),
            |key| key.workdir = Some("src".to_owned()),
            |key| key.foreach = None,
            |key| key.foreach = Some(String::new()),
            |key| {
                key.inputs.insert(StepInput::Step {
                    name: name("clean"),
                    tree: Digest::Blake3([4; 32]),
                });
            },
            |key| {
                key.inputs.pop_first();
                key.inputs.insert(StepInput::Step {
                    name: name("eeg"),
                    tree: Digest::Blake3([1; 32]),
                });
            },
            |key| {
                key.inputs.pop_first();
                key.inputs.insert(StepInput::Dataset {
                    name: name("eeg"),
                    tree: Digest::Blake3([9; 32]),
                });
            },
            |key| {
                key.inputs.pop_last();
                key.inputs.insert(StepInput::File {
                    path: path("filter.py"),
                    hash: Digest::Sha256([2; 32]),
                });
            },
            |key| {
                key.env.insert("BAND".to_owned(), Some(String::new()));
            },
            |key| {
                key.env.insert("BAND".to_owned(), None);
            },
            |key| {
                key.env.insert("OTHER".to_owned(), None);
            },
            |key| key.item = None,
            |key| key.item = Some((path("sub-02"), Digest::Blake3([3; 32]))),
            |key| key.item = Some((path("sub-01"), Digest::Blake3([5; 32]))),
        ];
        let original = base().digest();
        let mut seen = BTreeSet::from([original]);
        for change in changes {
            let mut key = base();
            change(&mut key);
            assert!(seen.insert(key.digest()), "{key:?}");
        }
    }

    #[test]
    fn fields_never_run_into_each_other() {
        let mut shell = base();
        shell.shell = Some("x".to_owned());
        let mut workdir = base();
        workdir.workdir = Some("x".to_owned());
        assert_ne!(shell.digest(), workdir.digest());
    }
}
