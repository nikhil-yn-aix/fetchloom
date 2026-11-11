//! captured with Invoke-WebRequest on 2026-10-31, keeping only fields read by search.

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const HF: &str = "https://huggingface.co/api/datasets?search=iris&limit=3&sort=downloads&direction=-1&expand%5B%5D=downloads&expand%5B%5D=mainSize&expand%5B%5D=gated&expand%5B%5D=private";
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
    let result = block_on(search(&recorded(), "iris", &[], LIMIT)).unwrap();
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
    assert_eq!(result.warnings, Vec::<String>::new());
    let hf = &result.candidates[0];
    assert_eq!(hf.title, "scikit-learn/iris");
    assert_eq!(hf.source, "hugging face");
    assert_eq!(hf.size, Some(18573));
    assert_eq!(hf.downloads, Some(11912));
    assert_eq!(hf.note, None);
    let kaggle = &result.candidates[1];
    assert_eq!(kaggle.title, "Iris Species");
    assert_eq!(kaggle.source, "kaggle");
    assert_eq!(kaggle.size, Some(3687));
    assert_eq!(kaggle.downloads, Some(958_555));
    let zenodo = &result.candidates[2];
    assert_eq!(zenodo.title, "IRIS: ICESat-2 River Surface Slope");
    assert_eq!(zenodo.source, "zenodo");
    assert_eq!(zenodo.size, Some(1_147_253_099));
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
    let result = block_on(search(&api, "iris", &[], LIMIT)).unwrap();
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
    let result = block_on(search(&api, "iris", &[], LIMIT)).unwrap();
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
    let error = block_on(search(&api, "iris", &[], LIMIT)).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(error.message.contains("huggingface.co"));
}

#[test]
fn empty_success_is_not_an_all_sources_failure() {
    let api = recorded()
        .on(HF, 200, b"[]".to_vec())
        .on(KAGGLE, 403, Vec::new())
        .on(ZENODO, 403, Vec::new());
    let result = block_on(search(&api, "iris", &[], LIMIT)).unwrap();
    assert!(result.candidates.is_empty());
    assert_eq!(result.warnings.len(), 2);
}

#[test]
fn invalid_json_warns_instead_of_hiding_successful_catalogs() {
    let api = recorded().on(HF, 200, b"not json".to_vec());
    let result = block_on(search(&api, "iris", &[], LIMIT)).unwrap();
    assert_eq!(result.candidates.len(), 6);
    assert!(result.warnings[0].starts_with("hugging face search failed: "));
}

