//! a repository listed at one commit: the revision is resolved, access is checked when the
//! repository is gated, and the tree is read at that commit.

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use url::Url;

use super::{Repo, RepoKind};
use crate::error::{Error, Kind};
use crate::net::Api;
use crate::source::{Listing, Reference};

/// characters a path segment keeps as they are: the unreserved ones of RFC 3986.
const SEGMENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

impl Repo {
    /// lists the repository at one commit: the revision becomes a commit first, then the tree at
    /// that commit is read page by page.
    pub(in crate::source) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let info = self.revision(api).await?;
        let repo = Repo {
            kind: self.kind,
            id: info.id.clone(),
            revision: None,
        };
        if info.is_gated() {
            repo.check_access(api).await?;
        }
        let pinned = Repo {
            revision: Some(info.sha.clone()),
            ..repo
        };
        let files = pinned.tree(api, &info.sha).await?;
        Ok(Listing {
            resolved: Reference::HuggingFace(pinned),
            title: None,
            license: info.license(),
            doi: info.doi(),
            files,
        })
    }

    /// an API URL about this repository, such as `/api/datasets/org/name/revision/main`.
    pub(super) fn api(&self, rest: &[&str]) -> Result<Url, Error> {
        let mut segments = vec!["api", self.kind.plural()];
        segments.extend(self.id.split('/'));
        segments.extend(rest);
        hub(&segments)
    }

    /// the path of the repository's page on the website, where models have no kind before them.
    pub(super) fn page_segments(&self) -> Vec<&str> {
        let mut segments = match self.kind {
            RepoKind::Model => Vec::new(),
            kind => vec![kind.plural()],
        };
        segments.extend(self.id.split('/'));
        segments
    }

    /// the repository's page, where its terms are accepted.
    pub(super) fn page(&self) -> String {
        format!("https://huggingface.co/{}", self.page_segments().join("/"))
    }
}

