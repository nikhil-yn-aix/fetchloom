use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Instant;

use super::*;
use crate::digest::Md5;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};
use crate::source::{Reference, Remote};

const OPENNEURO: &str =
    "https://s3.amazonaws.com/openneuro.org/?list-type=2&prefix=ds000001%2F&encoding-type=url";
const OPENNEURO_NEXT: &str = "https://s3.amazonaws.com/openneuro.org/?list-type=2&prefix=ds000001%2F&encoding-type=url&continuation-token=1a626583KTlgb7ONlPzl9YLS39I6ZpxwWL65%2Fcvpy0Frgusxmb5cxvg%3D%3D";
const SENTINEL_PREFIX: &str = "sentinel-s2-l2a-cogs/1/C/CV/2018/10/S2B_1CCV_20181004_0_L2A/";
const SENTINEL: &str = "https://sentinel-cogs.s3.amazonaws.com/?list-type=2&prefix=sentinel-s2-l2a-cogs%2F1%2FC%2FCV%2F2018%2F10%2FS2B_1CCV_20181004_0_L2A%2F&encoding-type=url";
const SENTINEL_WEST: &str = "https://sentinel-cogs.s3.us-west-2.amazonaws.com/?list-type=2&prefix=sentinel-s2-l2a-cogs%2F1%2FC%2FCV%2F2018%2F10%2FS2B_1CCV_20181004_0_L2A%2F&encoding-type=url";
const STATES: &str = "https://storage.googleapis.com/storage/v1/b/cloud-samples-data/o?prefix=bigquery%2Fus-states%2F&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken";
const STATES_NEXT: &str = "https://storage.googleapis.com/storage/v1/b/cloud-samples-data/o?prefix=bigquery%2Fus-states%2F&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken&pageToken=ClNiaWdxdWVyeS91cy1zdGF0ZXMvMjAyMS0wNy0wMlQxNjowNDo0OF83MDM0NC9hbGxfbmFtZXNwYWNlcy9raW5kX3VzLXN0YXRlcy9vdXRwdXQtMA%3D%3D";

fn reference(text: &str) -> Reference {
    Reference::parse(text).unwrap().reference
}

fn resolve(text: &str, recorded: &Recorded) -> Result<Listing, Error> {
    block_on(reference(text).resolve(recorded))
}

/// a one-page S3 listing of `(key, etag)` objects: one byte, or zero for folder markers.
fn page(escaped: bool, objects: &[(&str, &str)]) -> Vec<u8> {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>b</Name>",
    );
    if escaped {
        xml.push_str("<EncodingType>url</EncodingType>");
    }
    xml.push_str("<IsTruncated>false</IsTruncated>");
    for (key, etag) in objects {
        let size = u8::from(!key.ends_with('/') && !key.ends_with("%2F"));
        let _ = write!(
            xml,
            "<Contents><Key>{key}</Key><LastModified>2020-08-21T20:24:12.000Z</LastModified><ETag>{etag}</ETag><Size>{size}</Size></Contents>"
        );
    }
    xml.push_str("</ListBucketResult>");
    xml.into_bytes()
}

fn listing_url(base: &str, prefix: &str) -> String {
    format!(
        "{base}?{}",
        query(&[
            ("list-type", "2"),
            ("prefix", prefix),
            ("encoding-type", "url")
        ])
    )
}

#[test]
fn references_read_back_as_written() {
    for (text, canonical) in [
        (
            "s3://openneuro.org/ds000001/",
            "s3://openneuro.org/ds000001/",
        ),
        ("S3://noaa-ghcn-pds", "s3://noaa-ghcn-pds/"),
        (
            "s3://noaa-ghcn-pds/csv/by_year/1763.csv",
            "s3://noaa-ghcn-pds/csv/by_year/1763.csv",
        ),
        (
            "s3://bucket/a folder/with+plus/",
            "s3://bucket/a folder/with+plus/",
        ),
        (
            "gs://cloud-samples-data/bigquery/us-states/",
            "gs://cloud-samples-data/bigquery/us-states/",
        ),
        ("gs://gcp_public_data/", "gs://gcp_public_data/"),
        (
            "https://openneuro.org.s3.amazonaws.com/ds000001/",
            "s3://openneuro.org/ds000001/",
        ),
        (
            "https://noaa-ghcn-pds.s3.amazonaws.com/",
            "s3://noaa-ghcn-pds/",
        ),
        (
            "https://sentinel-cogs.s3.us-west-2.amazonaws.com/sentinel-s2-l2a-cogs/",
            "s3://sentinel-cogs/sentinel-s2-l2a-cogs/",
        ),
        (
            "https://my.s3data.s3-eu-west-1.amazonaws.com/x/",
            "s3://my.s3data/x/",
        ),
        (
            "https://s3.us-west-2.amazonaws.com/sentinel-cogs/sentinel-s2-l2a-cogs/",
            "s3://sentinel-cogs/sentinel-s2-l2a-cogs/",
        ),
        (
            "https://s3.amazonaws.com/openneuro.org/ds000001/",
            "s3://openneuro.org/ds000001/",
        ),
        (
            "https://s3.amazonaws.com/openneuro.org",
            "s3://openneuro.org/",
        ),
        (
            "https://storage.googleapis.com/cloud-samples-data/bigquery/us-states/",
            "gs://cloud-samples-data/bigquery/us-states/",
        ),
        (
            "https://cloud-samples-data.storage.googleapis.com/bigquery/",
            "gs://cloud-samples-data/bigquery/",
        ),
        (
            "https://storage.googleapis.com/bucket/a%20folder/%2B/",
            "gs://bucket/a folder/+/",
        ),
        (
            "https://storage.googleapis.com/cloud-samples-data",
            "gs://cloud-samples-data/",
        ),
    ] {
        let parsed = Reference::parse(text).unwrap();
        assert!(matches!(parsed.reference, Reference::Bucket(_)), "{text}");
        assert_eq!(parsed.reference.to_string(), canonical, "{text}");
        assert_eq!(parsed.file, None);
        assert_eq!(reference(canonical), parsed.reference, "{canonical}");
    }
}