#[test]
fn query_words_are_encoded_and_lucene_syntax_is_removed() {
    let api = Recorded::default()
        .on("https://huggingface.co/api/datasets?search=fashion+%28mnist%29%3A*%26&limit=3&sort=downloads&direction=-1&expand%5B%5D=downloads&expand%5B%5D=mainSize&expand%5B%5D=gated&expand%5B%5D=private", 200, b"[]".to_vec())
        .on("https://api.kaggle.com/v1/datasets/list?search=fashion+%28mnist%29%3A*%26&page=1", 200, b"[]".to_vec())
        .on("https://zenodo.org/api/records?q=title%3A%28fashion+AND+mnist%29&type=dataset&sort=mostviewed&size=3", 200, br#"{"hits":{"hits":[]}}"#.to_vec())
        .on("https://zenodo.org/api/records?q=title%3A%28fashion+OR+mnist%29&type=dataset&sort=mostviewed&size=3", 200, br#"{"hits":{"hits":[]}}"#.to_vec());
    let result = block_on(search(&api, "fashion (mnist):*&", &[], LIMIT)).unwrap();
    assert!(result.candidates.is_empty());
    assert_eq!(result.warnings, Vec::<String>::new());
    assert_eq!(api.asked().len(), 4);
}

#[test]
fn oversized_pages_are_limited_to_three_matches_each() {
    let mut kaggle: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/kaggle.json")).unwrap();
    let extra = kaggle[0].clone();
    kaggle.as_array_mut().unwrap().push(extra);
    let api = recorded().on(KAGGLE, 200, serde_json::to_vec(&kaggle).unwrap());
    let result = block_on(search(&api, "iris", &[], LIMIT)).unwrap();
    assert_eq!(result.candidates.len(), 9);
}

/// kaggle lists a dataset it cannot serve as files, such as one kept in bigquery, with no bytes
/// and no downloads. both are unknown then, not a size of 0 B: the match stays, as a site's own
/// listing, while a dataset with bytes keeps a count of 0 downloads as kaggle gives it.
#[test]
fn kaggle_zero_bytes_are_an_unknown_size_and_count() {
    let mut kaggle: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/kaggle.json")).unwrap();
    kaggle[0]["totalBytes"] = 0.into();
    kaggle[0]["downloadCount"] = 0.into();
    kaggle[1]["downloadCount"] = 0.into();
    let api = recorded().on(KAGGLE, 200, serde_json::to_vec(&kaggle).unwrap());
    let result = block_on(search(&api, "iris", &[Catalog::Kaggle], LIMIT)).unwrap();
    let facts: Vec<_> = result
        .candidates
        .iter()
        .map(|item| (item.size, item.downloads))
        .collect();
    assert_eq!(facts[0], (None, None), "{:?}", result.candidates[0]);
    assert_eq!(facts[1].1, Some(0), "{:?}", result.candidates[1]);
    assert_eq!(result.candidates.len(), 3);
}

/// once one catalog has answered, the others get a grace period and no more, so one slow site
/// cannot hold up the matches the rest found. no grace starts before the first answer, however
/// slow every catalog is.
#[test]
fn a_slow_catalog_cannot_hold_up_the_others() {
    use std::time::Duration;

    /// answers zenodo after `zenodo`, never when none, and the other catalogs after `others`.
    struct Late {
        recorded: Recorded,
        zenodo: Option<Duration>,
        others: Duration,
    }
    impl Api for Late {
        async fn get(&self, url: &url::Url) -> Result<crate::net::Response, Error> {
            let wait = if url.host_str() == Some("zenodo.org") {
                self.zenodo
            } else {
                Some(self.others)
            };
            match wait {
                Some(wait) => tokio::time::sleep(wait).await,
                None => std::future::pending().await,
            }
            self.recorded.get(url).await
        }
    }
    let minute = Duration::from_secs(60);
    let soon = GRACE.checked_sub(Duration::from_secs(1)).unwrap();
    for (zenodo, others, found, took) in [
        (None, Duration::ZERO, 6, GRACE),
        (Some(soon), Duration::ZERO, 9, soon),
        (Some(minute), minute, 9, minute),
    ] {
        let api = Late {
            recorded: recorded(),
            zenodo,
            others,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let (result, elapsed) = runtime.block_on(async {
            let start = tokio::time::Instant::now();
            let searched = tokio::time::timeout(minute * 60, search(&api, "iris", &[], LIMIT));
            let result = searched
                .await
                .expect("the search waited for zenodo")
                .unwrap();
            (result, start.elapsed())
        });

        assert_eq!(
            (result.candidates.len(), elapsed),
            (found, took),
            "{zenodo:?}"
        );
        if found == 6 {
            assert_eq!(
                result.warnings,
                ["zenodo did not answer within 10 s of the others; \
                  `fetchloom search \"iris\" --source zenodo` waits for it"]
            );
        } else {
            assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        }
    }
}

#[test]
fn sources_are_asked_concurrently() {
    use std::future::{Future as _, poll_fn};
    use std::pin::pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Poll, Waker};

    struct Together {
        recorded: Recorded,
        started: AtomicUsize,
        can_answer: AtomicBool,
    }
    impl Api for Together {
        async fn get(&self, url: &url::Url) -> Result<crate::net::Response, Error> {
            self.started.fetch_add(1, Ordering::Relaxed);
            poll_fn(|_| {
                if self.can_answer.load(Ordering::Relaxed) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
            self.recorded.get(url).await
        }
    }
    let api = Together {
        recorded: recorded(),
        started: AtomicUsize::new(0),
        can_answer: AtomicBool::new(false),
    };
    let result = block_on(async {
        let mut pending = pin!(search(&api, "iris", &[], LIMIT));
        assert!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        assert_eq!(api.started.load(Ordering::Relaxed), 3);
        api.can_answer.store(true, Ordering::Relaxed);
        pending.await
    })
    .unwrap();
    assert_eq!(result.candidates.len(), 9);
}

#[test]
fn json_keeps_the_reference_as_text_and_unknown_fields_as_null() {
    let mut hf: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/hf.json")).unwrap();
    hf[0].as_object_mut().unwrap().remove("mainSize");
    let api = recorded().on(HF, 200, serde_json::to_vec(&hf).unwrap());
    let result = block_on(search(&api, "iris", &[], LIMIT)).unwrap();
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
    let result = block_on(search(&api, "iris", &[], LIMIT)).unwrap();
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
    let result = block_on(search(&api, "iris", &[], LIMIT)).unwrap();
    assert_eq!(result.candidates.len(), 6);
    assert!(result.warnings[0].contains("invalid dataset id"));
}

#[test]
fn chosen_catalogs_alone_are_asked() {
    let api = recorded();
    let result = block_on(search(
        &api,
        "iris",
        &[Catalog::Kaggle, Catalog::Zenodo],
        LIMIT,
    ))
    .unwrap();
    assert_eq!(api.asked().len(), 2);
    assert!(
        result
            .candidates
            .iter()
            .all(|item| item.source != "hugging face")
    );
    assert_eq!(result.candidates.len(), 6);
}

#[test]
fn a_limit_asks_each_catalog_for_that_many() {
    let api = Recorded::default()
        .on(
            "https://huggingface.co/api/datasets?search=iris&limit=1&sort=downloads&direction=-1&expand%5B%5D=downloads&expand%5B%5D=mainSize&expand%5B%5D=gated&expand%5B%5D=private",
            200,
            fixture_bytes("search/hf.json"),
        )
        .on(KAGGLE, 200, fixture_bytes("search/kaggle.json"))
        .on(
            "https://zenodo.org/api/records?q=title%3A%28iris%29&type=dataset&sort=mostviewed&size=1",
            200,
            fixture_bytes("search/zenodo.json"),
        );
    let result = block_on(search(&api, "iris", &[], 1)).unwrap();
    assert_eq!(result.candidates.len(), 3);
}

#[test]
fn zenodo_preserves_whitespace_between_words() {
    let separated = "https://zenodo.org/api/records?q=title%3A%28iris+AND+flower%29&type=dataset&sort=mostviewed&size=3";
    let joined = "https://zenodo.org/api/records?q=title%3A%28irisflower%29&type=dataset&sort=mostviewed&size=3";
    for words in [
        "Iris flower",
        "Iris\tflower",
        "Iris\nflower",
        "Iris\u{a0}flower",
        "Iris\u{2003}flower",
    ] {
        let api = Recorded::default()
            .fixture(separated, "search/zenodo.json")
            .fixture(joined, "search/zenodo.json");
        let result = block_on(search(&api, words, &[Catalog::Zenodo], LIMIT)).unwrap();
        assert_eq!(result.candidates.len(), 3);
        assert_eq!(api.asked(), [separated], "{words:?}");
    }
}

#[test]
fn zenodo_titles_must_hold_every_word_before_any_will_do() {
    let every = "https://zenodo.org/api/records?q=title%3A%28iris+AND+flower%29&type=dataset&sort=mostviewed&size=3";
    let any = "https://zenodo.org/api/records?q=title%3A%28iris+OR+flower%29&type=dataset&sort=mostviewed&size=3";
    let found = Recorded::default().fixture(every, "search/zenodo.json");
    let result = block_on(search(&found, "Iris flower", &[Catalog::Zenodo], LIMIT)).unwrap();
    assert_eq!(result.candidates.len(), 3);
    assert_eq!(found.asked(), [every]);
    let fallback = Recorded::default()
        .on(every, 200, br#"{"hits":{"hits":[]}}"#.to_vec())
        .fixture(any, "search/zenodo.json");
    let result = block_on(search(&fallback, "Iris flower", &[Catalog::Zenodo], LIMIT)).unwrap();
    assert_eq!(
        result.candidates[0].reference.to_string(),
        "zenodo:23233774"
    );
    assert_eq!(fallback.asked(), [every, any]);
}

#[test]
fn matches_named_exactly_as_the_words_come_first_in_catalog_order() {
    let mut kaggle: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/kaggle.json")).unwrap();
    kaggle[0]["ref"] = "someone/Iris_Dataset".into();
    let api = Recorded::default()
        .on(
            "https://huggingface.co/api/datasets?search=iris+dataset&limit=3&sort=downloads&direction=-1&expand%5B%5D=downloads&expand%5B%5D=mainSize&expand%5B%5D=gated&expand%5B%5D=private",
            200,
            fixture_bytes("search/hf.json"),
        )
        .on(
            "https://api.kaggle.com/v1/datasets/list?search=iris+dataset&page=1",
            200,
            serde_json::to_vec(&kaggle).unwrap(),
        )
        .on(
            "https://zenodo.org/api/records?q=title%3A%28iris+AND+dataset%29&type=dataset&sort=mostviewed&size=3",
            200,
            fixture_bytes("search/zenodo.json"),
        );
    let result = block_on(search(&api, "iris dataset", &[], LIMIT)).unwrap();
    let references: Vec<_> = result
        .candidates
        .iter()
        .map(|item| item.reference.to_string())
        .collect();
    assert_eq!(
        &references[..4],
        [
            "kaggle:someone/Iris_Dataset",
            "kaggle:himanshunakrani/iris-dataset",
            "hf:datasets/scikit-learn/iris",
            "zenodo:23233774",
        ]
    );
}
