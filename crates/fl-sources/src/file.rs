//! Local files and directories: a `file://` URL or a path, listed without reading a byte.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fl_model::path::DataPath;
use fl_model::reference::{Reference, Target};
use fl_transfer::url::SafeUrl;
use futures_util::future::BoxFuture;
use url::Url;

use crate::listing::{Entry, Listing, Meta};
use crate::source::{Source, SourceError};

/// Serves `file://` URLs and local paths. A file is one entry; a directory is every file
/// below it, in name order, symbolic links left out.
#[derive(Clone, Debug)]
pub struct Files {
    schemes: Vec<String>,
    base: PathBuf,
}

impl Files {
    /// The adapter for local files, reading a relative path against `base`.
    #[must_use]
    pub fn new(base: PathBuf) -> Self {
        Self {
            schemes: vec!["file".to_owned()],
            base,
        }
    }

    fn listing(&self, reference: &Reference) -> Result<Listing, SourceError> {
        let raw = reference.as_str();
        let invalid = |reason: String| SourceError::Invalid {
            reference: raw.to_owned(),
            reason,
        };
        let path = match reference.target() {
            Target::File { .. } => Url::parse(raw)
                .ok()
                .and_then(|url| url.to_file_path().ok())
                .ok_or_else(|| invalid("it names no local path".to_owned()))?,
            Target::Path(path) => self.base.join(path),
            _ => {
                return Err(SourceError::Unresolved {
                    reference: raw.to_owned(),
                });
            }
        };
        let root = std::path::absolute(&path).map_err(|error| invalid(error.to_string()))?;
        let metadata = match fs::metadata(&root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(SourceError::Unresolved {
                    reference: raw.to_owned(),
                });
            }
            Err(error) => return Err(local(&root, &error)),
        };
        let mut entries = Vec::new();
        if metadata.is_dir() {
            walk(&root, "", &mut entries)?;
            entries.sort_by(|a, b| a.path.as_str().cmp(b.path.as_str()));
        } else {
            let name = root
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| invalid("the file name is not valid text".to_owned()))?;
            entries.push(entry(&root, name, metadata.len())?);
        }
        Ok(Listing {
            resolved: reference.clone(),
            meta: Meta::default(),
            entries,
        })
    }
}

impl Source for Files {
    fn schemes(&self) -> &[String] {
        &self.schemes
    }

    fn resolve<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> BoxFuture<'a, Result<Listing, SourceError>> {
        Box::pin(std::future::ready(self.listing(reference)))
    }
}

fn walk(dir: &Path, prefix: &str, entries: &mut Vec<Entry>) -> Result<(), SourceError> {
    let mut children: Vec<_> = fs::read_dir(dir)
        .map_err(|error| local(dir, &error))?
        .collect::<Result<_, _>>()
        .map_err(|error| local(dir, &error))?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        let path = child.path();
        let kind = child.file_type().map_err(|error| local(&path, &error))?;
        let name = child.file_name();
        let name = name.to_str().ok_or_else(|| SourceError::Local {
            path: path.clone(),
            reason: "its name is not valid text".to_owned(),
        })?;
        let inside = format!("{prefix}{name}");
        if kind.is_dir() {
            walk(&path, &format!("{inside}/"), entries)?;
        } else if kind.is_file() {
            let size = child
                .metadata()
                .map_err(|error| local(&path, &error))?
                .len();
            entries.push(entry(&path, &inside, size)?);
        }
    }
    Ok(())
}

fn entry(path: &Path, inside: &str, size: u64) -> Result<Entry, SourceError> {
    let fail = |reason: String| SourceError::Local {
        path: path.to_owned(),
        reason,
    };
    let url = Url::from_file_path(path)
        .ok()
        .and_then(|url| SafeUrl::parse(url.as_str()).ok())
        .ok_or_else(|| fail("it has no file URL".to_owned()))?;
    Ok(Entry {
        path: DataPath::normalize(inside).map_err(|error| fail(error.to_string()))?,
        size: Some(size),
        stated: Vec::new(),
        urls: vec![url],
    })
}

fn local(path: &Path, error: &io::Error) -> SourceError {
    SourceError::Local {
        path: path.to_owned(),
        reason: error.to_string(),
    }
}
