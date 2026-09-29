//! The project manifest, `data.toml`: what a project needs, written by people.

use std::collections::BTreeMap;
use std::fmt;
use std::ops::Range;

use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};

use crate::canonical::Encoder;
use crate::digest::Digest;
use crate::error::ErrorKind;
use crate::filter::{FilterError, Filters, Pattern, Sample};
use crate::name::Name;
use crate::path::DataPath;
use crate::reference::Reference;
use crate::toml_error::explain_error;
use crate::units::Size;

/// A project manifest as read from `data.toml`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The `[project]` table.
    #[serde(default)]
    pub project: Project,
    /// The datasets, by name.
    #[serde(default)]
    pub datasets: BTreeMap<Name, DatasetSpec>,
    /// The steps, by name.
    #[serde(default)]
    pub steps: BTreeMap<Name, StepSpec>,
    /// The `[tiers]` table.
    #[serde(default)]
    pub tiers: Tiers,
    /// The `[publish]` table, when present.
    pub publish: Option<Publish>,
}

/// The `[project]` table.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// The project's name.
    pub name: Option<String>,
    /// Where datasets land, relative to the project root; `data` by default.
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: None,
            data_dir: default_data_dir(),
        }
    }
}

fn default_data_dir() -> String {
    "data".to_owned()
}

/// One `[datasets.NAME]` entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetSpec {
    /// Where the data comes from.
    #[serde(rename = "ref")]
    pub reference: Reference,
    /// Globs of files to keep; empty keeps every file.
    #[serde(default)]
    pub select: Vec<Pattern>,
    /// Globs of files to drop.
    #[serde(default)]
    pub exclude: Vec<Pattern>,
    /// Source specific filters, applied by the source.
    #[serde(default)]
    pub facets: BTreeMap<String, Vec<String>>,
    /// Files larger than this are skipped.
    pub max_file_size: Option<Size>,
    /// A deterministic sample of the files.
    pub sample: Option<Sample>,
    /// When archives are unpacked.
    #[serde(default)]
    pub extract: Extract,
    /// How files are placed in the project.
    #[serde(default)]
    pub link: Link,
    /// Where the dataset lands under the data directory; its name by default.
    pub path: Option<DataPath>,
}

impl DatasetSpec {
    /// The hash of this entry, stored in the lock as `spec`.
    ///
    /// It covers every key in a fixed encoding, so formatting and comments never change it and
    /// any change of value does.
    #[must_use]
    pub fn spec(&self) -> Digest {
        let mut encoder = Encoder::new("fetchloom spec v1");
        encoder.str(self.reference.as_str());
        for patterns in [&self.select, &self.exclude] {
            encoder.u64(patterns.len() as u64);
            for pattern in patterns {
                encoder.str(pattern.as_str());
            }
        }
        encoder.u64(self.facets.len() as u64);
        for (facet, values) in &self.facets {
            encoder.str(facet).u64(values.len() as u64);
            for value in values {
                encoder.str(value);
            }
        }
        match self.max_file_size {
            None => encoder.u64(0),
            Some(size) => encoder.u64(1).u64(size.bytes()),
        };
        match self.sample {
            None => encoder.u64(0),
            Some(Sample::Count { n, seed }) => encoder.u64(1).u64(n).u64(seed),
            Some(Sample::Fraction { fraction, seed }) => {
                encoder.u64(2).u64(fraction.to_bits()).u64(seed)
            }
        };
        encoder.u64(self.extract as u64).u64(self.link as u64);
        match &self.path {
            None => encoder.u64(0),
            Some(path) => encoder.u64(1).str(path.as_str()),
        };
        Digest::Blake3(encoder.finish())
    }

    /// Compiles this entry's select, exclude, size limit and sample.
    ///
    /// # Errors
    ///
    /// Returns [`FilterError`] when the patterns cannot be combined.
    pub fn filters(&self) -> Result<Filters, FilterError> {
        Filters::new(
            self.select.clone(),
            self.exclude.clone(),
            self.max_file_size,
            self.sample,
        )
    }
}

/// When archives in a dataset are unpacked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Extract {
    /// Unpack when the reference resolves to exactly one archive.
    #[default]
    Auto,
    /// Unpack every archive.
    Yes,
    /// Never unpack.
    No,
}

/// How files of a dataset are placed in the project.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Link {
    /// The fastest safe link the filesystem allows.
    #[default]
    Auto,
    /// Real copies, for tools that open files for writing.
    Copy,
}

