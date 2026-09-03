//! Contract tests for the fixed provider help records and the function that
//! selects one for an endpoint.

use fetchloom_faults as _;
use serde_json as _;
use sha2 as _;

use rustls as _;
use rustls_graviola as _;
use ureq as _;

use fetchloom_engine::credential::{Necessity, ProviderHelp};
use fetchloom_sources::help_for;

fn steps_are_sound(record: &ProviderHelp) {
    assert!(
        record.steps.len() >= 3,
        "{} had fewer than three steps",
        record.provider
    );
    for step in &record.steps {
        if step.contains("http") {
            let words = step.split_whitespace().count();
            assert!(
                words > 3,
                "{} step looked like a bare link with no instruction: {step}",
                record.provider
            );
        }
    }
}

fn fields_are_non_empty(record: &ProviderHelp) {
    assert!(!record.provider.is_empty());
    assert!(!record.unlocks.is_empty());
    assert!(!record.steps.is_empty());
    assert!(!record.placement.is_empty());
    assert!(!record.verification.is_empty());
    assert!(!record.scope.is_empty());
}

#[test]
fn a_google_cloud_storage_host_selects_the_google_record() {
    let record = help_for("storage.googleapis.com", Necessity::Required);
    assert_eq!(record.provider, "Google Cloud Storage");
    fields_are_non_empty(&record);
    steps_are_sound(&record);
}

#[test]
fn the_google_match_is_case_insensitive_and_matches_by_suffix() {
    let record = help_for("STORAGE.GOOGLEAPIS.COM", Necessity::Optional);
    assert_eq!(record.provider, "Google Cloud Storage");

    let record = help_for("my-bucket.storage.googleapis.com", Necessity::Optional);
    assert_eq!(record.provider, "Google Cloud Storage");
}

#[test]
fn an_azure_blob_storage_host_selects_the_azure_record() {
    let record = help_for("myaccount.blob.core.windows.net", Necessity::Required);
    assert_eq!(record.provider, "Azure Blob Storage");
    fields_are_non_empty(&record);
    steps_are_sound(&record);
}

#[test]
fn the_azure_match_is_case_insensitive() {
    let record = help_for("MYACCOUNT.BLOB.CORE.WINDOWS.NET", Necessity::Optional);
    assert_eq!(record.provider, "Azure Blob Storage");
}

#[test]
fn an_amazon_s3_host_selects_the_amazon_record() {
    let record = help_for("s3.amazonaws.com", Necessity::Required);
    assert_eq!(record.provider, "Amazon S3");
    fields_are_non_empty(&record);
    steps_are_sound(&record);
}

#[test]
fn the_amazon_match_is_case_insensitive() {
    let record = help_for("MY-BUCKET.S3.AMAZONAWS.COM", Necessity::Optional);
    assert_eq!(record.provider, "Amazon S3");
}

#[test]
fn the_amazon_s3_record_names_the_credential_the_build_actually_resolves() {
    let record = help_for("s3.amazonaws.com", Necessity::Required);
    let combined = format!(
        "{} {} {}",
        record.unlocks,
        record.steps.join(" "),
        record.placement
    );
    for named in [
        "FETCHLOOM_ACCESS_KEY_S3_AMAZONAWS_COM",
        "FETCHLOOM_SECRET_KEY_S3_AMAZONAWS_COM",
        "FETCHLOOM_REGION_S3_AMAZONAWS_COM",
    ] {
        assert!(
            combined.contains(named),
            "the Amazon S3 record does not name {named}, which is what the build reads: {combined}"
        );
    }
    assert!(
        !combined.contains("cannot be fetched by name here"),
        "the Amazon S3 record still tells a user with a private bucket to go elsewhere"
    );
}

#[test]
fn an_unrecognized_host_gets_the_generic_record_rather_than_a_panic_or_an_empty_one() {
    let record = help_for("minio.example.org:9000", Necessity::Optional);
    assert_eq!(record.provider, "Object storage");
    fields_are_non_empty(&record);
    steps_are_sound(&record);
}

#[test]
fn every_record_has_at_least_three_numbered_steps_and_non_empty_fields() {
    for host in [
        "storage.googleapis.com",
        "myaccount.blob.core.windows.net",
        "s3.amazonaws.com",
        "some.other.endpoint.example",
    ] {
        let record = help_for(host, Necessity::Required);
        fields_are_non_empty(&record);
        steps_are_sound(&record);
    }
}

#[test]
fn the_placement_line_names_the_derived_environment_variable_for_a_generic_host() {
    let record = help_for("minio.example.org:9000", Necessity::Optional);
    assert!(
        record
            .placement
            .contains("FETCHLOOM_TOKEN_MINIO_EXAMPLE_ORG_9000"),
        "the placement line did not carry the derived environment variable: {}",
        record.placement
    );
}

#[test]
fn the_placement_line_names_the_derived_environment_variable_for_google_cloud_storage() {
    let record = help_for("storage.googleapis.com", Necessity::Required);
    assert!(
        record
            .placement
            .contains("FETCHLOOM_TOKEN_STORAGE_GOOGLEAPIS_COM"),
        "the placement line did not carry the derived environment variable: {}",
        record.placement
    );
}

#[test]
fn necessity_is_carried_through_from_the_argument() {
    let required = help_for("storage.googleapis.com", Necessity::Required);
    assert_eq!(required.necessity, Necessity::Required);

    let optional = help_for("storage.googleapis.com", Necessity::Optional);
    assert_eq!(optional.necessity, Necessity::Optional);

    let required = help_for("s3.amazonaws.com", Necessity::Required);
    assert_eq!(required.necessity, Necessity::Required);

    let optional = help_for("minio.example.org", Necessity::Optional);
    assert_eq!(optional.necessity, Necessity::Optional);
}