#[test]
fn endpoint_references_read_back_as_written() {
    for (text, canonical) in [
        (
            "s3:https://minio.lab.example:9000/scans/raw/",
            "s3:https://minio.lab.example:9000/scans/raw/",
        ),
        (
            "S3:HTTPS://MINIO.lab.example:9000/scans",
            "s3:https://minio.lab.example:9000/scans/",
        ),
        (
            "s3:https://ceph.lab.example:443/scans/a b+c/",
            "s3:https://ceph.lab.example/scans/a b+c/",
        ),
        (
            "s3:http://10.0.0.5/old-scans/",
            "s3:http://10.0.0.5/old-scans/",
        ),
        (
            "s3:https://[::1]:9000/scans/one.csv",
            "s3:https://[::1]:9000/scans/one.csv",
        ),
    ] {
        let parsed = Reference::parse(text).unwrap();
        assert!(matches!(parsed.reference, Reference::Bucket(_)), "{text}");
        assert_eq!(parsed.reference.to_string(), canonical, "{text}");
        assert_eq!(reference(canonical), parsed.reference, "{canonical}");
    }
}

#[test]
fn an_endpoint_reference_is_a_plain_origin_and_a_bucket() {
    for text in [
        "s3:https://ada:hunter2@minio.lab.example/scans/",
        "s3:https://minio.lab.example/scans/?versions",
        "s3:https://minio.lab.example/scans/#raw",
        "s3:https://minio.lab.example/",
        "s3:https://minio.lab.example",
        "s3:https://minio.lab.example/Scans/",
        "s3:ftp://minio.lab.example/scans/",
        "s3:https:///scans/",
        "s3:https://minio.lab.example/scans/../raw/",
        "gs:https://storage.lab.example/scans/",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text}");
        assert!(!error.message.contains("hunter2"), "{}", error.message);
        assert!(error.help.unwrap().contains("://"), "{text}");
    }
}

/// an object's link names its bucket: on AWS by the host or the first segment of the service's
/// own host, and at another service by the origin and the first segment.
#[test]
fn an_object_link_names_the_bucket_holding_it() {
    for (link, bucket) in [
        (
            "https://lab-raw.s3.amazonaws.com/a/b.csv?versionId=1",
            Some("s3://lab-raw"),
        ),
        (
            "https://s3.us-west-2.amazonaws.com/openneuro.org/ds1/a.tsv",
            Some("s3://openneuro.org"),
        ),
        (
            "https://minio.lab.example:9000/scans/raw/a.nii",
            Some("s3:https://minio.lab.example:9000/scans"),
        ),
        ("https://minio.lab.example:9000/a.nii", None),
        ("https://example.org/", None),
    ] {
        assert_eq!(
            Bucket::holding(&Url::parse(link).unwrap())
                .map(|found| found.to_string())
                .as_deref(),
            bucket,
            "{link}"
        );
    }
}

#[test]
fn single_objects_and_other_storage_links_stay_plain_links() {
    for text in [
        "https://openneuro.org.s3.amazonaws.com/ds000001/participants.tsv",
        "https://s3.amazonaws.com/openneuro.org/ds000001/participants.tsv",
        "https://noaa-ghcn-pds.s3.amazonaws.com/ghcnd-stations.txt",
        "https://storage.googleapis.com/cloud-samples-data/bigquery/us-states/us-states.csv",
        "https://storage.googleapis.com/storage/v1/b/cloud-samples-data/o/",
        "https://storage.googleapis.com/download/storage/v1/b/x/o/",
        "https://bucket.s3.amazonaws.com/folder/?X-Amz-Signature=abc",
        "https://bucket.s3-website-us-east-1.amazonaws.com/folder/",
        "https://bucket.s3.dualstack.us-east-1.amazonaws.com/folder/",
        "https://ec2.amazonaws.com/folder/",
        "https://example.org/folder/",
    ] {
        let parsed = Reference::parse(text).unwrap();
        assert!(matches!(parsed.reference, Reference::Url(_)), "{text}");
    }
}

#[test]
fn malformed_bucket_references_are_refused() {
    for text in [
        "s3:openneuro.org/ds000001/",
        "s3://",
        "s3://ab/",
        "s3://Upper/",
        "s3://under_score/",
        "s3://-dash/",
        "s3://dot./",
        "s3://two..dots/",
        "gs://bad!name/",
        "s3://bucket/./",
        "gs://bucket/a/../b/",
    ] {
        let error = Reference::parse(text).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid, "{text:?}");
        assert!(error.help.unwrap().contains("://"), "{text:?}");
    }
    assert_eq!(
        Reference::parse("s3://bucket/line\nbreak/")
            .unwrap_err()
            .kind,
        Kind::ReferenceInvalid
    );
    let long = format!("s3://bucket/{}", "k".repeat(LONGEST_KEY + 1));
    assert!(Reference::parse(&long).is_err());
}

#[test]
fn s3_pages_follow_continuation_tokens_through_the_path_for_dotted_buckets() {
    let recorded = Recorded::default()
        .fixture(OPENNEURO, "bucket/openneuro-page1.xml")
        .fixture(OPENNEURO_NEXT, "bucket/openneuro-page2.xml");
    let listing = resolve("s3://openneuro.org/ds000001/", &recorded).unwrap();
    assert_eq!(recorded.asked(), [OPENNEURO, OPENNEURO_NEXT]);
    assert_eq!(listing.resolved.to_string(), "s3://openneuro.org/ds000001/");
    assert_eq!(listing.title, None);
    assert_eq!(listing.files.len(), 136);
    assert_eq!(
        listing.files[0],
        Remote {
            path: ".datalad/.gitattributes".to_owned(),
            size: Some(132),
            sha256: None,
            md5: Md5::from_hex("1beeac62f1ded905f45c6d6d20a4688b"),
            url: Url::parse(
                "https://s3.amazonaws.com/openneuro.org/ds000001/.datalad/.gitattributes"
            )
            .unwrap(),
            named_by_server: false,
            etag: Some("\"1beeac62f1ded905f45c6d6d20a4688b\"".to_owned()),
        }
    );
    let participants = listing
        .files
        .iter()
        .find(|file| file.path == "participants.tsv")
        .unwrap();
    assert_eq!(participants.size, Some(216));
    assert_eq!(
        participants.url.as_str(),
        "https://s3.amazonaws.com/openneuro.org/ds000001/participants.tsv"
    );
    assert!(listing.files.iter().all(|file| file.md5.is_some()));
}