/// One `[steps.NAME]` entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepSpec {
    /// The command.
    pub run: Run,
    /// Dataset names, step names, and project paths or globs the step reads.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// `dataset:glob` or `step:glob`: one run per match.
    pub foreach: Option<String>,
    /// Variables whose values are part of the step key.
    #[serde(default)]
    pub env: Vec<String>,
    /// The shell for a string `run`.
    pub shell: Option<String>,
    /// The working directory relative to the project root.
    pub workdir: Option<String>,
}

/// A step's command.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Run {
    /// A program and its arguments, run directly.
    Argv(Vec<String>),
    /// A command line run through the shell.
    Shell(String),
}

impl<'de> Deserialize<'de> for Run {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(RunVisitor)
    }
}

struct RunVisitor;

const NO_PROGRAM: &str = "`run` needs a program to run";

impl<'de> Visitor<'de> for RunVisitor {
    type Value = Run;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a command string or an array of arguments")
    }

    fn visit_str<E: de::Error>(self, command: &str) -> Result<Run, E> {
        if command.is_empty() {
            return Err(E::custom(NO_PROGRAM));
        }
        Ok(Run::Shell(command.to_owned()))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Run, A::Error> {
        let mut argv = Vec::new();
        while let Some(arg) = seq.next_element::<String>()? {
            argv.push(arg);
        }
        if argv.is_empty() {
            return Err(de::Error::custom(NO_PROGRAM));
        }
        Ok(Run::Argv(argv))
    }
}

/// The `[tiers]` table.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tiers {
    /// Remote tiers this project shares, as URLs.
    #[serde(default)]
    pub remote: Vec<String>,
}

/// The `[publish]` table.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publish {
    /// The title of the published record.
    pub title: Option<String>,
    /// Its creators.
    #[serde(default)]
    pub creators: Vec<Creator>,
    /// Its license, as SPDX.
    pub license: Option<String>,
    /// Its keywords.
    #[serde(default)]
    pub keywords: Vec<String>,
}

/// A creator of a published record.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Creator {
    /// The name, as `Family, Given`.
    pub name: String,
    /// The ORCID.
    pub orcid: Option<String>,
}

impl Manifest {
    /// Reads and validates `data.toml` text. A leading byte order mark and CRLF line endings are
    /// accepted.
    ///
    /// # Errors
    ///
    /// Returns [`ManifestError`] for invalid TOML, an unknown key (naming the closest valid one),
    /// a value of the wrong form, a name used by both a dataset and a step, or a `foreach` that
    /// is not `name:glob` over a known dataset or step.
    pub fn from_toml(text: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(text).map_err(|err| {
            let explained = explain_error(&err);
            ManifestError {
                message: explained.message,
                span: explained.span,
                help: explained.help,
            }
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if let Some(name) = self
            .steps
            .keys()
            .find(|name| self.datasets.contains_key(*name))
        {
            return Err(ManifestError::plain(format!(
                "`{name}` names both a dataset and a step"
            )));
        }
        for step in self.steps.values() {
            if let Some(foreach) = &step.foreach {
                self.check_foreach(foreach)?;
            }
        }
        Ok(())
    }

    fn check_foreach(&self, foreach: &str) -> Result<(), ManifestError> {
        let (source, glob) = foreach.split_once(':').ok_or_else(|| {
            ManifestError::plain(format!(
                "`foreach = \"{foreach}\"` needs a dataset or step name, a colon and a glob"
            ))
        })?;
        let known = self.datasets.keys().chain(self.steps.keys());
        if !known.map(Name::as_str).any(|name| name == source) {
            return Err(ManifestError::plain(format!(
                "`foreach` names `{source}`, which is not a dataset or step"
            )));
        }
        glob.parse::<Pattern>()
            .map_err(|err| ManifestError::plain(err.to_string()))?;
        Ok(())
    }
}

/// Why `data.toml` could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ManifestError {
    /// What is wrong, in one sentence.
    pub message: String,
    /// The byte range of `data.toml` it points at, when known.
    pub span: Option<Range<usize>>,
    /// What may fix it, such as the closest valid key.
    pub help: Option<String>,
}

impl ManifestError {
    /// The kind this error is reported as.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        ErrorKind::ProjectInvalidManifest
    }

