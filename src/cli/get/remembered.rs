//! what an earlier `get` kept from a link only its source can list, as hugging face lists a
//! repository: the one file, by its path in the listing, its size and its SHA-256. offline, that
//! file stands in for the listing, so the same get runs again without a network. the cache keeps
//! it as it keeps what each URL served, and `cache gc` forgets both once the bytes are gone.

use url::Url;

use super::super::args::Get;
use super::super::sync as engine;
use crate::cache::{Cache, UrlRecord, Validators};
use crate::digest::Sha256;
use crate::source::{Access, Listing, Parsed, Remote};

/// what a link and the patterns choosing its files are remembered by, or none for a link no
/// source has to list, whose own URL record serves.
pub(super) fn key(args: &Get, parsed: &Parsed) -> Option<String> {
    if engine::resolves_locally(&parsed.reference) {
        return None;
    }
    let mut key = format!("get {}", parsed.reference);
    for pattern in super::wanted_patterns(args, parsed) {
        key.push_str("\nselect ");
        key.push_str(&pattern);
    }
    for pattern in &args.filter.exclude {
        key.push_str("\nexclude ");
        key.push_str(pattern);
    }
    Some(key)
}

/// remembers that the link `key` names kept the file at `path` in its listing, holding `sha256`.
pub(super) fn keep(cache: &Cache, key: &str, path: &str, sha256: Sha256, size: u64) {
    let record = UrlRecord {
        url: key.to_owned(),
        sha256,
        size,
        name: Some(path.to_owned()),
        validators: Validators::default(),
    };
    if cache.url_record(key).as_ref() == Some(&record) {
        return;
    }
    // the record only spares an offline run its source, so failing to write one costs no more
    if let Err(err) = cache.save_url_record(&record) {
        tracing::debug!(%err, "could not record what the link kept");
    }
}

/// the one file the link kept last time, as the listing an offline run takes in place of its
/// source's, holding those bytes, or the ones `sha256` names. none for a record with no path.
pub(super) fn listing(
    parsed: &Parsed,
    kept: &UrlRecord,
    sha256: Option<Sha256>,
) -> Option<Listing> {
    Some(Listing {
        resolved: parsed.reference.clone(),
        title: None,
        license: None,
        doi: None,
        access: Access::Unknown,
        files: vec![file(parsed, kept.name.clone()?, Some(kept), sha256)?],
        unavailable: Vec::new(),
        warnings: Vec::new(),
        within: Vec::new(),
    })
}

/// the file at `path` the link kept, holding the bytes `sha256` names, or else those of `kept`.
/// its link is the reference itself, which nothing downloads offline.
pub(super) fn file(
    parsed: &Parsed,
    path: String,
    kept: Option<&UrlRecord>,
    sha256: Option<Sha256>,
) -> Option<Remote> {
    let sha256 = sha256.or_else(|| kept.map(|kept| kept.sha256))?;
    Some(Remote {
        path,
        size: kept
            .filter(|kept| kept.sha256 == sha256)
            .map(|kept| kept.size),
        sha256: Some(sha256),
        md5: None,
        url: Url::parse(&parsed.reference.to_string()).ok()?,
        named_by_server: false,
        etag: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::args::Filter;
    use crate::source::Reference;

    fn args(select: &[&str], exclude: &[&str]) -> Get {
        Get {
            links: Vec::new(),
            output: None,
            force: false,
            dir: None,
            input_file: None,
            sha256: None,
            filter: Filter {
                select: select.iter().map(|&pattern| pattern.to_owned()).collect(),
                exclude: exclude.iter().map(|&pattern| pattern.to_owned()).collect(),
            },
            no_extract: false,
            dry: crate::cli::args::DryRun { dry_run: false },
        }
    }

    /// a pasted link to one file and the same repository with that file selected keep one
    /// record, other patterns another, and a plain link none, since its own URL record serves.
    #[test]
    fn a_link_is_remembered_with_the_patterns_that_chose_its_file() {
        let pasted = Reference::parse(
            "https://huggingface.co/datasets/scikit-learn/iris/blob/main/Iris.csv",
        )
        .unwrap();
        let repository = Reference::parse("hf:datasets/scikit-learn/iris@main").unwrap();
        let chosen = key(&args(&["Iris.csv"], &[]), &repository);
        assert_eq!(key(&args(&[], &[]), &pasted), chosen);
        assert_ne!(key(&args(&["*.csv"], &[]), &repository), chosen);
        assert_ne!(key(&args(&["Iris.csv"], &["x"]), &repository), chosen);
        let plain = Reference::parse("https://example.org/iris.csv").unwrap();
        assert_eq!(key(&args(&[], &[]), &plain), None);
    }
}