#[test]
fn s3_follows_a_bucket_to_its_region_once() {
    let recorded = Recorded::default()
        .on(SENTINEL, 301, fixture_bytes("bucket/redirect.xml"))
        .region(SENTINEL, "us-west-2")
        .fixture(SENTINEL_WEST, "bucket/sentinel.xml");
    let text = format!("s3://sentinel-cogs/{SENTINEL_PREFIX}");
    let listing = resolve(&text, &recorded).unwrap();
    assert_eq!(recorded.asked(), [SENTINEL, SENTINEL_WEST]);
    assert_eq!(listing.resolved.to_string(), text);
    assert_eq!(listing.files.len(), 21);
    assert_eq!(listing.files[0].path, "AOT.tif");
    assert_eq!(
        listing.files[0].url.as_str(),
        format!("https://sentinel-cogs.s3.us-west-2.amazonaws.com/{SENTINEL_PREFIX}AOT.tif")
    );
    let parts = listing
        .files
        .iter()
        .find(|file| file.path == "B02.tif")
        .unwrap();
    assert_eq!(parts.md5, None, "a multipart ETag is no MD5");
    assert_eq!(parts.size, Some(38_149_405));
}

#[test]
fn soap_objects_use_path_style_at_global_and_regional_endpoints() {
    for bucket in ["bucket", "open.data"] {
        for region in [None, Some("eu-west-1")] {
            let service = region.map_or_else(
                || "s3.amazonaws.com".to_owned(),
                |region| format!("s3.{region}.amazonaws.com"),
            );
            let base = |service: &str| {
                if bucket.contains('.') {
                    format!("https://{service}/{bucket}/")
                } else {
                    format!("https://{bucket}.{service}/")
                }
            };
            let initial = listing_url(&base("s3.amazonaws.com"), "");
            let active = listing_url(&base(&service), "");
            let body = page(
                false,
                &[
                    ("soap", "\"x\""),
                    ("folder/soap", "\"x\""),
                    ("SOAP", "\"x\""),
                ],
            );
            let recorded = if let Some(region) = region {
                Recorded::default()
                    .on(
                        &initial,
                        301,
                        format!("<Error><Region>{region}</Region></Error>"),
                    )
                    .on(&active, 200, body)
            } else {
                Recorded::default().on(&initial, 200, body)
            };
            let listing = resolve(&format!("s3://{bucket}/"), &recorded).unwrap();
            assert_eq!(listing.files.len(), 3);
            assert_eq!(
                listing.files[0].url.as_str(),
                format!("https://{service}/{bucket}/soap")
            );
            assert_eq!(
                listing.files[1].url.as_str(),
                format!("{}folder/soap", base(&service))
            );
            assert_eq!(
                listing.files[2].url.as_str(),
                format!("{}SOAP", base(&service))
            );
            let asked = if region.is_some() {
                vec![initial, active]
            } else {
                vec![initial]
            };
            assert_eq!(recorded.asked(), asked);
        }
    }
}

#[test]
fn a_dotted_bucket_moves_to_the_regional_path_named_in_the_error() {
    let global = listing_url("https://s3.amazonaws.com/open.data/", "x/");
    let regional = listing_url("https://s3.eu-west-1.amazonaws.com/open.data/", "x/");
    let moved = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>AuthorizationHeaderMalformed</Code><Message>the region is wrong</Message><Region>eu-west-1</Region></Error>".to_vec();
    let recorded = Recorded::default().on(&global, 400, moved).on(
        &regional,
        200,
        page(false, &[("x/a.csv", "\"abc-2\"")]),
    );
    let listing = resolve("s3://open.data/x/", &recorded).unwrap();
    assert_eq!(recorded.asked(), [global.clone(), regional]);
    assert_eq!(
        listing.files[0].url.as_str(),
        "https://s3.eu-west-1.amazonaws.com/open.data/x/a.csv"
    );
    let endpoint = b"<Error><Code>PermanentRedirect</Code><Endpoint>open.data.s3.ap-south-1.amazonaws.com</Endpoint></Error>".to_vec();
    let south = listing_url("https://s3.ap-south-1.amazonaws.com/open.data/", "x/");
    let recorded = Recorded::default().on(&global, 301, endpoint).on(
        &south,
        200,
        page(false, &[("x/a.csv", "\"abc\"")]),
    );
    assert_eq!(
        resolve("s3://open.data/x/", &recorded).unwrap().files.len(),
        1
    );
}

#[test]
fn a_redirect_without_a_region_or_a_second_one_fails() {
    let recorded = Recorded::default().on(SENTINEL, 301, fixture_bytes("bucket/redirect.xml"));
    let text = format!("s3://sentinel-cogs/{SENTINEL_PREFIX}");
    let error = resolve(&text, &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::NetworkStatus);
    assert_eq!(recorded.asked(), [SENTINEL]);
    let recorded = Recorded::default()
        .on(SENTINEL, 301, fixture_bytes("bucket/redirect.xml"))
        .region(SENTINEL, "us-west-2")
        .on(SENTINEL_WEST, 301, fixture_bytes("bucket/redirect.xml"))
        .region(SENTINEL_WEST, "us-east-2");
    let error = resolve(&text, &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::NetworkStatus);
    assert_eq!(recorded.asked(), [SENTINEL, SENTINEL_WEST]);
    let recorded = Recorded::default()
        .on(SENTINEL, 301, fixture_bytes("bucket/redirect.xml"))
        .region(SENTINEL, "../../evil");
    assert_eq!(
        resolve(&text, &recorded).unwrap_err().kind,
        Kind::NetworkStatus
    );
}