    fn plain(message: String) -> Self {
        Self {
            message,
            span: None,
            help: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reference::Target;

    const EXAMPLE: &str = r#"[project]
name = "motor-imagery"
data_dir = "data"

[datasets.eeg]
ref = "openneuro:ds003061@1.1.0"
select = ["**/*.edf", "participants.tsv"]
exclude = ["**/derivatives/**"]
facets = { subject = ["01", "02", "03"] }
max_file_size = "2 GiB"
sample = { n = 20, seed = 1 }
extract = "auto"
link = "auto"

[datasets.grch38]
ref = "ensembl:homo_sapiens/GRCh38@110"
select = ["*dna.primary_assembly.fa.gz"]

[steps.filter]
foreach = "eeg:sub-*/"
run = ["python", "scripts/filter.py", "{item}", "{out}"]
inputs = ["scripts/filter.py", "uv.lock"]
env = ["FILTER_BAND"]

[steps.features]
run = "duckdb -c \"COPY (SELECT * FROM '{in.filter}/**/*.parquet') TO '{out}/features.parquet'\""
inputs = ["filter"]

[tiers]
remote = ["s3://lab-bucket/fetchloom"]

[publish]
title = "Filtered motor imagery EEG"
creators = [{ name = "Okafor, A.", orcid = "0000-0002-1825-0097" }]
license = "CC-BY-4.0"
keywords = ["eeg", "motor imagery"]
"#;

    fn name(text: &str) -> Name {
        Name::new(text).unwrap()
    }

    fn read(text: &str) -> Manifest {
        Manifest::from_toml(text).unwrap()
    }

    fn refused(text: &str) -> ManifestError {
        Manifest::from_toml(text).unwrap_err()
    }

    #[test]
    fn reads_every_key_of_the_documented_example() {
        let manifest = read(EXAMPLE);
        assert_eq!(manifest.project.name.as_deref(), Some("motor-imagery"));
        assert_eq!(manifest.project.data_dir, "data");
        let eeg = &manifest.datasets[&name("eeg")];
        assert!(
            matches!(eeg.reference.target(), Target::Scheme { scheme, .. } if scheme == "openneuro")
        );
        let select: Vec<&str> = eeg.select.iter().map(Pattern::as_str).collect();
        assert_eq!(select, ["**/*.edf", "participants.tsv"]);
        assert_eq!(eeg.exclude[0].as_str(), "**/derivatives/**");
        assert_eq!(eeg.facets["subject"], ["01", "02", "03"]);
        assert_eq!(eeg.max_file_size.unwrap().bytes(), 2 << 30);
        assert_eq!(eeg.sample, Some(Sample::Count { n: 20, seed: 1 }));
        assert_eq!(eeg.extract, Extract::Auto);
        assert_eq!(eeg.link, Link::Auto);
        assert!(eeg.path.is_none());
        let filter = &manifest.steps[&name("filter")];
        assert_eq!(filter.foreach.as_deref(), Some("eeg:sub-*/"));
        assert_eq!(
            filter.run,
            Run::Argv(vec![
                "python".into(),
                "scripts/filter.py".into(),
                "{item}".into(),
                "{out}".into()
            ])
        );
        assert_eq!(filter.inputs, ["scripts/filter.py", "uv.lock"]);
        assert_eq!(filter.env, ["FILTER_BAND"]);
        let features = &manifest.steps[&name("features")];
        assert!(matches!(&features.run, Run::Shell(command) if command.starts_with("duckdb -c")));
        assert_eq!(manifest.tiers.remote, ["s3://lab-bucket/fetchloom"]);
        let publish = manifest.publish.unwrap();
        assert_eq!(publish.title.as_deref(), Some("Filtered motor imagery EEG"));
        assert_eq!(publish.creators[0].name, "Okafor, A.");
        assert_eq!(
            publish.creators[0].orcid.as_deref(),
            Some("0000-0002-1825-0097")
        );
        assert_eq!(publish.license.as_deref(), Some("CC-BY-4.0"));
        assert_eq!(publish.keywords, ["eeg", "motor imagery"]);
    }

    #[test]
    fn an_empty_file_is_an_empty_project() {
        let manifest = read("");
        assert!(manifest.project.name.is_none());
        assert_eq!(manifest.project.data_dir, "data");
        assert!(manifest.datasets.is_empty());
        assert!(manifest.steps.is_empty());
        assert!(manifest.tiers.remote.is_empty());
        assert!(manifest.publish.is_none());
    }

    #[test]
    fn defaults_fill_what_an_entry_leaves_out() {
        let manifest = read("[datasets.x]\nref = \"zenodo:1\"\n");
        let x = &manifest.datasets[&name("x")];
        assert!(x.select.is_empty() && x.exclude.is_empty() && x.facets.is_empty());
        assert_eq!((x.extract, x.link), (Extract::Auto, Link::Auto));
        let manifest = read(
            "[datasets.x]\nref = \"zenodo:1\"\nextract = \"no\"\nlink = \"copy\"\npath = \"raw/x\"\nsample = { fraction = 0.5 }\n",
        );
        let x = &manifest.datasets[&name("x")];
        assert_eq!((x.extract, x.link), (Extract::No, Link::Copy));
        assert_eq!(x.path.as_ref().unwrap().as_str(), "raw/x");
        assert_eq!(
            x.sample,
            Some(Sample::Fraction {
                fraction: 0.5,
                seed: 0
            })
        );
    }

    #[test]
    fn accepts_a_byte_order_mark_and_crlf_line_endings() {
        let text = format!("\u{feff}{}", EXAMPLE.replace('\n', "\r\n"));
        assert_eq!(read(&text).datasets.len(), 2);
    }

    #[test]
    fn unknown_keys_name_the_closest_valid_key_and_point_at_it() {
        let cases = [
            ("[projct]\n", "projct", "project"),
            ("[project]\nnme = \"a\"\n", "nme", "name"),
            (
                "[datasets.x]\nref = \"zenodo:1\"\nselct = []\n",
                "selct",
                "select",
            ),
            (
                "[datasets.x]\nref = \"zenodo:1\"\nsample = { n = 1, sed = 2 }\n",
                "sed",
                "seed",
            ),
            ("[steps.s]\nrun = \"x\"\ninput = []\n", "input", "inputs"),
            ("[tiers]\nremotes = []\n", "remotes", "remote"),
            (
                "[publish]\ncreators = [{ name = \"a\", orcd = \"b\" }]\n",
                "orcd",
                "orcid",
            ),
        ];
        for (text, key, closest) in cases {
            let err = refused(text);
            assert_eq!(err.to_string(), format!("unknown key `{key}`"), "{text}");
            assert_eq!(
                err.help.as_deref(),
                Some(format!("the closest valid key is `{closest}`").as_str())
            );
            assert_eq!(&text[err.span.clone().unwrap()], key);
        }
    }

    #[test]
    fn refuses_bad_values_with_their_reason() {
        let cases = [
            ("[datasets.x]\n", "missing key `ref`"),
            (
                "[datasets.EEG]\nref = \"zenodo:1\"\n",
                "name `EEG` may hold only a-z, 0-9, `-` and `_`",
            ),
            (
                "[datasets.x]\nref = \"zenodo:\"\n",
                "reference `zenodo:` has nothing after `zenodo:`",
            ),
            (
                "[datasets.x]\nref = \"zenodo:1\"\nselect = [\"a[\"]\n",
                "`a[` is not a valid glob: unclosed character class; missing ']'",
            ),
            (
                "[datasets.x]\nref = \"zenodo:1\"\nmax_file_size = \"2 G\"\n",
                "`2 G` is not a size like `2 GiB` or `500 MB`",
            ),
            (
                "[datasets.x]\nref = \"zenodo:1\"\nextract = \"maybe\"\n",
                "unknown variant `maybe`, expected one of `auto`, `yes`, `no`",
            ),
            (
                "[datasets.x]\nref = \"zenodo:1\"\npath = \"../x\"\n",
                "dataset path `../x` has a `..` component",
            ),
            (
                "[datasets.x]\nref = \"zenodo:1\"\nsample = { n = 1, fraction = 0.5 }\n",
                "a sample takes either `n` or `fraction`, not both",
            ),
            (
                "[datasets.x]\nref = \"zenodo:1\"\nsample = { seed = 1 }\n",
                "a sample needs `n` or `fraction`",
            ),
            (
                "[datasets.x]\nref = \"zenodo:1\"\nsample = { n = 0 }\n",
                "a sample needs at least one file",
            ),
            (
                "[datasets.x]\nref = \"zenodo:1\"\nsample = { fraction = 2.0 }\n",
                "a sample fraction must be above 0 and at most 1, got 2",
            ),
            ("[steps.s]\nrun = []\n", "`run` needs a program to run"),
            ("[steps.s]\nrun = \"\"\n", "`run` needs a program to run"),
            (
                "[steps.s]\nrun = 3\n",
                "invalid type: integer `3`, expected a command string or an array of arguments",
            ),
            (
                "[steps.s]\nrun = [\"a\", 3]\n",
                "invalid type: integer `3`, expected a string",
            ),
            ("name = 1\n", "unknown key `name`"),
            ("[project]\nname = \"a\"\nname = \"b\"\n", "duplicate key"),
        ];
        for (text, message) in cases {
            let err = refused(text);
            assert_eq!(err.to_string(), message, "{text}");
            assert!(err.span.is_some(), "{text}");
        }
    }

    #[test]
    fn refuses_what_only_the_whole_file_can_show() {
        let cases = [
            (
                "[datasets.a]\nref = \"zenodo:1\"\n[steps.a]\nrun = \"x\"\n",
                "`a` names both a dataset and a step",
            ),
            (
                "[steps.s]\nrun = \"x\"\nforeach = \"eeg\"\n",
                "`foreach = \"eeg\"` needs a dataset or step name, a colon and a glob",
            ),
            (
                "[steps.s]\nrun = \"x\"\nforeach = \"eeg:sub-*/\"\n",
                "`foreach` names `eeg`, which is not a dataset or step",
            ),
            (
                "[datasets.eeg]\nref = \"zenodo:1\"\n[steps.s]\nrun = \"x\"\nforeach = \"eeg:a[\"\n",
                "`a[` is not a valid glob: unclosed character class; missing ']'",
            ),
        ];
        for (text, message) in cases {
            assert_eq!(refused(text).to_string(), message, "{text}");
        }
        let ok = "[datasets.eeg]\nref = \"zenodo:1\"\n[steps.a]\nrun = \"x\"\nforeach = \"eeg:*\"\n[steps.b]\nrun = \"x\"\nforeach = \"a:*\"\n";
        assert_eq!(read(ok).steps.len(), 2);
    }

    fn spec_of(text: &str) -> Digest {
        read(text).datasets[&name("x")].spec()
    }

    #[test]
    fn spec_ignores_formatting_and_comments() {
        let a = "[datasets.x]\nref = \"zenodo:1\"\nselect = [\"a\", \"b\"]\n";
        let b = "# data\n[datasets.x]   # the x\nselect = [ \"a\",\n  \"b\" ]\nref='zenodo:1'\n";
        assert_eq!(spec_of(a), spec_of(b));
        assert_eq!(
            spec_of(a).to_string(),
            "blake3:352b30074d5882be5a59758904a342afc00e87db8aedc5839453fe2750d73e3a"
        );
    }

    #[test]
    fn spec_changes_with_every_key() {
        let base = "[datasets.x]\nref = \"zenodo:1\"\n";
        let variants = [
            "ref = \"zenodo:2\"\n",
            "ref = \"zenodo:1\"\nselect = [\"a\"]\n",
            "ref = \"zenodo:1\"\nselect = [\"a\", \"b\"]\n",
            "ref = \"zenodo:1\"\nselect = [\"b\", \"a\"]\n",
            "ref = \"zenodo:1\"\nexclude = [\"a\"]\n",
            "ref = \"zenodo:1\"\nfacets = { s = [\"1\"] }\n",
            "ref = \"zenodo:1\"\nfacets = { s = [\"1\", \"2\"] }\n",
            "ref = \"zenodo:1\"\nfacets = { t = [\"1\"] }\n",
            "ref = \"zenodo:1\"\nmax_file_size = 10\n",
            "ref = \"zenodo:1\"\nmax_file_size = 11\n",
            "ref = \"zenodo:1\"\nsample = { n = 1 }\n",
            "ref = \"zenodo:1\"\nsample = { n = 1, seed = 1 }\n",
            "ref = \"zenodo:1\"\nsample = { n = 2 }\n",
            "ref = \"zenodo:1\"\nsample = { fraction = 0.5 }\n",
            "ref = \"zenodo:1\"\nsample = { fraction = 0.25 }\n",
            "ref = \"zenodo:1\"\nextract = \"yes\"\n",
            "ref = \"zenodo:1\"\nextract = \"no\"\n",
            "ref = \"zenodo:1\"\nlink = \"copy\"\n",
            "ref = \"zenodo:1\"\npath = \"x\"\n",
            "ref = \"zenodo:1\"\npath = \"y\"\n",
        ];
        let mut specs = vec![spec_of(base)];
        for variant in variants {
            specs.push(spec_of(&format!("[datasets.x]\n{variant}")));
        }
        for (i, a) in specs.iter().enumerate() {
            for b in &specs[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn filters_are_built_from_the_entry() {
        let manifest = read(EXAMPLE);
        let filters = manifest.datasets[&name("grch38")].filters().unwrap();
        let files = [
            (
                DataPath::new("homo.dna.primary_assembly.fa.gz").unwrap(),
                Some(1),
            ),
            (DataPath::new("x.gtf").unwrap(), Some(1)),
        ];
        let selection = filters.apply(files.iter().map(|(p, s)| (p, *s)));
        assert_eq!(selection.kept(), [0]);
    }
}
