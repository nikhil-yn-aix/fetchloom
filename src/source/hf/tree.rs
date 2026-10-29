//! the files of a repository at one commit, read from the tree API page by page.

use serde::Deserialize;

use super::Repo;
use super::listing::hub;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::source::{MOST_FILES, Remote};

/// the most pages followed, so a server that keeps sending pages cannot keep fetchloom busy
/// forever. the hub sends 1,000 entries a page.
const MOST_PAGES: usize = 1_000;

/// one entry of a tree listing: a file or a folder.
#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    size: Option<u64>,
    lfs: Option<Lfs>,
}

/// the LFS pointer of a large file, whose id is the SHA-256 of its bytes.
#[derive(Deserialize)]
struct Lfs {
    oid: String,
}

impl Repo {
    /// every file in the tree at `commit`, following the pages the hub sends.
    pub(super) async fn tree(&self, api: &impl Api, commit: &str) -> Result<Vec<Remote>, Error> {
        let mut first = self.api(&["tree", commit])?;
        first.query_pairs_mut().append_pair("recursive", "true");
        let mut next = Some(first);
        let mut files = Vec::new();
        let mut pages = 0;
        while let Some(url) = next {
            pages += 1;
            if pages > MOST_PAGES {
                return Err(Error::new(
                    Kind::SourceInvalid,
                    format!("huggingface.co sent more than {MOST_PAGES} pages listing {self}"),
                ));
            }
            let response = api.get(&url).await?;
            let entries: Vec<Entry> = crate::source::json(self, &response)?;
            for entry in entries.into_iter().filter(|entry| entry.kind == "file") {
                files.push(self.remote(commit, entry)?);
            }
            if files.len() > MOST_FILES {
                return Err(Error::new(
                    Kind::SourceInvalid,
                    format!("{self} holds more than {MOST_FILES} files, more than fetchloom handles in one dataset"),
                )
                .help("ask the owner to pack the files into archives, or pick a smaller repository"));
            }
            next = response.next;
        }
        Ok(files)
    }

    /// one file of the tree, downloaded through `/resolve/<commit>/<path>`.
    fn remote(&self, commit: &str, entry: Entry) -> Result<Remote, Error> {
        let sha256 = match &entry.lfs {
            None => None,
            Some(lfs) => Some(Sha256::from_hex(&lfs.oid).ok_or_else(|| {
                Error::new(
                    Kind::SourceInvalid,
                    format!(
                        "huggingface.co gave {} in {self} an LFS id that is not a SHA-256",
                        entry.path
                    ),
                )
            })?),
        };
        let mut segments = self.page_segments();
        segments.extend(["resolve", commit]);
        segments.extend(entry.path.split('/'));
        let url = hub(&segments)?;
        Ok(Remote {
            path: entry.path,
            size: entry.size,
            sha256,
            md5: None,
            url,
            named_by_server: false,
        })
    }
}