/// S3 refusing a listing, as S3 words it.
const DENIED: &[u8] = b"<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>";

#[test]
fn a_refused_s3_listing_says_whether_a_key_went_with_it_and_what_to_do() {
    let url = listing_url("https://lab-raw.s3.amazonaws.com/", "raw/");
    let bucket = Bucket::parse("s3://lab-raw").unwrap();
    let unlisted = (
        "s3://lab-raw/raw/ is private: S3 refused to list it without a key",
        "if you trust s3://lab-raw with your AWS keys, add it to s3_buckets in the settings file or FETCHLOOM_S3_BUCKETS",
    );
    for (keyed, (message, help)) in [
        (None, unlisted),
        (
            Some(Keyed::Unsigned {
                bucket: bucket.clone(),
                listed: false,
            }),
            unlisted,
        ),
        (
            Some(Keyed::Unsigned {
                bucket: bucket.clone(),
                listed: true,
            }),
            (
                "s3://lab-raw/raw/ is private: S3 refused to list it without a key, and no AWS keys were found",
                "set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or AWS_PROFILE to a profile with keys in ~/.aws/credentials, a credential_process, or sso or a role through the AWS CLI. see https://nikhil-yn-aix.github.io/fetchloom/user/sources/buckets.html#private-s3-buckets",
            ),
        ),
        (
            Some(Keyed::Signed {
                bucket: bucket.clone(),
                key: "\u{2026}WXYZ from AWS_ACCESS_KEY_ID".to_owned(),
                region: "us-east-1".to_owned(),
            }),
            (
                "S3 refused the AWS key \u{2026}WXYZ from AWS_ACCESS_KEY_ID for s3://lab-raw/raw/",
                "ask the bucket's owner to let that key list and read it, or set AWS_PROFILE to a profile whose key can",
            ),
        ),
    ] {
        let mut recorded = Recorded::default().on(&url, 403, DENIED);
        if let Some(keyed) = keyed {
            recorded = recorded.keyed(&url, keyed);
        }
        let error = resolve("s3://lab-raw/raw/", &recorded).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(error.message, message);
        assert_eq!(error.help.as_deref(), Some(help));
        assert_eq!(
            error.details,
            ["lab-raw.s3.amazonaws.com said AccessDenied: Access Denied"]
        );
    }
    // fetchloom never agrees to pay for a bucket's requests, with a key or without one
    let recorded = Recorded::default().on(&url, 403, fixture_bytes("bucket/denied.xml"));
    let error = resolve("s3://lab-raw/raw/", &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "s3://lab-raw/raw/ bills the reader for each request, and fetchloom never agrees to pay"
    );
    assert_eq!(
        error.help.as_deref(),
        Some("use the AWS CLI with --request-payer requester for this bucket")
    );
}

#[test]
fn a_refused_download_says_whether_a_key_went_with_it_and_what_to_do() {
    let aws = Bucket::parse("s3://lab-raw").unwrap();
    let lab = Bucket::parse("s3:https://minio.lab.example:9000/scans").unwrap();
    let object =
        Url::parse("https://lab-raw.s3.eu-west-1.amazonaws.com/raw/a.csv?versionId=v1").unwrap();
    let scan = Url::parse("https://minio.lab.example:9000/scans/raw/a.csv").unwrap();
    for (keyed, url, message, help) in [
        (
            Keyed::Unsigned {
                bucket: aws.clone(),
                listed: false,
            },
            &object,
            "S3 refused https://lab-raw.s3.eu-west-1.amazonaws.com/raw/a.csv?versionId=v1 without a key",
            "if you trust s3://lab-raw with your AWS keys, add it to s3_buckets in the settings file or FETCHLOOM_S3_BUCKETS",
        ),
        (
            Keyed::Unsigned {
                bucket: lab.clone(),
                listed: true,
            },
            &scan,
            "minio.lab.example:9000 refused https://minio.lab.example:9000/scans/raw/a.csv without a key, and no AWS settings send keys to https://minio.lab.example:9000",
            "put endpoint_url = https://minio.lab.example:9000 in a profile in ~/.aws/config, with its keys in ~/.aws/credentials, or set AWS_ENDPOINT_URL_S3=https://minio.lab.example:9000 with AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY",
        ),
        (
            Keyed::Signed {
                bucket: aws,
                key: "\u{2026}WXYZ from profile lab in /home/ada/.aws/credentials".to_owned(),
                region: "eu-west-1".to_owned(),
            },
            &object,
            "S3 refused the AWS key \u{2026}WXYZ from profile lab in /home/ada/.aws/credentials for https://lab-raw.s3.eu-west-1.amazonaws.com/raw/a.csv?versionId=v1",
            "ask the owner of s3://lab-raw to let that key read it, or set AWS_PROFILE to a profile whose key can",
        ),
    ] {
        let error = keyed.refusal(url);
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(error.message, message);
        assert_eq!(error.help.as_deref(), Some(help));
    }
}

