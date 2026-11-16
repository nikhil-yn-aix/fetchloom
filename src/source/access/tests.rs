use super::*;
use crate::source::recorded::{Recorded, block_on};
use crate::source::{Listing, Reference, Remote};

/// a resolver's request to an origin its source does not name is refused before it goes out.
#[test]
fn a_request_outside_the_descriptor_never_goes_out() {
    let recorded = Recorded::default().on("https://zenodo.org/api/records/1", 200, b"{}".to_vec());
    let scoped = Scoped::new(&recorded, &ZENODO, None, "zenodo:1".to_owned());
    for elsewhere in [
        "https://example.org/api/records/1",
        "http://zenodo.org/api/records/1",
        "https://zenodo.org:8443/api/records/1",
        "https://api.github.com/repos/org/private/contents/",
    ] {
        let url = Url::parse(elsewhere).unwrap();
        let error = block_on(scoped.get(&url)).unwrap_err();
        assert_eq!(error.kind, Kind::NetworkRefused, "{elsewhere}");
        assert!(error.message.contains("zenodo:1"), "{}", error.message);
        assert!(block_on(scoped.get_once(&url)).is_err(), "{elsewhere}");
        assert!(block_on(scoped.get_with(&url, &[])).is_err(), "{elsewhere}");
    }
    assert!(recorded.asked().is_empty(), "{:?}", recorded.asked());
    let own = Url::parse("https://zenodo.org/api/records/1").unwrap();
    assert_eq!(block_on(scoped.get(&own)).unwrap().status, 200);
}

/// a written origin is the reference's own, at its scheme, host and port alone; a fixed one with
/// `*` takes the hosts below its name and no other.
#[test]
fn origins_hold_their_own_scheme_host_and_port() {
    let url = |text: &str| Url::parse(text).unwrap();
    let written = url("https://dv.example.org:8443/").origin();
    for (asked, held) in [
        ("https://dv.example.org:8443/api/users/:me", true),
        ("https://dv.example.org/api/users/:me", false),
        ("http://dv.example.org:8443/api/users/:me", false),
        ("https://other.example.org:8443/api", false),
    ] {
        assert_eq!(
            Origins::Written.holds(&url(asked), Some(&written)),
            held,
            "{asked}"
        );
    }
    assert!(!Origins::Written.holds(&url("https://dv.example.org:8443/"), None));
    for (asked, held) in [
        ("https://ftp.sra.ebi.ac.uk/vol1/a.fastq.gz", true),
        ("https://ebi.ac.uk/a", true),
        ("https://deep.ftp.ebi.ac.uk/a", true),
        ("https://evilebi.ac.uk/a", false),
        ("https://ebi.ac.uk.example.org/a", false),
        ("http://ftp.sra.ebi.ac.uk/a", false),
        ("https://ftp.sra.ebi.ac.uk:8443/a", false),
    ] {
        assert_eq!(ENA.files.holds(&url(asked), None), held, "{asked}");
    }
}

/// a listing whose file links where its source keeps no files is refused, whichever way the link
/// got there.
#[test]
fn a_file_linked_outside_its_sources_files_is_refused() {
    let reference = Reference::parse("zenodo:3608990").unwrap().reference;
    let listing = |link: &str| Listing {
        resolved: reference.clone(),
        title: None,
        license: None,
        doi: None,
        access: crate::source::Access::Public,
        files: vec![Remote {
            path: "iris.csv".to_owned(),
            size: None,
            sha256: None,
            md5: None,
            url: Url::parse(link).unwrap(),
            named_by_server: false,
            etag: None,
            blob: None,
        }],
        unavailable: Vec::new(),
        warnings: Vec::new(),
        within: Vec::new(),
    };
    let kept = "https://zenodo.org/api/records/3608990/files/iris.csv/content";
    assert!(crate::source::check(&reference, listing(kept), 10).is_ok());
    let error =
        crate::source::check(&reference, listing("https://example.org/iris.csv"), 10).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(
        error.message.contains(
            "links \"iris.csv\" at https://example.org, where zenodo's files are not kept"
        ),
        "{}",
        error.message
    );
}

/// a refusal from a source's own host names its account, and only from where its key goes or
/// its own site, so a sign-in it offers can lift it.
#[test]
fn a_refusal_names_the_account_whose_key_reaches_its_host() {
    for (host, account) in [
        ("huggingface.co", Some(Account::HuggingFace)),
        ("www.huggingface.co", Some(Account::HuggingFace)),
        ("hf.co", Some(Account::HuggingFace)),
        ("api.kaggle.com", Some(Account::Kaggle)),
        ("www.kaggle.com", Some(Account::Kaggle)),
        ("ndownloader.figshare.com", Some(Account::Figshare)),
        ("files.osf.io", Some(Account::Osf)),
        ("api.semanticscholar.org", Some(Account::S2)),
        ("github.com", None),
        ("storage.googleapis.com", None),
        ("export.arxiv.org", None),
        ("example.org", None),
    ] {
        assert_eq!(account_at(host), account, "{host}");
    }
}

/// a source's key goes to its own api, the first origin its resolver asks, or for an
/// installation named by its reference, to that installation alone.
#[test]
fn every_key_goes_to_its_sources_own_api() {
    for access in ALL {
        let Some(key) = access.key else {
            continue;
        };
        match key.host {
            Some(host) => assert_eq!(access.api.hosts().next(), Some(host), "{}", access.name),
            None => assert!(matches!(access.api, Origins::Written), "{}", access.name),
        }
    }
}
