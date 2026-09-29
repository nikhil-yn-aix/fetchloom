use std::fmt::{self, Display, Formatter, Write};

use toml_writer::{TomlStringBuilder, TomlWrite};

use super::{Lock, LockedDataset, LockedFile, LockedStep, Role};
use crate::hex;

/// Writes `lock` as `data.lock` text in the order its entries are held.
pub(super) fn to_toml(lock: &Lock) -> String {
    Written(lock).to_string()
}

struct Written<'a>(&'a Lock);

impl Display for Written<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str("version = 1\n")?;
        for dataset in &self.0.datasets {
            dataset_table(f, dataset)?;
        }
        for step in &self.0.steps {
            step_table(f, step)?;
        }
        Ok(())
    }
}

fn dataset_table(f: &mut Formatter<'_>, dataset: &LockedDataset) -> fmt::Result {
    f.write_str("\n[[dataset]]\n")?;
    text(f, "name", dataset.name.as_str())?;
    text(f, "ref", dataset.reference.as_str())?;
    writeln!(f, "spec = \"{}\"", dataset.spec)?;
    text(f, "resolved", dataset.resolved.as_str())?;
    for (key, value) in [
        ("title", &dataset.title),
        ("license", &dataset.license),
        ("doi", &dataset.doi),
    ] {
        if let Some(value) = value {
            text(f, key, value)?;
        }
    }
    writeln!(f, "retrieved = \"{}\"", dataset.retrieved)?;
    writeln!(f, "tree = \"{}\"", dataset.tree)?;
    match dataset.files.as_deref() {
        None => Ok(()),
        Some([]) => f.write_str("files = []\n"),
        Some(files) => {
            f.write_str("files = [\n")?;
            for file in files {
                file_row(f, file)?;
            }
            f.write_str("]\n")
        }
    }
}

fn file_row(f: &mut Formatter<'_>, file: &LockedFile) -> fmt::Result {
    f.write_str("  { path = ")?;
    string(f, file.path.as_str())?;
    write!(f, ", size = {}", file.size)?;
    digest(f, "blake3", &file.blake3)?;
    if let Some(bytes) = &file.sha256 {
        digest(f, "sha256", bytes)?;
    }
    if let Some(bytes) = &file.sha1 {
        digest(f, "sha1", bytes)?;
    }
    if let Some(bytes) = &file.md5 {
        digest(f, "md5", bytes)?;
    }
    if !file.at.is_empty() {
        f.write_str(", at = [")?;
        for (i, location) in file.at.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            string(f, location)?;
        }
        f.write_str("]")?;
    }
    if let Some(from) = &file.from {
        write!(f, ", from = \"{from}\"")?;
    }
    if let Some(Role::Archive) = file.role {
        f.write_str(", role = \"archive\"")?;
    }
    f.write_str(" },\n")
}

fn step_table(f: &mut Formatter<'_>, step: &LockedStep) -> fmt::Result {
    f.write_str("\n[[step]]\n")?;
    text(f, "name", step.name.as_str())?;
    writeln!(f, "key = \"{}\"", step.key)?;
    if let Some(items) = step.items {
        writeln!(f, "items = {items}")?;
    }
    writeln!(f, "tree = \"{}\"", step.tree)
}

fn text(f: &mut Formatter<'_>, key: &str, value: &str) -> fmt::Result {
    write!(f, "{key} = ")?;
    string(f, value)?;
    f.write_char('\n')
}

fn string(f: &mut Formatter<'_>, value: &str) -> fmt::Result {
    f.value(TomlStringBuilder::new(value).as_basic())
}

fn digest(f: &mut Formatter<'_>, key: &str, bytes: &[u8]) -> fmt::Result {
    write!(f, ", {key} = \"")?;
    hex::write(bytes, f)?;
    f.write_char('"')
}