#[test]
fn a_refused_endpoint_listing_names_the_service_and_the_settings_it_needs() {
    let url = "https://minio.lab.example:9000/scans/?list-type=2&prefix=raw%2F&encoding-type=url";
    let reference = "s3:https://minio.lab.example:9000/scans/raw/";
    let bucket = Bucket::parse("s3:https://minio.lab.example:9000/scans").unwrap();
    let recorded = Recorded::default().on(url, 403, DENIED).keyed(
        url,
        Keyed::Unsigned {
            bucket,
            listed: true,
        },
    );
    let error = resolve(reference, &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "s3:https://minio.lab.example:9000/scans/raw/ is private: minio.lab.example:9000 refused to list it without a key, and no AWS settings send keys to https://minio.lab.example:9000"
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "put endpoint_url = https://minio.lab.example:9000 in a profile in ~/.aws/config, with its keys in ~/.aws/credentials, or set AWS_ENDPOINT_URL_S3=https://minio.lab.example:9000 with AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY"
        )
    );
    // a signature for another region than the service's is refused, and it says which one
    let malformed = b"<Error><Code>AuthorizationHeaderMalformed</Code><Message>the region 'us-east-1' is wrong; expecting 'eu-1'</Message><Region>eu-1</Region></Error>";
    let recorded = Recorded::default().on(url, 400, malformed.to_vec());
    let error = resolve(reference, &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "minio.lab.example:9000 wants the requests for s3:https://minio.lab.example:9000/scans/raw/ signed for the region eu-1"
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "set region = eu-1 in the AWS profile for https://minio.lab.example:9000, or AWS_REGION=eu-1"
        )
    );
    assert_eq!(recorded.asked(), [url]);
}

#[test]
fn an_endpoint_bucket_is_listed_by_path_and_states_no_md5() {
    let url = "https://minio.lab.example:9000/scans/?list-type=2&prefix=raw%2F&encoding-type=url";
    let body = page(
        true,
        &[
            ("raw%2Fa.csv", "\"1beeac62f1ded905f45c6d6d20a4688b\""),
            ("raw%2Fsoap", "\"x\""),
        ],
    );
    let recorded = Recorded::default().on(url, 200, body);
    let listing = resolve("s3:https://minio.lab.example:9000/scans/raw/", &recorded).unwrap();
    assert_eq!(
        listing.resolved.to_string(),
        "s3:https://minio.lab.example:9000/scans/raw/"
    );
    let links: Vec<_> = listing.files.iter().map(|file| file.url.as_str()).collect();
    assert_eq!(
        links,
        [
            "https://minio.lab.example:9000/scans/raw/a.csv",
            "https://minio.lab.example:9000/scans/raw/soap"
        ]
    );
    assert!(listing.files.iter().all(|file| file.md5.is_none()));
}

#[test]
fn a_signed_listing_states_no_md5_and_links_objects_in_the_region_it_was_signed_for() {
    let global = listing_url("https://lab-raw.s3.amazonaws.com/", "raw/");
    let body = page(
        true,
        &[("raw%2Fa.csv", "\"1beeac62f1ded905f45c6d6d20a4688b\"")],
    );
    let recorded = Recorded::default().on(&global, 200, body).keyed(
        &global,
        Keyed::Signed {
            bucket: Bucket::parse("s3://lab-raw").unwrap(),
            key: "\u{2026}WXYZ from AWS_ACCESS_KEY_ID".to_owned(),
            region: "eu-west-1".to_owned(),
        },
    );
    let listing = resolve("s3://lab-raw/raw/", &recorded).unwrap();
    assert_eq!(
        listing.files[0].url.as_str(),
        "https://lab-raw.s3.eu-west-1.amazonaws.com/raw/a.csv"
    );
    assert_eq!(listing.files[0].md5, None);
}

/// a refused cloud storage listing says whether google credentials went with it, and what would
/// let the next one through.
#[test]
fn a_refused_cloud_storage_listing_says_whether_credentials_went_with_it() {
    let url = STATES
        .replace("cloud-samples-data", "test")
        .replace("bigquery%2Fus-states%2F", "x%2F");
    let anonymous = |listed| crate::net::Authorized::Anonymous {
        bucket: "test".to_owned(),
        listed,
    };
    let unlisted = (
        "gs://test/x/ is private: google cloud storage refused to list it without credentials",
        "if you trust gs://test with your google credentials, add it to gcs_buckets in the settings file or FETCHLOOM_GCS_BUCKETS",
    );
    for (authorized, (message, help)) in [
        (None, unlisted),
        (Some(anonymous(false)), unlisted),
        (
            Some(anonymous(true)),
            (
                "gs://test/x/ is private: google cloud storage refused to list it without credentials, and no google application default credentials were found",
                "run `gcloud auth application-default login`, or set GOOGLE_APPLICATION_CREDENTIALS to a service account's key file",
            ),
        ),
        (
            Some(crate::net::Authorized::Token {
                bucket: "test".to_owned(),
                identity: "the google login in /home/ada/adc.json".to_owned(),
                lacks_scope: false,
            }),
            (
                "google cloud storage refused the google login in /home/ada/adc.json for gs://test/x/",
                "ask the bucket's owner to let that account list and read it",
            ),
        ),
    ] {
        let recorded = Recorded::default().on(&url, 401, fixture_bytes("bucket/gcs-denied.json"));
        let recorded = match authorized {
            Some(authorized) => recorded.authorized(&url, authorized),
            None => recorded,
        };
        let error = resolve("gs://test/x/", &recorded).unwrap_err();
        assert_eq!(error.kind, Kind::AuthRequired);
        assert_eq!(error.message, message);
        assert_eq!(error.help.as_deref(), Some(help));
        assert!(
            error.details[0].contains("required: Anonymous caller"),
            "{:?}",
            error.details
        );
    }

    // a token without the scope that reads cloud storage, as google words that
    let scoped = br#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","errors":[{"message":"Insufficient Permission","domain":"global","reason":"insufficientPermissions"}],"status":"PERMISSION_DENIED"}}"#;
    let recorded = Recorded::default()
        .on(&url, 403, scoped.to_vec())
        .authorized(
            &url,
            crate::net::Authorized::Token {
                bucket: "test".to_owned(),
                identity: "the google login in /home/ada/adc.json".to_owned(),
                lacks_scope: false,
            },
        );
    let error = resolve("gs://test/x/", &recorded).unwrap_err();
    assert_eq!(
        error.message,
        "google cloud storage refused the google login in /home/ada/adc.json for gs://test/x/: its token lacks the scope that reads cloud storage"
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "a login needs the cloud-platform scope: sign in again with `gcloud auth application-default login`, with no --scopes, or with https://www.googleapis.com/auth/cloud-platform among them"
        )
    );

    let url = STATES
        .replace("cloud-samples-data", "billed")
        .replace("bigquery%2Fus-states%2F", "x%2F");
    let billed = br#"{"error":{"code":400,"message":"Bucket is a requester pays bucket but no user project provided.","errors":[{"message":"Bucket is a requester pays bucket but no user project provided.","domain":"global","reason":"required"}]}}"#;
    let recorded = Recorded::default().on(&url, 400, billed.to_vec());
    let error = resolve("gs://billed/x/", &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert_eq!(
        error.message,
        "gs://billed/x/ bills the reader for each request, and fetchloom never agrees to pay"
    );
    assert_eq!(
        error.help.as_deref(),
        Some("use `gcloud storage cp` with --billing-project for this bucket")
    );
}

