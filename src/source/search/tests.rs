//! captured with Invoke-WebRequest on 2026-10-31, keeping only fields read by search.

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const HF: &str =
    "https://huggingface.co/api/datasets?search=iris&limit=3&sort=downloads&direction=-1";
const KAGGLE: &str = "https://api.kaggle.com/v1/datasets/list?search=iris&page=1";
const ZENODO: &str =
    "https://zenodo.org/api/records?q=title%3A%28iris%29&type=dataset&sort=mostviewed&size=3";

fn recorded() -> Recorded {
    Recorded::default()
        .fixture(HF, "search/hf.json")
        .fixture(KAGGLE, "search/kaggle.json")
        .fixture(ZENODO, "search/zenodo.json")
}

#[test]
fn words_search_but_links_ids_dois_and_punctuation_do_not() {
    for text in [
        "iris",
        "fashion mnist",
        "uciml/iris",
        "  iris  ",
        "123",
        "\u{6570}\u{636e}\u{96c6}",
    ] {
        assert!(is_search(text), "{text}");
    }
    for text in [
        "",
        "  ",
        "---",
        "/?!",
        "zenodo:abc",
        "https://example.org/iris",
        "10.5281/zenodo.1",
        " 10.5281/zenodo.1 ",
        "doi:10.5281/zenodo.1",
    ] {
        assert!(!is_search(text), "{text}");
    }
}

#[test]
fn captured_catalogs_interleave_by_rank_and_keep_canonical_references() {
    let result = block_on(search(&recorded(), "iris")).unwrap();
    let references: Vec<_> = result
        .candidates
        .iter()
        .map(|item| item.reference.to_string())
        .collect();
    assert_eq!(
        references,
        [
            "hf:datasets/scikit-learn/iris",
            "kaggle:uciml/iris",
            "zenodo:23233774",
            "hf:datasets/fatdove/Iris_Database",
            "kaggle:himanshunakrani/iris-dataset",
            "zenodo:1319069",
            "hf:datasets/irisreto/HAL",
            "kaggle:arshid/iris-flower-dataset",
            "zenodo:4135078",
        ]
    );
    for text in references {
        assert_eq!(Reference::parse(&text).unwrap().reference.to_string(), text);
    }
    assert!(result.warnings.is_empty());
    let hf = &result.candidates[0];
    assert_eq!(hf.title, "scikit-learn/iris");
    assert_eq!(hf.source, "hugging face");
    assert_eq!(hf.size, None);
    assert_eq!(hf.downloads, Some(11912));
    assert_eq!(hf.note, None);
    let kaggle = &result.candidates[1];
    assert_eq!(kaggle.title, "Iris Species");
    assert_eq!(kaggle.source, "kaggle");
    assert_eq!(kaggle.size, Some(3687));
    assert_eq!(kaggle.downloads, Some(958555));
    let zenodo = &result.candidates[2];
    assert_eq!(zenodo.title, "IRIS: ICESat-2 River Surface Slope");
    assert_eq!(zenodo.source, "zenodo");
    assert_eq!(zenodo.size, Some(1147253099));
    assert_eq!(zenodo.downloads, Some(2400));
}

#[test]
fn private_repositories_are_skipped_and_gated_ones_marked() {
    let mut hf: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/hf.json")).unwrap();
    hf[0]["private"] = true.into();
    hf[1]["gated"] = true.into();
    hf[2]["gated"] = "manual".into();
    let api = recorded().on(HF, 200, serde_json::to_vec(&hf).unwrap());
    let result = block_on(search(&api, "iris")).unwrap();
    let hf: Vec<_> = result
        .candidates
        .iter()
        .filter(|item| item.source == "hugging face")
        .collect();
    assert_eq!(hf.len(), 2);
    assert_eq!(
        hf[0].reference.to_string(),
        "hf:datasets/fatdove/Iris_Database"
    );
    assert!(
        hf.iter()
            .all(|item| item.note.as_deref() == Some("needs access"))
    );
}

#[test]
fn a_failed_source_warns_without_discarding_the_others() {
    let api = recorded().on(KAGGLE, 403, b"denied".to_vec());
    let result = block_on(search(&api, "iris")).unwrap();
    assert_eq!(result.candidates.len(), 6);
    assert_eq!(result.candidates[1].source, "zenodo");
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].starts_with("kaggle search failed: "));
}

#[test]
fn all_failed_sources_return_the_first_error() {
    let api = recorded()
        .on(HF, 403, Vec::new())
        .on(KAGGLE, 400, Vec::new())
        .on(ZENODO, 404, Vec::new());
    let error = block_on(search(&api, "iris")).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(error.message.contains("huggingface.co"));
}

