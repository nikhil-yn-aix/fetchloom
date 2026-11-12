use super::*;
use crate::error::Kind;
use crate::net::{Authorized, Given, Sent};
use crate::source::Reference;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const LISTING: &str =
    "https://azureopendatastorage.blob.core.windows.net/mnist?restype=container&comp=list";

fn listed(reference: &str, recorded: &Recorded) -> Result<crate::source::Listing, Error> {
    block_on(Reference::parse(reference)?.reference.resolve(recorded))
}

fn paths(listing: &crate::source::Listing) -> Vec<&str> {
    listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect()
}

/// a container's blobs, each with its size, the MD5 azure keeps when it keeps one, its link and
/// its `ETag` as a GET quotes it. the empty blobs that only mark a folder of other blobs are no
/// files.
#[test]
fn a_container_lists_its_blobs_with_the_md5_azure_states() {
    let recorded = Recorded::default().fixture(&format!("{LISTING}&prefix="), "azure/mnist.xml");
    let listing = listed("azure:azureopendatastorage/mnist/", &recorded).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "azure:azureopendatastorage/mnist/"
    );
    assert_eq!(listing.files.len(), 30);
    let labels = listing
        .files
        .iter()
        .find(|file| file.path == "t10k-labels-idx1-ubyte.gz")
        .unwrap();
    assert_eq!(labels.size, Some(4542));
    assert_eq!(
        labels.md5,
        crate::digest::Md5::from_hex("ec29112dd5afa0611ce80d1b7f02629c")
    );
    assert_eq!(
        labels.url.as_str(),
        "https://azureopendatastorage.blob.core.windows.net/mnist/t10k-labels-idx1-ubyte.gz"
    );
    assert!(
        labels
            .etag
            .as_deref()
            .is_some_and(|etag| etag.starts_with("\"0x") && etag.ends_with('"')),
        "{:?}",
        labels.etag
    );
    let merged = listing
        .files
        .iter()
        .find(|file| file.path == "processed/dataset_filter=all/merged.csv")
        .unwrap();
    assert_eq!((merged.size, merged.md5), (Some(127_868_469), None));
    let names = paths(&listing);
    assert!(names.contains(&"processedManifest/manifest.json"));
    for mark in ["processedManifest", "processedManifest/spdx_2.2"] {
        assert!(!names.contains(&mark), "{mark}");
    }
    assert_eq!(recorded.asked(), [format!("{LISTING}&prefix=")]);
}

/// a prefix that names one blob lists that blob alone.
#[test]
fn a_prefix_naming_a_blob_is_that_one_file() {
    let recorded = Recorded::default().fixture(
        &format!("{LISTING}&prefix=t10k-labels-idx1-ubyte.gz"),
        "azure/one.xml",
    );
    let listing = listed(
        "azure:azureopendatastorage/mnist/t10k-labels-idx1-ubyte.gz",
        &recorded,
    )
    .unwrap();
    assert_eq!(paths(&listing), ["t10k-labels-idx1-ubyte.gz"]);
}

/// a folder listed in two pages: the second is asked for by the first's marker, and a blob that
/// marks a folder on the first page is dropped once the second shows what is in that folder.
#[test]
fn a_folder_in_pages_follows_each_marker() {
    let first = format!("{LISTING}&prefix=processedManifest%2F");
    let second = format!(
        "{first}&marker=2%21120%21MDAwMDQ1IXByb2Nlc3NlZE1hbmlmZXN0L3NwZHhfMi4yL21hbmlmZXN0LnNwZHguanNvbiEwMDAwMjghOTk5OS0xMi0zMVQyMzo1OTo1OS45OTk5OTk5WiE-"
    );
    let recorded = Recorded::default()
        .fixture(&first, "azure/folder-1.xml")
        .fixture(&second, "azure/folder-2.xml");
    let listing = listed(
        "azure:azureopendatastorage/mnist/processedManifest/",
        &recorded,
    )
    .unwrap();
    assert_eq!(
        paths(&listing),
        [
            "manifest.json",
            "manifest.json.sha256",
            "spdx_2.2/manifest.spdx.json",
            "spdx_2.2/manifest.spdx.json.sha256",
        ]
    );
    assert_eq!(recorded.asked(), [first, second]);
}