#[test]
fn missing_buckets_are_unresolved() {
    let url = listing_url(
        "https://fetchloom-no-such-bucket-8d1f2.s3.amazonaws.com/",
        "",
    );
    let recorded = Recorded::default().on(&url, 404, fixture_bytes("bucket/missing.xml"));
    let error = resolve("s3://fetchloom-no-such-bucket-8d1f2", &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert!(error.details[0].contains("NoSuchBucket"));

    let url = "https://storage.googleapis.com/storage/v1/b/fetchloom-no-such-bucket-8d1f2/o?prefix=&fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken";
    let recorded = Recorded::default().on(url, 404, fixture_bytes("bucket/gcs-missing.json"));
    let error = resolve("gs://fetchloom-no-such-bucket-8d1f2/", &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert!(error.details[0].contains("notFound"));
}

/// a listing answered with a status no case handles, such as 418, names it and keeps what the
/// service said, rather than reading as a refusal or a missing bucket.
#[test]
fn a_listing_answered_with_another_status_names_it_and_what_the_service_said() {
    let url = STATES
        .replace("cloud-samples-data", "test")
        .replace("bigquery%2Fus-states%2F", "x%2F");
    let teapot =
        br#"{"error":{"code":418,"message":"I'm a teapot","errors":[{"reason":"teapot"}]}}"#;
    let recorded = Recorded::default().on(&url, 418, teapot.to_vec());
    let error = resolve("gs://test/x/", &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::NetworkStatus);
    assert_eq!(
        error.message,
        "storage.googleapis.com answered 418 for gs://test/x/"
    );
    assert_eq!(
        error.details,
        ["storage.googleapis.com said teapot: I'm a teapot"]
    );
}

#[test]
fn escaped_keys_are_decoded_and_linked_escaped_again() {
    let url = listing_url("https://bucket.s3.amazonaws.com/", "d/");
    let body = page(
        true,
        &[
            (
                "d%2Fa+b%2Bc%26d%25.csv",
                "&quot;1beeac62f1ded905f45c6d6d20a4688b&quot;",
            ),
            ("d%2Fsub%2F%C3%A9t%C3%A9.txt", "&quot;x&quot;"),
            (
                "d%2Fempty%2F",
                "&quot;d41d8cd98f00b204e9800998ecf8427e&quot;",
            ),
        ],
    );
    let recorded = Recorded::default().on(&url, 200, body);
    let listing = resolve("s3://bucket/d/", &recorded).unwrap();
    let paths: Vec<_> = listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["a b+c&d%.csv", "sub/été.txt"]);
    assert_eq!(
        listing.files[0].url.as_str(),
        "https://bucket.s3.amazonaws.com/d/a%20b%2Bc%26d%25.csv"
    );
    assert_eq!(
        listing.files[0].md5,
        Md5::from_hex("1beeac62f1ded905f45c6d6d20a4688b")
    );
    assert_eq!(listing.files[1].md5, None);

    let body = page(false, &[("d/x &amp; y+z.csv", "\"\"")]);
    let recorded = Recorded::default().on(&url, 200, body);
    let listing = resolve("s3://bucket/d/", &recorded).unwrap();
    assert_eq!(listing.files[0].path, "x & y+z.csv");
}

#[test]
fn a_prefix_without_a_slash_names_one_object_or_one_folder() {
    let url = listing_url("https://bucket.s3.amazonaws.com/", "ds/participants.tsv");
    let body = page(
        true,
        &[
            ("ds%2Fparticipants.tsv", "\"x\""),
            ("ds%2Fparticipants.tsv.bak", "\"x\""),
        ],
    );
    let recorded = Recorded::default().on(&url, 200, body);
    let listing = resolve("s3://bucket/ds/participants.tsv", &recorded).unwrap();
    assert_eq!(listing.files.len(), 1);
    assert_eq!(listing.files[0].path, "participants.tsv");
    assert_eq!(
        listing.files[0].url.as_str(),
        "https://bucket.s3.amazonaws.com/ds/participants.tsv"
    );

    let url = listing_url("https://bucket.s3.amazonaws.com/", "ds");
    let body = page(
        true,
        &[
            ("ds%2Fa.csv", "\"x\""),
            ("ds%2Fb%2Fc.csv", "\"x\""),
            ("ds0%2Fd.csv", "\"x\""),
        ],
    );
    let recorded = Recorded::default().on(&url, 200, body);
    let listing = resolve("s3://bucket/ds", &recorded).unwrap();
    let paths: Vec<_> = listing
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    assert_eq!(paths, ["a.csv", "b/c.csv"]);
}

#[test]
fn nonempty_folder_objects_are_not_ignored_by_s3() {
    let url = listing_url("https://bucket.s3.amazonaws.com/", "d/");
    for key in ["d/", "d/payload/"] {
        let encoded = key.replace('/', "%2F");
        let body = String::from_utf8(page(
            true,
            &[("d%2Fkeep.csv", "\"x\""), (&encoded, "\"x\"")],
        ))
        .unwrap()
        .replace("<Size>0</Size>", "<Size>7</Size>");
        let recorded = Recorded::default().on(&url, 200, body);
        let error = resolve("s3://bucket/d/", &recorded).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(error.message.contains("nonempty object"));
        assert!(error.message.contains(key));
        assert_eq!(recorded.asked(), [url.as_str()]);
    }
}

#[test]
fn nonempty_folder_objects_are_not_ignored_by_gcs() {
    let url = STATES
        .replace("cloud-samples-data", "bucket")
        .replace("bigquery%2Fus-states%2F", "d%2F");
    for key in ["d/", "d/payload/"] {
        let body = serde_json::json!({"items": [
            {"name": "d/keep.csv", "size": "1", "generation": "1"},
            {"name": key, "size": "7", "generation": "1"},
        ]});
        let recorded = Recorded::default().on(&url, 200, body.to_string());
        let error = resolve("gs://bucket/d/", &recorded).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(error.message.contains("nonempty object"));
        assert!(error.message.contains(key));
        assert_eq!(recorded.asked(), [url.as_str()]);
    }
}

#[test]
fn nonempty_folder_objects_outside_the_selected_prefix_are_ignored() {
    let url = listing_url("https://bucket.s3.amazonaws.com/", "d");
    for (key, marker, path) in [
        ("d%2Fkeep.csv", "different%2Fpayload%2F", "keep.csv"),
        ("d", "d%2Fpayload%2F", "d"),
    ] {
        let body = String::from_utf8(page(true, &[(key, "\"x\""), (marker, "\"x\"")]))
            .unwrap()
            .replace("<Size>0</Size>", "<Size>7</Size>");
        let recorded = Recorded::default().on(&url, 200, body);
        let listing = resolve("s3://bucket/d", &recorded).unwrap();
        assert_eq!(listing.files.len(), 1);
        assert_eq!(listing.files[0].path, path);
    }
}

#[test]
fn an_empty_prefix_is_unresolved() {
    let url = listing_url("https://bucket.s3.amazonaws.com/", "nothing/");
    let recorded = Recorded::default().on(&url, 200, page(true, &[("nothing%2F", "\"x\"")]));
    let error = resolve("s3://bucket/nothing/", &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::ReferenceUnresolved);
    assert!(error.help.unwrap().contains("ends with /"));
}

#[test]
fn malformed_s3_pages_are_invalid_listings() {
    let url = listing_url("https://bucket.s3.amazonaws.com/", "d/");
    let truncated = String::from_utf8(page(true, &[("d%2Fa", "\"x\"")]))
        .unwrap()
        .replace("<IsTruncated>false", "<IsTruncated>true");
    let repeated = truncated.replace(
        "<IsTruncated>",
        "<NextContinuationToken>t</NextContinuationToken><IsTruncated>",
    );
    for body in [
        b"not xml".to_vec(),
        b"<!DOCTYPE x [<!ENTITY e \"e\">]><ListBucketResult>&e;</ListBucketResult>".to_vec(),
        b"<Other></Other>".to_vec(),
        String::from_utf8(page(true, &[]))
            .unwrap()
            .replace("<IsTruncated>false</IsTruncated>", "")
            .into_bytes(),
        truncated.clone().into_bytes(),
        String::from_utf8(page(true, &[("d%2Fa", "\"x\"")]))
            .unwrap()
            .replace("<Size>1</Size>", "<Size>-1</Size>")
            .into_bytes(),
        String::from_utf8(page(true, &[("d%2F%FF", "\"x\"")]))
            .unwrap()
            .into_bytes(),
    ] {
        let recorded = Recorded::default().on(&url, 200, body.clone());
        let error = resolve("s3://bucket/d/", &recorded).unwrap_err();
        assert_eq!(
            error.kind,
            Kind::SourceInvalid,
            "{}",
            String::from_utf8_lossy(&body)
        );
    }
    let next = format!("{url}&continuation-token=t");
    let recorded = Recorded::default()
        .on(&url, 200, repeated.clone())
        .on(&next, 200, repeated);
    let error = resolve("s3://bucket/d/", &recorded).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert!(error.message.contains("repeated"));
    assert_eq!(recorded.asked(), [url, next]);
}

#[test]
fn escaping_keys_reach_the_shared_listing_check() {
    let url = listing_url("https://bucket.s3.amazonaws.com/", "d/");
    let recorded = Recorded::default().on(&url, 200, page(true, &[("d%2F..%2Fx", "\"x\"")]));
    assert_eq!(
        resolve("s3://bucket/d/", &recorded).unwrap_err().kind,
        Kind::SourceInvalid
    );
}

#[test]
fn gcs_pages_list_generations_and_md5_hashes() {
    let recorded = Recorded::default()
        .fixture(STATES, "bucket/gcs-page1.json")
        .fixture(STATES_NEXT, "bucket/gcs-page2.json");
    let listing = resolve("gs://cloud-samples-data/bigquery/us-states/", &recorded).unwrap();
    assert_eq!(recorded.asked(), [STATES, STATES_NEXT]);
    assert_eq!(
        listing.resolved.to_string(),
        "gs://cloud-samples-data/bigquery/us-states/"
    );
    // the folder marker bigquery/us-states/ is no file
    assert_eq!(listing.files.len(), 10);
    assert_eq!(
        listing.files[0],
        Remote {
            path: "2021-07-02T16:04:48_70344/2021-07-02T16:04:48_70344.overall_export_metadata"
                .to_owned(),
            size: Some(116),
            sha256: None,
            md5: Md5::from_hex("ba97c17b015296f9cddde71358cac74e"),
            url: Url::parse("https://storage.googleapis.com/cloud-samples-data/bigquery/us-states/2021-07-02T16%3A04%3A48_70344/2021-07-02T16%3A04%3A48_70344.overall_export_metadata?generation=1625246960472434").unwrap(),
            named_by_server: false,
            etag: None,
        }
    );
    let csv = listing
        .files
        .iter()
        .find(|file| file.path == "us-states.csv")
        .unwrap();
    assert_eq!(csv.size, Some(688));
    assert!(
        csv.url
            .as_str()
            .contains("/bigquery/us-states/us-states.csv?generation=")
    );
}

#[test]
fn missing_gcs_generations_are_refused() {
    let url = STATES
        .replace("cloud-samples-data", "bucket")
        .replace("bigquery%2Fus-states%2F", "d%2F");
    for explicit_null in [false, true] {
        let mut body = serde_json::json!({"items": [
            {"name": "d/keep.csv", "size": "1"},
        ]});
        if explicit_null {
            body["items"][0]["generation"] = serde_json::Value::Null;
        }
        let recorded = Recorded::default().on(&url, 200, body.to_string());
        let error = resolve("gs://bucket/d/", &recorded).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(error.message.contains("generation"));
        assert!(error.message.contains("d/keep.csv"));
        assert_eq!(recorded.asked(), [url.as_str()]);
    }
}

#[test]
fn generation_errors_escape_unvalidated_object_names() {
    let url = STATES
        .replace("cloud-samples-data", "bucket")
        .replace("bigquery%2Fus-states%2F", "d%2F");
    for control in ['\n', '\u{202e}', '\u{1b}'] {
        let name = format!("d/bad{control}.csv");
        for generation in [None, Some(""), Some("not-a-number")] {
            let body = serde_json::json!({"items": [{"name": name, "size": "1", "generation": generation}]});
            let recorded = Recorded::default().on(&url, 200, body.to_string());
            let error = resolve("gs://bucket/d/", &recorded).unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid);
            assert!(!error.message.contains(control), "{:?}", error.message);
            assert!(error.message.contains(&name.escape_debug().to_string()));
        }
    }
}

