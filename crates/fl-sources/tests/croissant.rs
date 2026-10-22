//! Croissant metadata whose files live only in file sets.

use fl_model::error::ErrorKind;
use fl_sources::{Croissant, Source};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::support::{self, Failure};

const SQUAD: &str = include_str!("../../../fixtures/responses/croissant/squad.json");

#[tokio::test]
async fn file_sets_inside_a_repository_point_to_the_repository_reference() -> Result<(), Failure> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/datasets/rajpurkar/squad/croissant"))
        .respond_with(ResponseTemplate::new(200).set_body_string(SQUAD))
        .mount(&server)
        .await;
    let reference = format!(
        "croissant:{}/api/datasets/rajpurkar/squad/croissant",
        server.uri()
    );
    let error = Croissant::new(support::client()?)
        .resolve(&reference.parse()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceInvalid);
    assert!(error.to_string().contains("file sets"), "{error}");
    Ok(())
}