/// a page that sends a marker it sent before would keep fetchloom asking, so it is refused.
#[test]
fn a_marker_sent_twice_is_refused() {
    let page = |marker: &str| {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults ContainerName=\"raw\"><Blobs><Blob><Name>a-{marker}.csv</Name><Properties><Content-Length>1</Content-Length></Properties></Blob></Blobs><NextMarker>{marker}</NextMarker></EnumerationResults>"
        )
    };
    let first = "https://lab.blob.core.windows.net/raw?restype=container&comp=list&prefix=";
    let recorded = Recorded::default().on(first, 200, page("again")).on(
        &format!("{first}&marker=again"),
        200,
        page("again"),
    );
    let error = listed("azure:lab/raw/", &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(error.message.contains("repeated"), "{}", error.message);
}

/// azure refuses a container without credentials as it refuses one that does not exist. the
/// error says how to send them, as the request went: for a container nobody listed, for a listed
/// one with none found, and for a SAS azure refused. what azure's error repeats of the SAS never
/// shows.
#[test]
fn a_refused_listing_says_how_to_send_a_sas() {
    let url = "https://lab.blob.core.windows.net/raw?restype=container&comp=list&prefix=scans%2F";
    let refused = |recorded: Recorded| listed("azure:lab/raw/scans/", &recorded).unwrap_err();
    let unlisted = refused(Recorded::default().on(url, 401, fixture_bytes("azure/private.xml")));
    assert_eq!(unlisted.kind, Kind::AuthRequired);
    assert_eq!(
        unlisted.message,
        "azure:lab/raw/scans/ is private, or does not exist: azure refused to list it without credentials"
    );
    assert_eq!(
        unlisted.help.as_deref(),
        Some(
            "if you trust azure:lab/raw with your azure credentials, list it in azure_containers in the settings file or FETCHLOOM_AZURE_CONTAINERS, and sign in with `az login` or set AZURE_STORAGE_SAS_TOKEN"
        )
    );
    let listed_without = refused(
        Recorded::default()
            .on(url, 401, fixture_bytes("azure/private.xml"))
            .authorized(
                url,
                Authorized::Azure(Sent::Anonymous {
                    container: "azure:lab/raw".to_owned(),
                    listed: true,
                }),
            ),
    );
    assert!(
        listed_without
            .message
            .ends_with("and none were found for azure:lab/raw"),
        "{}",
        listed_without.message
    );
    let sas = refused(
        Recorded::default()
            .on(url, 403, fixture_bytes("azure/refused.xml"))
            .authorized(
                url,
                Authorized::Azure(Sent::Sas {
                    container: "azure:lab/raw".to_owned(),
                    given: Given::Environment,
                    expiry: Some("2026-01-01T00:00:00Z".to_owned()),
                }),
            ),
    );
    assert_eq!(
        sas.message,
        "azure refused the SAS in AZURE_STORAGE_SAS_TOKEN for azure:lab/raw/scans/"
    );
    let said = format!("{sas:?}");
    assert!(
        said.contains("the SAS says it works until 2026-01-01T00:00:00Z"),
        "{said}"
    );
    assert!(said.contains("AuthenticationFailed"), "{said}");
    assert!(!said.contains("CANARY"), "{said}");
}