#[test]
fn malformed_gcs_pages_are_invalid_listings() {
    let change = |key: &str, value: serde_json::Value| {
        let mut page: serde_json::Value =
            serde_json::from_slice(&fixture_bytes("bucket/gcs-page2.json")).unwrap();
        page["items"][1][key] = value;
        serde_json::to_vec(&page).unwrap()
    };
    for body in [
        b"{broken".to_vec(),
        change("md5Hash", "not base64".into()),
        change("size", "-1".into()),
        change("generation", "1 OR 2".into()),
        change("name", 7.into()),
    ] {
        let recorded = Recorded::default().on(STATES, 200, body.clone());
        let error = resolve("gs://cloud-samples-data/bigquery/us-states/", &recorded).unwrap_err();
        assert_eq!(
            error.kind,
            Kind::SourceInvalid,
            "{}",
            String::from_utf8_lossy(&body)
        );
    }
    let mut page: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("bucket/gcs-page1.json")).unwrap();
    page["nextPageToken"] = "same".into();
    let page = serde_json::to_vec(&page).unwrap();
    let next = format!("{STATES}&pageToken=same");
    let recorded = Recorded::default()
        .on(STATES, 200, page.clone())
        .on(&next, 200, page);
    let error = resolve("gs://cloud-samples-data/bigquery/us-states/", &recorded).unwrap_err();
    assert!(error.message.contains("repeated"));
    // empty pages taking turns between two tokens never repeat the one just before
    let page = |next: &str| format!(r#"{{"items":[],"nextPageToken":"{next}"}}"#).into_bytes();
    let (to_a, to_b) = (
        format!("{STATES}&pageToken=A"),
        format!("{STATES}&pageToken=B"),
    );
    let recorded = Recorded::default()
        .on(STATES, 200, page("A"))
        .on(&to_a, 200, page("B"))
        .on(&to_b, 200, page("A"));
    let error = resolve("gs://cloud-samples-data/bigquery/us-states/", &recorded).unwrap_err();
    assert!(error.message.contains("repeated"), "{}", error.message);
    assert_eq!(recorded.asked(), [STATES.to_owned(), to_a, to_b]);
}

