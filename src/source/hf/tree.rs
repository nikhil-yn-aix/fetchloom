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
                )
                .help("ask the dataset's owner to pack the files into archives, or use a smaller dataset"));
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
                        "huggingface.co gave {} in {self} a file fingerprint that is not a SHA-256",
                        entry.path
                    ),
                )
                .help("ask the dataset's owner to check that file's fingerprint on huggingface.co")
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

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;
    use crate::source::recorded::{Recorded, block_on};

    const FIRST: &str = "https://huggingface.co/api/datasets/org/data/tree/abc123?recursive=true";
    const SECOND: &str = "https://huggingface.co/api/datasets/org/data/tree/abc123?cursor=next";

    #[test]
    fn files_keep_their_metadata_and_encoded_paths_across_pages() {
        let recorded = Recorded::default()
            .on(FIRST, 200, r#"[{"type":"directory","path":"folder"},{"type":"file","path":"folder/a #?.csv","size":0}]"#)
            .next(FIRST, SECOND)
            .on(SECOND, 200, r#"[{"type":"file","path":"weights.bin","lfs":{"oid":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}}]"#);
        let repo = Repo::parse("datasets/org/data").unwrap();
        let files = block_on(repo.tree(&recorded, "abc123")).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "folder/a #?.csv");
        assert_eq!(files[0].size, Some(0));
        assert_eq!(files[0].sha256, None);
        assert_eq!(
            files[0].url.as_str(),
            "https://huggingface.co/datasets/org/data/resolve/abc123/folder/a%20%23%3F.csv"
        );
        assert_eq!(files[1].size, None);
        assert_eq!(
            files[1].sha256.unwrap().to_string(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(files[1].md5, None);
        assert!(!files[1].named_by_server);
        assert_eq!(recorded.asked(), [FIRST, SECOND]);
    }

    #[test]
    fn malformed_lfs_fingerprints_refuse_the_listing_with_a_repair_hint() {
        for fingerprint in [
            "abc",
            "g123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ] {
            let body = serde_json::json!([{
                "type": "file", "path": "weights.bin", "lfs": {"oid": fingerprint}
            }]);
            let recorded = Recorded::default().on(FIRST, 200, body.to_string());
            let repo = Repo::parse("datasets/org/data").unwrap();
            let error = block_on(repo.tree(&recorded, "abc123")).unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid);
            assert!(error.message.contains("weights.bin"));
            assert!(error.message.contains("SHA-256"));
            assert!(error.help.unwrap().contains("fingerprint"));
        }
    }

    #[test]
    fn cyclic_pagination_stops_before_requesting_page_one_thousand_and_one() {
        let recorded = Recorded::default().on(FIRST, 200, "[]").next(FIRST, FIRST);
        let repo = Repo::parse("datasets/org/data").unwrap();
        let error = block_on(repo.tree(&recorded, "abc123")).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(error.message.contains("1000 pages"));
        assert!(error.help.unwrap().contains("smaller dataset"));
        assert_eq!(recorded.asked().len(), 1_000);
    }

    #[test]
    fn the_file_limit_counts_across_pages_and_allows_the_boundary() {
        let mut body = String::from("[");
        for index in 0..100_000 {
            if index != 0 {
                body.push(',');
            }
            write!(body, r#"{{"type":"file","path":"{index}.bin"}}"#).unwrap();
        }
        body.push(']');
        let recorded = Recorded::default().on(FIRST, 200, body);
        let repo = Repo::parse("datasets/org/data").unwrap();
        let files = block_on(repo.tree(&recorded, "abc123")).unwrap();
        assert_eq!(files.len(), 100_000);
        assert_eq!(files[99_999].path, "99999.bin");
        drop(files);
        let recorded = recorded.next(FIRST, SECOND).on(
            SECOND,
            200,
            r#"[{"type":"file","path":"one-too-many.bin"}]"#,
        );
        let error = block_on(repo.tree(&recorded, "abc123")).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(error.message.contains("100000 files"));
        assert!(error.help.unwrap().contains("archives"));
        assert_eq!(recorded.asked(), [FIRST, FIRST, SECOND]);
    }

    #[test]
    fn malformed_entries_and_failed_later_pages_do_not_return_partial_trees() {
        for (status, body, kind) in [
            (200, r#"[{"type":"file"}]"#, Kind::SourceInvalid),
            (
                200,
                r#"[{"type":"file","path":"x","size":"large"}]"#,
                Kind::SourceInvalid,
            ),
            (403, "not json", Kind::AuthRequired),
            (404, "not json", Kind::ReferenceUnresolved),
        ] {
            let recorded = Recorded::default()
                .on(FIRST, 200, r#"[{"type":"file","path":"good.bin"}]"#)
                .next(FIRST, SECOND)
                .on(SECOND, status, body);
            let repo = Repo::parse("datasets/org/data").unwrap();
            let error = block_on(repo.tree(&recorded, "abc123")).unwrap_err();
            assert_eq!(error.kind, kind);
            assert_eq!(recorded.asked(), [FIRST, SECOND]);
        }
    }
}