/// an answer that is no listing, or a blob a listing cannot describe, is refused with why.
#[test]
fn an_answer_that_is_no_listing_says_why() {
    let url = "https://lab.blob.core.windows.net/raw?restype=container&comp=list&prefix=";
    let blob = |properties: &str| {
        format!(
            "<EnumerationResults><Blobs><Blob><Name>a.csv</Name><Properties>{properties}</Properties></Blob></Blobs><NextMarker /></EnumerationResults>"
        )
    };
    for (body, said) in [
        ("<EnumerationResults><Blobs>".to_owned(), "is not closed"),
        (
            "<Error><Code>x</Code></Error>".to_owned(),
            "the answer is <Error>",
        ),
        (blob(""), "the size of \"a.csv\" is not a number"),
        (
            blob("<Content-Length>1</Content-Length><Content-MD5>not-base64</Content-MD5>"),
            "the MD5 of \"a.csv\" is not base64",
        ),
        (
            "<EnumerationResults><Blobs><Blob><Name>a/../b.csv</Name><Properties><Content-Length>1</Content-Length></Properties></Blob></Blobs></EnumerationResults>".to_owned(),
            "no link can name",
        ),
    ] {
        let recorded = Recorded::default().on(url, 200, body.clone());
        let error = listed("azure:lab/raw/", &recorded).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid, "{body}");
        assert!(error.message.contains(said), "{body}: {}", error.message);
    }
}

/// a SAS reads as the portal and the az cli give one, and a container's entry as its name or
/// its link, which may carry its own SAS. neither shows the SAS when printed for debugging.
#[test]
fn settings_read_containers_and_sas_as_azure_gives_them() {
    let sas =
        Sas::parse("?sv=2022-11-02&sr=c&sp=rl&se=2026-12-31T00%3A00%3A00Z&sig=abc%3D").unwrap();
    assert_eq!(
        sas.query(),
        "sv=2022-11-02&sr=c&sp=rl&se=2026-12-31T00%3A00%3A00Z&sig=abc%3D"
    );
    assert_eq!(sas.expiry().as_deref(), Some("2026-12-31T00:00:00Z"));
    assert_eq!(format!("{sas:?}"), "Sas(***)");
    for bad in ["", "sv=1", "sig=abc", "sv=1&sig=a b", "sv=1&sig=a#b"] {
        assert_eq!(Sas::parse(bad), None, "{bad}");
    }
    let linked =
        AzureContainer::parse("https://lab.blob.core.windows.net/raw/?sv=2022-11-02&sig=abc")
            .unwrap();
    assert_eq!(linked.to_string(), "azure:lab/raw");
    assert_eq!(linked.sas().map(Sas::query), Some("sv=2022-11-02&sig=abc"));
    assert!(!format!("{linked:?}").contains("abc"));
    assert_eq!(
        AzureContainer::parse("azure:Lab/raw/").map(|entry| entry.to_string()),
        Some("azure:lab/raw".to_owned())
    );
    for bad in [
        "azure:lab",
        "azure:lab/raw/scans",
        "https://lab.blob.core.windows.net/raw?sp=rl",
        "https://lab.blob.core.windows.net:8443/raw",
        "https://lab.example.org/raw",
    ] {
        assert_eq!(AzureContainer::parse(bad), None, "{bad}");
    }
}

#[test]
#[ignore = "live: talks to azureopendatastorage.blob.core.windows.net"]
fn live_azure() {
    let work = std::sync::Arc::new(crate::work::Work::default());
    let net = crate::net::Net::new(
        crate::net::Options {
            offline: crate::net::Offline::No,
            jobs: 4,
            credentials: Vec::new(),
            split: crate::net::Splitting::WhenSlow,
        },
        work.clone(),
        std::sync::Arc::default(),
        std::sync::Arc::new(|_| {}),
    );
    let start = std::time::Instant::now();
    let reference = Reference::parse("azure:azureopendatastorage/mnist/")
        .unwrap()
        .reference;
    let listing = block_on(reference.resolve(&net)).unwrap();
    eprintln!(
        "azure: {} files, {} requests, {:?}",
        listing.files.len(),
        work.counts().requests,
        start.elapsed()
    );
    let labels = listing
        .files
        .iter()
        .find(|file| file.path == "t10k-labels-idx1-ubyte.gz")
        .unwrap();
    assert_eq!(
        (labels.size, labels.md5),
        (
            Some(4542),
            crate::digest::Md5::from_hex("ec29112dd5afa0611ce80d1b7f02629c")
        )
    );
    assert_eq!(work.counts().requests, 1);
}