/// a URL on huggingface.co whose path is these segments, each percent-encoded on its own.
pub(super) fn hub(segments: &[&str]) -> Result<Url, Error> {
    let mut text = String::from("https://huggingface.co");
    for segment in segments {
        text.push('/');
        text.extend(utf8_percent_encode(segment, SEGMENT));
    }
    Url::parse(&text).map_err(|err| {
        Error::new(
            Kind::SourceInvalid,
            format!("could not make a link to huggingface.co from {text}: {err}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::digest::Sha256;
    use crate::error::Kind;
    use crate::net::{Net, Options};
    use crate::source::Remote;
    use crate::source::hf::revision::is_commit;
    use crate::source::recorded::{Recorded, block_on, fixture_bytes};
    use crate::work::Work;

    const IMDB: &str = "e6281661ce1c48d982bc483cf8a173c1bbeb5d31";
    const FLEURS: &str = "70bb2e84b976b7e960aa89f1c648e09c59f894dd";
    const EXPAND: &str = "expand=sha&expand=gated&expand=tags";

    fn repo(text: &str) -> Repo {
        Repo::parse(text).unwrap()
    }

    fn api(path: &str) -> String {
        format!("https://huggingface.co/api/{path}")
    }

    /// the hub as it answered for stanfordnlp/imdb at its main branch.
    fn imdb() -> Recorded {
        Recorded::default()
            .fixture(
                &api(&format!("datasets/stanfordnlp/imdb/revision/main?{EXPAND}")),
                "hf/imdb-revision.json",
            )
            .fixture(
                &api(&format!(
                    "datasets/stanfordnlp/imdb/tree/{IMDB}?recursive=true"
                )),
                "hf/imdb-tree.json",
            )
    }

    #[test]
    fn a_small_dataset_is_listed_at_its_commit() {
        let recorded = imdb();
        let listing = block_on(repo("datasets/stanfordnlp/imdb").resolve(&recorded)).unwrap();
        assert_eq!(
            listing.resolved.to_string(),
            format!("hf:datasets/stanfordnlp/imdb@{IMDB}")
        );
        assert_eq!(listing.title, None);
        assert_eq!(listing.license.as_deref(), Some("other"));
        assert_eq!(listing.doi, None);
        let file = |path: &str, size, lfs: Option<&str>| Remote {
            path: path.to_owned(),
            size: Some(size),
            sha256: lfs.map(|hex| Sha256::from_hex(hex).unwrap()),
            md5: None,
            url: Url::parse(&format!(
                "https://huggingface.co/datasets/stanfordnlp/imdb/resolve/{IMDB}/{path}"
            ))
            .unwrap(),
            named_by_server: false,
        };
        assert_eq!(
            listing.files,
            [
                file(".gitattributes", 1174, None),
                file("README.md", 7809, None),
                file(
                    "plain_text/test-00000-of-00001.parquet",
                    20_470_363,
                    Some("b52e26e2f872d282ffac460bf9770b25ac6f102cda0e6ca7158df98c94e8b3da")
                ),
                file(
                    "plain_text/train-00000-of-00001.parquet",
                    20_979_968,
                    Some("db47d16b5c297cc0dd625e519c81319c24c9149e70e8496de5475f6fa928342c")
                ),
                file(
                    "plain_text/unsupervised-00000-of-00001.parquet",
                    41_996_509,
                    Some("74d14fbfcbb39fb7d299c38ca9f0ae6d231bf97108da85d620027ba437b6d52e")
                ),
            ]
        );
        assert_eq!(
            recorded.asked(),
            [
                api(&format!("datasets/stanfordnlp/imdb/revision/main?{EXPAND}")),
                api(&format!(
                    "datasets/stanfordnlp/imdb/tree/{IMDB}?recursive=true"
                )),
            ]
        );
    }

    #[test]
    fn a_listing_sent_in_pages_is_read_to_its_last_page() {
        let revision = api(&format!("datasets/google/fleurs/revision/main?{EXPAND}"));
        let first = api(&format!(
            "datasets/google/fleurs/tree/{FLEURS}?recursive=true"
        ));
        let second = api(&format!(
            "datasets/google/fleurs/tree/{FLEURS}?expand=false&recursive=true&limit=1000&cursor=ZXlKbWFXeGxYMjVoYldVaU9pSndZWEp4ZFdWMExXUmhkR0V2WVd4c0wzUnlZV2x1TFRBd01EVTFMVzltTFRBd01EVTNMbkJoY25GMVpYUWlMQ0owY21WbFgyOXBaQ0k2SW1JNU4yTmtNakF4WTJFeE9UZGtOekUxT1RobVltWXlNR0l4TjJKbFpUTmlObU15TTJVMU9XRWlmUT09OjEwMDA%3D"
        ));
        // both pages are trimmed to a few of the 1,313 entries the hub sent
        let recorded = Recorded::default()
            .fixture(&revision, "hf/fleurs-revision.json")
            .fixture(&first, "hf/fleurs-tree-1.json")
            .next(&first, &second)
            .fixture(&second, "hf/fleurs-tree-2.json");
        let listing = block_on(repo("datasets/google/fleurs").resolve(&recorded)).unwrap();
        let paths: Vec<&str> = listing
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        assert_eq!(
            paths,
            [
                "parquet-data/all/train-00054-of-00057.parquet",
                "parquet-data/all/train-00055-of-00057.parquet",
                "parquet-data/all/train-00056-of-00057.parquet",
                "parquet-data/all/validation-00000-of-00007.parquet",
            ]
        );
        assert_eq!(recorded.asked(), [revision, first, second]);
    }

    #[test]
    fn the_resolved_reference_asks_for_the_same_commit() {
        let at_commit = api(&format!(
            "datasets/stanfordnlp/imdb/revision/{IMDB}?{EXPAND}"
        ));
        // the hub answers for a commit exactly as it answered for the branch pointing at it
        let recorded = imdb().fixture(&at_commit, "hf/imdb-revision.json");
        let first = block_on(repo("datasets/stanfordnlp/imdb").resolve(&recorded)).unwrap();
        let again = block_on(first.resolved.resolve(&recorded)).unwrap();
        assert_eq!(again.resolved, first.resolved);
        assert_eq!(again.files, first.files);
        assert_eq!(
            recorded.asked()[2..],
            [
                at_commit,
                api(&format!(
                    "datasets/stanfordnlp/imdb/tree/{IMDB}?recursive=true"
                ))
            ]
        );
    }

    #[test]
    fn a_missing_revision_is_unresolved() {
        let recorded = Recorded::default().on(
            &api(&format!(
                "datasets/stanfordnlp/imdb/revision/no-such-branch?{EXPAND}"
            )),
            404,
            fixture_bytes("hf/missing-revision.json"),
        );
        let err = block_on(repo("datasets/stanfordnlp/imdb@no-such-branch").resolve(&recorded))
            .unwrap_err();
        assert_eq!(err.kind, Kind::ReferenceUnresolved);
    }

    #[test]
    fn a_repository_hidden_from_this_machine_names_the_token() {
        let recorded = Recorded::default().on(
            &api(&format!(
                "datasets/stanfordnlp/no-such-dataset/revision/main?{EXPAND}"
            )),
            401,
            fixture_bytes("hf/hidden.json"),
        );
        let err =
            block_on(repo("datasets/stanfordnlp/no-such-dataset").resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::AuthRequired);
        let help = err.help.unwrap();
        assert!(
            help.contains("https://huggingface.co/datasets/stanfordnlp/no-such-dataset")
                && help.contains("HF_TOKEN"),
            "{help}"
        );
    }

    #[test]
    fn a_gated_repository_names_the_page_that_accepts_its_terms() {
        let recorded = Recorded::default()
            .fixture(
                &api(&format!(
                    "models/meta-llama/Llama-2-7b/revision/main?{EXPAND}"
                )),
                "hf/llama-revision.json",
            )
            .on(
                &api("models/meta-llama/Llama-2-7b/auth-check"),
                401,
                fixture_bytes("hf/llama-auth-check.json"),
            );
        let err = block_on(repo("models/meta-llama/Llama-2-7b").resolve(&recorded)).unwrap_err();
        assert_eq!(err.kind, Kind::AuthRequired);
        let help = err.help.unwrap();
        assert!(
            help.contains("https://huggingface.co/meta-llama/Llama-2-7b ")
                && help.contains("HF_TOKEN"),
            "{help}"
        );
        assert_eq!(recorded.asked().len(), 2, "nothing is listed before access");
    }

    #[test]
    #[ignore = "live: talks to huggingface.co"]
    fn live_imdb_resolves_to_a_commit() {
        let work = Arc::new(Work::default());
        let options = Options {
            offline: false,
            jobs: 4,
            credentials: Vec::new(),
        };
        let net = Net::new(options, work.clone(), Arc::default(), Arc::new(|_| {})).unwrap();
        let reference = Reference::parse("hf:datasets/stanfordnlp/imdb")
            .unwrap()
            .reference;
        let listing = block_on(reference.resolve(&net)).unwrap();
        let Reference::HuggingFace(pinned) = &listing.resolved else {
            panic!("{}", listing.resolved);
        };
        assert!(pinned.revision.as_deref().is_some_and(is_commit));
        assert!(
            listing
                .files
                .iter()
                .any(|file| file.path.ends_with(".parquet") && file.sha256.is_some())
        );
        assert_eq!(work.counts().requests, 2);
    }

    #[test]
    fn a_pinned_commit_cannot_resolve_to_another_commit() {
        let requested = "1111111111111111111111111111111111111111";
        let revision = api(&format!(
            "datasets/stanfordnlp/imdb/revision/{requested}?{EXPAND}"
        ));
        let recorded = Recorded::default()
            .fixture(&revision, "hf/imdb-revision.json")
            .fixture(
                &api(&format!(
                    "datasets/stanfordnlp/imdb/tree/{IMDB}?recursive=true"
                )),
                "hf/imdb-tree.json",
            );
        let error =
            block_on(repo(&format!("datasets/stanfordnlp/imdb@{requested}")).resolve(&recorded))
                .unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert_eq!(recorded.asked(), [revision]);
    }

    #[test]
    fn the_stack_requires_accepted_terms_and_a_token() {
        let revision = api(&format!(
            "datasets/bigcode/the-stack/revision/main?{EXPAND}"
        ));
        let access = api("datasets/bigcode/the-stack/auth-check");
        let recorded = Recorded::default()
            .fixture(&revision, "hf/stack-revision.json")
            .on(&access, 401, fixture_bytes("hf/stack-auth-check.json"));
        let error = block_on(repo("datasets/bigcode/the-stack").resolve(&recorded)).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        let help = error.help.unwrap();
        assert!(help.contains("https://huggingface.co/datasets/bigcode/the-stack"));
        assert!(help.contains("HF_TOKEN"));
        assert_eq!(recorded.asked(), [revision, access]);
    }
}