#[test]
fn no_credential_is_kept_for_a_storage_host() {
    let every = |_: &str| Some(std::ffi::OsString::from("secret"));
    for credential in crate::auth::credentials(&every) {
        let host = credential.host;
        assert!(
            !host.ends_with("amazonaws.com") && !host.ends_with("googleapis.com"),
            "{host}"
        );
    }
}

#[test]
#[ignore = "live: talks to s3.amazonaws.com and storage.googleapis.com"]
fn live_buckets() {
    let work = Arc::new(crate::work::Work::default());
    let net = crate::net::Net::new(
        crate::net::Options {
            offline: crate::net::Offline::No,
            jobs: 4,
            credentials: Vec::new(),
            split: crate::net::Splitting::WhenSlow,
        },
        work.clone(),
        Arc::default(),
        Arc::new(|_| {}),
    );
    let start = Instant::now();
    let openneuro =
        block_on(reference("s3://openneuro.org/ds000001/participants.tsv").resolve(&net)).unwrap();
    assert_eq!(openneuro.files.len(), 1);
    assert_eq!(openneuro.files[0].path, "participants.tsv");
    assert!(openneuro.files[0].md5.is_some());
    let folder = block_on(reference("s3://openneuro.org/ds000001/").resolve(&net)).unwrap();
    assert!(
        folder
            .files
            .iter()
            .any(|file| file.path == "participants.tsv")
    );
    let states =
        block_on(reference("gs://cloud-samples-data/bigquery/us-states/").resolve(&net)).unwrap();
    assert!(states.files.iter().any(|file| file.path == "us-states.csv"));
    let moved = format!("s3://sentinel-cogs/{SENTINEL_PREFIX}");
    let west = block_on(reference(&moved).resolve(&net)).unwrap();
    assert!(west.files.iter().any(|file| file.path == "AOT.tif"));
    eprintln!(
        "buckets: {} + {} + {} + {} files, {} requests, {:?}",
        openneuro.files.len(),
        folder.files.len(),
        states.files.len(),
        west.files.len(),
        work.counts().requests,
        start.elapsed()
    );
}