#[test]
fn empty_success_is_not_an_all_sources_failure() {
    let api = recorded()
        .on(HF, 200, b"[]".to_vec())
        .on(KAGGLE, 403, Vec::new())
        .on(ZENODO, 403, Vec::new());
    let result = block_on(search(&api, "iris")).unwrap();
    assert!(result.candidates.is_empty());
    assert_eq!(result.warnings.len(), 2);
}

#[test]
fn invalid_json_warns_instead_of_hiding_successful_catalogs() {
    let api = recorded().on(HF, 200, b"not json".to_vec());
    let result = block_on(search(&api, "iris")).unwrap();
    assert_eq!(result.candidates.len(), 6);
    assert!(result.warnings[0].starts_with("hugging face search failed: "));
}

#[test]
fn query_words_are_encoded_and_lucene_syntax_is_removed() {
    let api = Recorded::default()
        .on("https://huggingface.co/api/datasets?search=fashion+%28mnist%29%3A*%26&limit=3&sort=downloads&direction=-1", 200, b"[]".to_vec())
        .on("https://api.kaggle.com/v1/datasets/list?search=fashion+%28mnist%29%3A*%26&page=1", 200, b"[]".to_vec())
        .on("https://zenodo.org/api/records?q=title%3A%28fashion+mnist%29&type=dataset&sort=mostviewed&size=3", 200, br#"{"hits":{"hits":[]}}"#.to_vec());
    let result = block_on(search(&api, "fashion (mnist):*&")).unwrap();
    assert!(result.candidates.is_empty());
    assert!(result.warnings.is_empty());
    assert_eq!(api.asked().len(), 3);
}

#[test]
fn oversized_pages_are_limited_to_three_matches_each() {
    let mut kaggle: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/kaggle.json")).unwrap();
    let extra = kaggle[0].clone();
    kaggle.as_array_mut().unwrap().push(extra);
    let api = recorded().on(KAGGLE, 200, serde_json::to_vec(&kaggle).unwrap());
    let result = block_on(search(&api, "iris")).unwrap();
    assert_eq!(result.candidates.len(), 9);
}

#[test]
fn sources_are_asked_concurrently() {
    struct Together {
        recorded: Recorded,
        barrier: tokio::sync::Barrier,
    }
    impl Api for Together {
        async fn get(&self, url: &url::Url) -> Result<crate::net::Response, Error> {
            self.barrier.wait().await;
            self.recorded.get(url).await
        }
    }
    let api = Together {
        recorded: recorded(),
        barrier: tokio::sync::Barrier::new(3),
    };
    let result = block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(2), search(&api, "iris")).await
    })
    .unwrap()
    .unwrap();
    assert_eq!(result.candidates.len(), 9);
}

#[test]
fn json_keeps_the_reference_as_text_and_unknown_fields_as_null() {
    let result = block_on(search(&recorded(), "iris")).unwrap();
    let value = serde_json::to_value(&result.candidates[0]).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "reference": "hf:datasets/scikit-learn/iris", "title": "scikit-learn/iris",
            "source": "hugging face", "size": null, "downloads": 11912, "note": null,
        })
    );
}

#[test]
fn missing_metadata_and_overflowing_sizes_stay_unknown() {
    let body = br#"{"hits":{"hits":[
        {"id":1,"metadata":{"title":"no sizes"}},
        {"id":2,"metadata":{"title":"overflow"},"files":[{"size":18446744073709551615},{"size":1}]},
        {"id":3,"metadata":{"title":"unknown file"},"files":[{"size":1},{}]}
    ]}}"#;
    let api = recorded().on(ZENODO, 200, body.to_vec());
    let result = block_on(search(&api, "iris")).unwrap();
    let zenodo: Vec<_> = result
        .candidates
        .iter()
        .filter(|item| item.source == "zenodo")
        .collect();
    assert_eq!(zenodo.len(), 3);
    assert!(
        zenodo
            .iter()
            .all(|item| item.size.is_none() && item.downloads.is_none())
    );
}

#[test]
fn invalid_catalog_ids_never_become_candidates() {
    let api = recorded().on(
        KAGGLE,
        200,
        br#"[{"ref":"not a reference","title":"bad"}]"#.to_vec(),
    );
    let result = block_on(search(&api, "iris")).unwrap();
    assert_eq!(result.candidates.len(), 6);
    assert!(result.warnings[0].contains("invalid dataset id"));
}
