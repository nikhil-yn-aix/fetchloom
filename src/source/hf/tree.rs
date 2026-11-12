//! the files of a repository at one commit, read from the tree API page by page.

use serde::Deserialize;
use url::Url;

use super::Repo;
use super::listing::hub;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::source::{Remote, most_files, too_many};

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
    /// every file in the tree at `commit`, or only those in `folders` when every file wanted lies
    /// in one of them: a repository of many folders then takes a few requests rather than one for
    /// each thousand files it holds. a folder the hub does not have, or one without files, has
    /// the whole tree listed instead, so a pattern that matches nothing is told against all the
    /// repository holds.
    pub(super) async fn tree(
        &self,
        api: &impl Api,
        commit: &str,
        folders: Option<&[String]>,
    ) -> Result<(Vec<Remote>, Vec<String>), Error> {
        if let Some(folders) = folders {
            let mut files = Vec::new();
            let mut each = true;
            for folder in folders {
                let mut segments = vec!["tree", commit];
                segments.extend(folder.split('/'));
                let before = files.len();
                if !self
                    .pages(api, commit, self.api(&segments)?, true, &mut files)
                    .await?
                    || files.len() == before
                {
                    each = false;
                    break;
                }
            }
            if each {
                return Ok((files, folders.to_vec()));
            }
        }
        let mut files = Vec::new();
        self.pages(api, commit, self.api(&["tree", commit])?, false, &mut files)
            .await?;
        Ok((files, Vec::new()))
    }

    /// adds the files of the listing that starts at `first` to `files`, following the pages the
    /// hub sends. false when the listing is of a `folder` the hub does not have.
    async fn pages(
        &self,
        api: &impl Api,
        commit: &str,
        mut first: Url,
        folder: bool,
        files: &mut Vec<Remote>,
    ) -> Result<bool, Error> {
        first.query_pairs_mut().append_pair("recursive", "true");
        let mut next = Some(first);
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
            if folder && pages == 1 && response.status == 404 {
                return Ok(false);
            }
            let entries: Vec<Entry> = crate::source::json(self, &response)?;
            for entry in entries.into_iter().filter(|entry| entry.kind == "file") {
                files.push(self.remote(commit, entry)?);
            }
            if files.len() > most_files() {
                return Err(too_many(self, "select the folders you need with --select"));
            }
            next = response.next;
        }
        Ok(true)
    }

    /// one file of the tree, downloaded through `/resolve/<commit>/<path>`.
    fn remote(&self, commit: &str, entry: Entry) -> Result<Remote, Error> {
        let sha256 = match &entry.lfs {
            None => None,
            Some(lfs) => Some(Sha256::from_hex(&lfs.oid).ok_or_else(|| {
                Error::new(
                    Kind::SourceInvalid,
                    format!(
                        "huggingface.co gave {} in {self} a SHA-256 that is not valid",
                        entry.path
                    ),
                )
                .help("ask the dataset's owner to check that file's SHA-256 on huggingface.co")
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
            etag: None,
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
        let (files, within) = block_on(repo.tree(&recorded, "abc123", None)).unwrap();
        assert_eq!(within, Vec::<String>::new());
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
            let error = block_on(repo.tree(&recorded, "abc123", None)).unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid);
            assert!(error.message.contains("weights.bin"));
            assert!(error.message.contains("SHA-256"));
            assert!(error.help.unwrap().contains("that file's SHA-256"));
        }
    }

    #[test]
    fn cyclic_pagination_stops_before_requesting_page_one_thousand_and_one() {
        let recorded = Recorded::default().on(FIRST, 200, "[]").next(FIRST, FIRST);
        let repo = Repo::parse("datasets/org/data").unwrap();
        let error = block_on(repo.tree(&recorded, "abc123", None)).unwrap_err();
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
        let (files, within) = block_on(repo.tree(&recorded, "abc123", None)).unwrap();
        assert_eq!(within, Vec::<String>::new());
        assert_eq!(files.len(), 100_000);
        assert_eq!(files[99_999].path, "99999.bin");
        drop(files);
        let recorded = recorded.next(FIRST, SECOND).on(
            SECOND,
            200,
            r#"[{"type":"file","path":"one-too-many.bin"}]"#,
        );
        let error = block_on(repo.tree(&recorded, "abc123", None)).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(error.message.contains("100000 files"));
        assert!(error.help.unwrap().contains("FETCHLOOM_MOST_FILES"));
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
            let error = block_on(repo.tree(&recorded, "abc123", None)).unwrap_err();
            assert_eq!(error.kind, kind);
            assert_eq!(recorded.asked(), [FIRST, SECOND]);
        }
    }

    const COLA: &str =
        "https://huggingface.co/api/datasets/org/data/tree/abc123/cola?recursive=true";
    const QQP: &str =
        "https://huggingface.co/api/datasets/org/data/tree/abc123/glue/qqp?recursive=true";

    fn folders(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn folders_holding_every_wanted_file_are_listed_alone() {
        let recorded = Recorded::default()
            .on(COLA, 200, r#"[{"type":"file","path":"cola/train.parquet","size":3}]"#)
            .on(QQP, 200, r#"[{"type":"directory","path":"glue/qqp/x"},{"type":"file","path":"glue/qqp/x/test.parquet","size":4}]"#);
        let repo = Repo::parse("datasets/org/data").unwrap();
        let (files, within) =
            block_on(repo.tree(&recorded, "abc123", Some(&folders(&["cola", "glue/qqp"]))))
                .unwrap();
        assert_eq!(within, ["cola", "glue/qqp"]);
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["cola/train.parquet", "glue/qqp/x/test.parquet"]);
        assert_eq!(
            files[0].url.as_str(),
            "https://huggingface.co/datasets/org/data/resolve/abc123/cola/train.parquet"
        );
        assert_eq!(recorded.asked(), [COLA, QQP]);
    }

    #[test]
    fn a_folder_missing_or_without_files_lists_the_whole_tree() {
        let whole = r#"[{"type":"file","path":"Cola/train.parquet"},{"type":"file","path":"qqp/test.parquet"}]"#;
        for (status, body) in [
            (404, r#"{"error":"cola does not exist on \"abc123\""}"#),
            (200, r#"[{"type":"directory","path":"cola/empty"}]"#),
        ] {
            let recorded = Recorded::default()
                .on(COLA, status, body)
                .on(FIRST, 200, whole);
            let repo = Repo::parse("datasets/org/data").unwrap();
            let (files, within) =
                block_on(repo.tree(&recorded, "abc123", Some(&folders(&["cola"])))).unwrap();
            assert_eq!(files.len(), 2, "{status}");
            assert_eq!(within, Vec::<String>::new(), "{status}");
            assert_eq!(recorded.asked(), [COLA, FIRST], "{status}");
        }
    }

    #[test]
    fn a_folder_name_is_encoded_as_path_segments() {
        const ODD: &str = "https://huggingface.co/api/datasets/org/data/tree/abc123/odd%20%5B1%5D/a%23b?recursive=true";
        let recorded =
            Recorded::default().on(ODD, 200, r#"[{"type":"file","path":"odd [1]/a#b/x.csv"}]"#);
        let repo = Repo::parse("datasets/org/data").unwrap();
        let (files, _) =
            block_on(repo.tree(&recorded, "abc123", Some(&folders(&["odd [1]/a#b"])))).unwrap();
        assert_eq!(files[0].path, "odd [1]/a#b/x.csv");
        assert_eq!(recorded.asked(), [ODD]);
    }
}
