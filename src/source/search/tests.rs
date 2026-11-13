//! tests on recorded catalog answers, captured with Invoke-WebRequest on 2026-10-31 and, for ENA,
//! GEO, dandi and openneuro, with curl on 2026-11-12, keeping only the fields search reads.

use super::*;
use crate::error::Kind;
use crate::source::recorded::{Recorded, block_on, fixture_bytes};

const HF: &str = "https://huggingface.co/api/datasets?search=iris&limit=3&sort=downloads&direction=-1&expand%5B%5D=downloads&expand%5B%5D=mainSize&expand%5B%5D=gated&expand%5B%5D=private";
const KAGGLE: &str = "https://api.kaggle.com/v1/datasets/list?search=iris&page=1";
const ZENODO: &str =
    "https://zenodo.org/api/records?q=title%3A%28iris%29&type=dataset&sort=mostviewed&size=3";
const ENA: &str = "https://www.ebi.ac.uk/ebisearch/ws/rest/sra-study?query=iris&size=3&format=json&fields=name%2Cdescription";
const GEO: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi?db=gds&term=iris+AND+gse%5BETYP%5D&retmax=3&retmode=json&tool=fetchloom";
const GEO_SUMMARIES: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi?db=gds&id=200318345%2C200311178%2C200320313&retmode=json&tool=fetchloom";
const DANDI: &str =
    "https://api.dandiarchive.org/api/dandisets/?search=iris&page_size=3&ordering=-stars";

/// arXiv's search for `words` in titles, then, when that finds fewer than `limit`, anywhere.
fn arxiv(words: &str, limit: usize) -> [String; 2] {
    ["ti", "all"].map(|field| {
        let mut url = Url::parse("https://export.arxiv.org/api/query").unwrap();
        url.query_pairs_mut()
            .append_pair("search_query", &format!("{field}:\"{words}\""))
            .append_pair("start", "0")
            .append_pair("max_results", &limit.to_string())
            .append_pair("sortBy", "relevance");
        url.to_string()
    })
}

/// arXiv's answer when it finds no paper.
const NO_PAPERS: &[u8] =
    b"<feed xmlns=\"http://www.w3.org/2005/Atom\"><totalResults>0</totalResults></feed>";

/// semantic scholar's search for `words`.
fn s2(words: &str, limit: usize) -> String {
    let mut url = Url::parse("https://api.semanticscholar.org/graph/v1/paper/search").unwrap();
    url.query_pairs_mut()
        .append_pair("query", words)
        .append_pair("limit", &limit.to_string())
        .append_pair(
            "fields",
            "paperId,corpusId,externalIds,url,title,year,authors,openAccessPdf",
        );
    url.to_string()
}

/// semantic scholar's answer when it finds no paper.
const NO_S2_PAPERS: &[u8] = br#"{"total":0,"offset":0,"data":[]}"#;

/// openneuro's search for `words`, a GraphQL query sent by GET with the words as a variable.
fn openneuro(words: &str, first: usize) -> String {
    let query = "query($words:[String!],$first:Int){advancedSearch(query:{keywords:$words},first:$first){edges{node{id latestSnapshot{size description{Name}} analytics{downloads}}}}}";
    let variables = format!(
        r#"{{"words":[{}],"first":{first}}}"#,
        serde_json::to_string(words).unwrap()
    );
    let mut url = Url::parse("https://openneuro.org/crn/graphql").unwrap();
    url.query_pairs_mut()
        .append_pair("query", query)
        .append_pair("variables", &variables);
    url.to_string()
}

/// the first runs of `study` in ENA's file report, which search reads to tell which of their
/// files add would take.
fn files(study: &str) -> String {
    format!(
        "https://www.ebi.ac.uk/ena/portal/api/filereport?accession={study}&result=read_run&fields=run_accession,fastq_ftp,submitted_ftp,submitted_format&format=json&limit=100"
    )
}

/// a run with FASTQ files, as ENA's file report gave SRP455996's on 2026-11-13.
const FASTQ_RUN: &[u8] = br#"[{"run_accession":"SRR25703980","fastq_ftp":"ftp.sra.ebi.ac.uk/vol1/fastq/SRR257/080/SRR25703980/SRR25703980.fastq.gz","submitted_ftp":"","submitted_format":""}]"#;

/// the captured ENA studies, each with a run of FASTQ files to add.
fn with_files(recorded: Recorded) -> Recorded {
    ["SRP455996", "SRP630995", "SRP266745"]
        .iter()
        .fold(recorded, |recorded, study| {
            recorded.on(&files(study), 200, FASTQ_RUN.to_vec())
        })
}

fn recorded() -> Recorded {
    with_files(Recorded::default())
        .fixture(HF, "search/hf.json")
        .fixture(KAGGLE, "search/kaggle.json")
        .fixture(ZENODO, "search/zenodo.json")
        .fixture(ENA, "search/ena.json")
        .fixture(GEO, "search/geo-search.json")
        .fixture(GEO_SUMMARIES, "search/geo-summary.json")
        // dandi has no dandiset for iris
        .on(DANDI, 200, br#"{"results":[]}"#.to_vec())
        .fixture(&openneuro("iris", 3), "search/openneuro.json")
        .on(&arxiv("iris", 3)[0], 200, NO_PAPERS.to_vec())
        .on(&arxiv("iris", 3)[1], 200, NO_PAPERS.to_vec())
        .on(&s2("iris", 3), 200, NO_S2_PAPERS.to_vec())
}

/// EBI's search also finds studies add cannot add as they are, so the first runs of each in ENA's
/// file report decide what search offers: FASTQ as `ena:<id>`, only submitted files as
/// `ena:submitted/<id>`, and neither, as no runs, runs with no files yet or runs all under
/// controlled access, nothing. a study whose report fails stays for add to try.
#[test]
fn ena_offers_each_study_by_the_files_add_would_take() {
    let words = "https://www.ebi.ac.uk/ebisearch/ws/rest/sra-study?query=influenza&size=7&format=json&fields=name%2Cdescription";
    let studies = [
        "ERP021368",
        "ERP010958",
        "SRP431668",
        "SRP036297",
        "PRJNA172563",
        "SRP455996",
        "SRP500000",
    ]
    .map(|study| {
        format!(r#"{{"acc":"{study}","fields":{{"name":[],"description":["{study}"]}}}}"#)
    });
    let protected = r#"{"run_accession":"SRR036297","fastq_ftp":"","submitted_ftp":"Protected file(s). Go to dbGap","submitted_format":"PROTECTED"}"#;
    let recorded = Recorded::default()
        .on(
            words,
            200,
            format!(r#"{{"entries":[{}]}}"#, studies.join(",")).into_bytes(),
        )
        // sequences alone, with no runs
        .on(&files("ERP021368"), 200, b"[\n\n]".to_vec())
        // nanopore runs ENA made no FASTQ from
        .on(
            &files("ERP010958"),
            200,
            br#"[{"run_accession":"ERR974981","fastq_ftp":"","submitted_ftp":"ftp.sra.ebi.ac.uk/vol1/run/ERR974/ERR974981/all_2d_reads.fasta.tar.gz","submitted_format":"OXFORDNANOPORE_NATIVE"}]"#.to_vec(),
        )
        .on(
            &files("SRP431668"),
            200,
            br#"[{"run_accession":"SRR24113501","fastq_ftp":"","submitted_ftp":"","submitted_format":""}]"#.to_vec(),
        )
        .on(&files("SRP036297"), 200, format!("[{protected}]").into_bytes())
        // controlled runs first, then public ones, as PRJNA172563's report lists them
        .on(
            &files("PRJNA172563"),
            200,
            format!(
                "[{protected},{}]",
                std::str::from_utf8(&FASTQ_RUN[1..FASTQ_RUN.len() - 1]).unwrap()
            )
            .into_bytes(),
        )
        .on(&files("SRP455996"), 200, FASTQ_RUN.to_vec())
        .on(&files("SRP500000"), 500, Vec::new());
    let found = block_on(search(&recorded, "influenza", &[Catalog::Ena], Some(7))).unwrap();
    let references: Vec<String> = found
        .candidates
        .iter()
        .map(|candidate| candidate.reference.to_string())
        .collect();
    assert_eq!(
        references,
        [
            "ena:submitted/ERP010958",
            "ena:PRJNA172563",
            "ena:SRP455996",
            "ena:SRP500000"
        ]
    );
}

/// the check of ENA's studies runs within the grace the search already gives, never past it: a
/// study whose report is not back when the grace ends is offered as found, and ENA's matches
/// stay however slow the reports are.
#[test]
fn ena_reports_end_with_the_grace() {
    use std::time::Duration;

    /// answers EBI's search after `answered`, never a file report, and the rest at once.
    struct Unreported {
        recorded: Recorded,
        answered: Duration,
    }
    impl Api for Unreported {
        async fn get(&self, url: &url::Url) -> Result<crate::net::Response, Error> {
            if url.path().starts_with("/ena/portal/api/") {
                std::future::pending::<()>().await;
            }
            if url.path().starts_with("/ebisearch/") {
                tokio::time::sleep(self.answered).await;
            }
            self.recorded.get(url).await
        }
    }
    let seconds = Duration::from_secs;
    for (catalogs, answered, took) in [
        // ENA alone is waited for, then its reports have the grace of a named site
        (&[Catalog::Ena][..], seconds(30), seconds(40)),
        // ENA answers a second before the grace zenodo's answer began ends
        (
            &[Catalog::Zenodo, Catalog::Ena][..],
            seconds(9),
            seconds(10),
        ),
    ] {
        let api = Unreported {
            recorded: recorded(),
            answered,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let (result, elapsed) = runtime.block_on(async {
            let start = tokio::time::Instant::now();
            let searched =
                tokio::time::timeout(seconds(3600), search(&api, "iris", catalogs, Some(LIMIT)));
            let result = searched
                .await
                .expect("the search waited for ENA's reports")
                .unwrap();
            (result, start.elapsed())
        });
        let ena: Vec<String> = result
            .candidates
            .iter()
            .filter(|candidate| candidate.source == "ENA")
            .map(|candidate| candidate.reference.to_string())
            .collect();
        assert_eq!(
            ena,
            ["ena:SRP455996", "ena:SRP630995", "ena:SRP266745"],
            "{catalogs:?}"
        );
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert_eq!(elapsed, took, "{catalogs:?}");
    }
}

/// a search without `--source` leaves out ENA and dandi, whose specialist archives answer slowly,
/// and asks each of them once it is named.
#[test]
fn ena_and_dandi_are_searched_only_when_named() {
    let hosts = |catalogs: &[Catalog]| {
        let recorded = recorded();
        block_on(search(&recorded, "iris", catalogs, Some(LIMIT))).unwrap();
        let mut hosts: Vec<String> = recorded
            .asked()
            .iter()
            .map(|asked| Url::parse(asked).unwrap().host_str().unwrap().to_owned())
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts
    };
    assert_eq!(
        hosts(&[]),
        [
            "api.kaggle.com",
            "eutils.ncbi.nlm.nih.gov",
            "huggingface.co",
            "openneuro.org",
            "zenodo.org"
        ]
    );
    assert_eq!(
        hosts(&[Catalog::Ena, Catalog::Dandi]),
        ["api.dandiarchive.org", "www.ebi.ac.uk"]
    );
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
    let result = block_on(search(&recorded(), "iris", &Catalog::ALL, Some(LIMIT))).unwrap();
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
            "ena:SRP455996",
            "geo:GSE318345",
            "openneuro:ds001037",
            "hf:datasets/fatdove/Iris_Database",
            "kaggle:himanshunakrani/iris-dataset",
            "zenodo:1319069",
            "ena:SRP630995",
            "geo:GSE311178",
            "openneuro:ds007046",
            "hf:datasets/irisreto/HAL",
            "kaggle:arshid/iris-flower-dataset",
            "zenodo:4135078",
            "ena:SRP266745",
            "geo:GSE320313",
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
    // EBI's search gives a study's title as its description, and no size or downloads
    let ena = &result.candidates[3];
    assert_eq!(ena.title, "Iris germanica Assembly");
    assert_eq!(ena.source, "ENA");
    assert_eq!((ena.size, ena.downloads), (None, None));
    let geo = &result.candidates[4];
    assert_eq!(
        geo.title,
        "Early eye organoids demonstrate cadherin and semaphorin disruption in PAX6-related aniridia"
    );
    assert_eq!(geo.source, "GEO");
    assert_eq!((geo.size, geo.downloads), (None, None));
    // openneuro's first match is a private dataset it would not show, so its second leads
    let openneuro = &result.candidates[5];
    assert_eq!(openneuro.title, "The brain of Chris");
    assert_eq!(openneuro.source, "openneuro");
    assert_eq!(openneuro.size, Some(8_600_815));
    assert_eq!(openneuro.downloads, Some(42));
}

#[test]
fn private_repositories_are_skipped_and_gated_ones_marked() {
    let mut hf: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/hf.json")).unwrap();
    hf[0]["private"] = true.into();
    hf[1]["gated"] = true.into();
    hf[2]["gated"] = "manual".into();
    let api = recorded().on(HF, 200, serde_json::to_vec(&hf).unwrap());
    let result = block_on(search(&api, "iris", &[], Some(LIMIT))).unwrap();
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
    let result = block_on(search(&api, "iris", &Catalog::ALL, Some(LIMIT))).unwrap();
    assert_eq!(result.candidates.len(), 14);
    assert_eq!(result.candidates[1].source, "zenodo");
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].starts_with("kaggle search failed: "));
}

#[test]
fn all_failed_sources_return_the_first_error() {
    let api = recorded()
        .on(HF, 403, Vec::new())
        .on(KAGGLE, 400, Vec::new())
        .on(ZENODO, 404, Vec::new())
        .on(ENA, 500, Vec::new())
        .on(GEO, 400, Vec::new())
        .on(DANDI, 404, Vec::new())
        .on(&openneuro("iris", 3), 400, Vec::new());
    let error = block_on(search(&api, "iris", &[], Some(LIMIT))).unwrap_err();
    assert_eq!(error.kind, Kind::AuthRequired);
    assert!(error.message.contains("huggingface.co"));
}

#[test]
fn empty_success_is_not_an_all_sources_failure() {
    let api = recorded()
        .on(HF, 200, b"[]".to_vec())
        .on(KAGGLE, 403, Vec::new())
        .on(ZENODO, 403, Vec::new())
        .on(ENA, 403, Vec::new())
        .on(GEO, 403, Vec::new())
        .on(DANDI, 403, Vec::new())
        .on(&openneuro("iris", 3), 403, Vec::new());
    let result = block_on(search(&api, "iris", &Catalog::ALL, Some(LIMIT))).unwrap();
    assert!(result.candidates.is_empty());
    assert_eq!(result.warnings.len(), 6);
}

#[test]
fn invalid_json_warns_instead_of_hiding_successful_catalogs() {
    let api = recorded().on(HF, 200, b"not json".to_vec());
    let result = block_on(search(&api, "iris", &Catalog::ALL, Some(LIMIT))).unwrap();
    assert_eq!(result.candidates.len(), 14);
    assert!(result.warnings[0].starts_with("hugging face search failed: "));
}

#[test]
fn query_words_are_encoded_and_lucene_syntax_is_removed() {
    let api = Recorded::default()
        .on("https://huggingface.co/api/datasets?search=fashion+%28mnist%29%3A*%26&limit=3&sort=downloads&direction=-1&expand%5B%5D=downloads&expand%5B%5D=mainSize&expand%5B%5D=gated&expand%5B%5D=private", 200, b"[]".to_vec())
        .on("https://api.kaggle.com/v1/datasets/list?search=fashion+%28mnist%29%3A*%26&page=1", 200, b"[]".to_vec())
        .on("https://zenodo.org/api/records?q=title%3A%28fashion+AND+mnist%29&type=dataset&sort=mostviewed&size=3", 200, br#"{"hits":{"hits":[]}}"#.to_vec())
        .on("https://zenodo.org/api/records?q=title%3A%28fashion+OR+mnist%29&type=dataset&sort=mostviewed&size=3", 200, br#"{"hits":{"hits":[]}}"#.to_vec())
        .on("https://www.ebi.ac.uk/ebisearch/ws/rest/sra-study?query=fashion+mnist&size=3&format=json&fields=name%2Cdescription", 200, br#"{"entries":[]}"#.to_vec())
        .on("https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi?db=gds&term=fashion+mnist+AND+gse%5BETYP%5D&retmax=3&retmode=json&tool=fetchloom", 200, br#"{"esearchresult":{"idlist":[]}}"#.to_vec())
        .on("https://api.dandiarchive.org/api/dandisets/?search=fashion+%28mnist%29%3A*%26&page_size=3&ordering=-stars", 200, br#"{"results":[]}"#.to_vec())
        .on(&openneuro("fashion (mnist):*&", 3), 200, br#"{"data":{"advancedSearch":{"edges":[]}}}"#.to_vec())
        .on(&arxiv("fashion mnist", 3)[0], 200, NO_PAPERS.to_vec())
        .on(&arxiv("fashion mnist", 3)[1], 200, NO_PAPERS.to_vec())
        .on(&s2("fashion mnist", 3), 200, NO_S2_PAPERS.to_vec());
    let result = block_on(search(
        &api,
        "fashion (mnist):*&",
        &Catalog::ALL,
        Some(LIMIT),
    ))
    .unwrap();
    assert!(result.candidates.is_empty());
    assert_eq!(result.warnings, Vec::<String>::new());
    // GEO, having found no series, asks for no summaries, and arXiv asks titles, then anywhere
    assert_eq!(api.asked().len(), 11);
}

#[test]
fn oversized_pages_are_limited_to_three_matches_each() {
    let mut kaggle: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/kaggle.json")).unwrap();
    let extra = kaggle[0].clone();
    kaggle.as_array_mut().unwrap().push(extra);
    let api = recorded().on(KAGGLE, 200, serde_json::to_vec(&kaggle).unwrap());
    let result = block_on(search(&api, "iris", &Catalog::ALL, Some(LIMIT))).unwrap();
    assert_eq!(result.candidates.len(), 17);
}

/// kaggle lists a dataset it cannot serve as files, as one kept in bigquery, with no bytes and no
/// downloads; the match stays, while a dataset with bytes keeps a count of 0 downloads.
#[test]
fn kaggle_zero_bytes_are_an_unknown_size_and_count() {
    let mut kaggle: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/kaggle.json")).unwrap();
    kaggle[0]["totalBytes"] = 0.into();
    kaggle[0]["downloadCount"] = 0.into();
    kaggle[1]["downloadCount"] = 0.into();
    let api = recorded().on(KAGGLE, 200, serde_json::to_vec(&kaggle).unwrap());
    let result = block_on(search(&api, "iris", &[Catalog::Kaggle], Some(LIMIT))).unwrap();
    let facts: Vec<_> = result
        .candidates
        .iter()
        .map(|item| (item.size, item.downloads))
        .collect();
    assert_eq!(facts[0], (None, None), "{:?}", result.candidates[0]);
    assert_eq!(facts[1].1, Some(0), "{:?}", result.candidates[1]);
    assert_eq!(result.candidates.len(), 3);
}

/// once one catalog answered, the others get a grace period and no more: 3 s in a search of every
/// site, 10 s for sites named with `--source`. no grace starts before the first answer, however
/// slow every catalog is.
#[test]
fn a_slow_catalog_cannot_hold_up_the_others() {
    use std::time::Duration;

    /// answers zenodo's first request after `zenodo`, never when none, and each other catalog's
    /// first after `others`; later requests, as GEO's for its summaries, are answered at once.
    struct Late {
        recorded: Recorded,
        zenodo: Option<Duration>,
        others: Duration,
        answered: std::sync::Mutex<std::collections::HashSet<String>>,
    }
    impl Api for Late {
        async fn get(&self, url: &url::Url) -> Result<crate::net::Response, Error> {
            let host = url.host_str().unwrap_or_default().to_owned();
            let first = self.answered.lock().unwrap().insert(host.clone());
            let wait = if !first {
                Some(Duration::ZERO)
            } else if host == "zenodo.org" {
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
    let (every, named) = (Duration::from_secs(3), Duration::from_secs(10));
    // zenodo's answer a second before each grace ends
    let (every_soon, named_soon) = (Duration::from_secs(2), Duration::from_secs(9));
    for (catalogs, zenodo, others, found, took) in [
        (&[][..], None, Duration::ZERO, 11, every),
        (&[][..], Some(every_soon), Duration::ZERO, 14, every_soon),
        (&Catalog::ALL[..], None, Duration::ZERO, 14, named),
        (
            &Catalog::ALL[..],
            Some(named_soon),
            Duration::ZERO,
            17,
            named_soon,
        ),
        (&[][..], Some(minute), minute, 14, minute),
    ] {
        let api = Late {
            recorded: recorded(),
            zenodo,
            others,
            answered: std::sync::Mutex::default(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        let (result, elapsed) = runtime.block_on(async {
            let start = tokio::time::Instant::now();
            let searched =
                tokio::time::timeout(minute * 60, search(&api, "iris", catalogs, Some(LIMIT)));
            let result = searched
                .await
                .expect("the search waited for zenodo")
                .unwrap();
            (result, start.elapsed())
        });

        assert_eq!(
            (result.candidates.len(), elapsed),
            (found, took),
            "{catalogs:?} {zenodo:?}"
        );
        if zenodo.is_none() {
            assert_eq!(
                result.warnings,
                [format!(
                    "zenodo did not answer within {} s of the others; \
                     `fetchloom search \"iris\" --source zenodo` waits for it",
                    took.as_secs()
                )]
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
        let mut pending = pin!(search(&api, "iris", &Catalog::ALL, Some(LIMIT)));
        assert!(
            pending
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        assert_eq!(api.started.load(Ordering::Relaxed), 9);
        api.can_answer.store(true, Ordering::Relaxed);
        pending.await
    })
    .unwrap();
    assert_eq!(result.candidates.len(), 17);
}

#[test]
fn json_keeps_the_reference_as_text_and_unknown_fields_as_null() {
    let mut hf: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/hf.json")).unwrap();
    hf[0].as_object_mut().unwrap().remove("mainSize");
    let api = recorded().on(HF, 200, serde_json::to_vec(&hf).unwrap());
    let result = block_on(search(&api, "iris", &[], Some(LIMIT))).unwrap();
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
    let result = block_on(search(&api, "iris", &[], Some(LIMIT))).unwrap();
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
    let result = block_on(search(&api, "iris", &Catalog::ALL, Some(LIMIT))).unwrap();
    assert_eq!(result.candidates.len(), 14);
    assert!(result.warnings[0].contains("invalid dataset id"));
}

/// papers are no datasets, so a search asks arXiv and semantic scholar only when `--source` names
/// them, as it asks ENA and dandi.
#[test]
fn paper_catalogs_are_asked_only_by_name() {
    assert_eq!(
        Catalog::asked(&[]),
        [
            Catalog::Hf,
            Catalog::Kaggle,
            Catalog::Zenodo,
            Catalog::Geo,
            Catalog::OpenNeuro
        ]
    );
    assert_eq!(Catalog::asked(&[Catalog::Arxiv]), [Catalog::Arxiv]);
    assert_eq!(Catalog::asked(&[Catalog::S2]), [Catalog::S2]);
}

#[test]
fn chosen_catalogs_alone_are_asked() {
    let api = recorded();
    let result = block_on(search(
        &api,
        "iris",
        &[Catalog::Kaggle, Catalog::Zenodo],
        Some(LIMIT),
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
    let api = with_files(Recorded::default())
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
        )
        .fixture(
            "https://www.ebi.ac.uk/ebisearch/ws/rest/sra-study?query=iris&size=1&format=json&fields=name%2Cdescription",
            "search/ena.json",
        )
        .fixture(
            "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi?db=gds&term=iris+AND+gse%5BETYP%5D&retmax=1&retmode=json&tool=fetchloom",
            "search/geo-search.json",
        )
        .fixture(
            "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi?db=gds&id=200318345&retmode=json&tool=fetchloom",
            "search/geo-summary.json",
        )
        .fixture(
            "https://api.dandiarchive.org/api/dandisets/?search=iris&page_size=1&ordering=-stars",
            "search/dandi.json",
        )
        .fixture(&openneuro("iris", 1), "search/openneuro.json")
        .fixture(&arxiv("iris", 1)[0], "arxiv/paper.xml")
        .fixture(&s2("iris", 1), "s2/search.json");
    let result = block_on(search(&api, "iris", &Catalog::ALL, Some(1))).unwrap();
    assert_eq!(result.candidates.len(), 9);
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
        let result = block_on(search(&api, words, &[Catalog::Zenodo], Some(LIMIT))).unwrap();
        assert_eq!(result.candidates.len(), 3);
        assert_eq!(api.asked(), [separated], "{words:?}");
    }
}

#[test]
fn zenodo_titles_must_hold_every_word_before_any_will_do() {
    let every = "https://zenodo.org/api/records?q=title%3A%28iris+AND+flower%29&type=dataset&sort=mostviewed&size=3";
    let any = "https://zenodo.org/api/records?q=title%3A%28iris+OR+flower%29&type=dataset&sort=mostviewed&size=3";
    let found = Recorded::default().fixture(every, "search/zenodo.json");
    let result = block_on(search(
        &found,
        "Iris flower",
        &[Catalog::Zenodo],
        Some(LIMIT),
    ))
    .unwrap();
    assert_eq!(result.candidates.len(), 3);
    assert_eq!(found.asked(), [every]);
    let fallback = Recorded::default()
        .on(every, 200, br#"{"hits":{"hits":[]}}"#.to_vec())
        .fixture(any, "search/zenodo.json");
    let result = block_on(search(
        &fallback,
        "Iris flower",
        &[Catalog::Zenodo],
        Some(LIMIT),
    ))
    .unwrap();
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
    let three = [Catalog::Hf, Catalog::Kaggle, Catalog::Zenodo];
    let result = block_on(search(&api, "iris dataset", &three, Some(LIMIT))).unwrap();
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

/// fetchloom cannot download an embargoed dandiset, and one with no assets has nothing to give; one
/// never published is described by its draft, which fetchloom then reads.
#[test]
fn dandi_leaves_out_embargoed_and_empty_dandisets_and_reads_a_draft() {
    let mut dandi: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/dandi.json")).unwrap();
    dandi["results"][0]["embargo_status"] = "EMBARGOED".into();
    dandi["results"][1]["most_recent_published_version"] = serde_json::Value::Null;
    dandi["results"][1]["draft_version"]["name"] = "the draft's title".into();
    dandi["results"][1]["draft_version"]["size"] = 7.into();
    let mut empty = dandi["results"][2].clone();
    empty["identifier"] = "000999".into();
    empty["most_recent_published_version"] = serde_json::Value::Null;
    empty["draft_version"]["asset_count"] = 0.into();
    dandi["results"].as_array_mut().unwrap().insert(2, empty);
    let api = Recorded::default().on(
        "https://api.dandiarchive.org/api/dandisets/?search=hippocampus&page_size=3&ordering=-stars",
        200,
        serde_json::to_vec(&dandi).unwrap(),
    );
    let result = block_on(search(&api, "hippocampus", &[Catalog::Dandi], Some(LIMIT))).unwrap();
    let found: Vec<_> = result
        .candidates
        .iter()
        .map(|item| (item.reference.to_string(), item.title.as_str(), item.size))
        .collect();
    assert_eq!(
        found,
        [
            ("dandi:000044".to_owned(), "the draft's title", Some(7)),
            (
                "dandi:001361".to_owned(),
                "A flexible hippocampal population code for experience relative to reward",
                Some(92_448_350_544)
            ),
        ]
    );
    assert!(
        result
            .candidates
            .iter()
            .all(|item| item.source == "dandi" && item.downloads.is_none())
    );
}

/// openneuro answers a GET query only with the header guarding it against other sites; errors with
/// no data fail the catalog, while errors beside data, as for a private dataset, do not.
#[test]
fn openneuro_is_asked_with_its_header_and_fails_on_errors_alone() {
    let url = openneuro("iris", 3);
    let api = Recorded::default().on(
        &url,
        200,
        br#"{"errors":[{"message":"Cannot query field \"advancedSearch\" on type \"Query\".\nmore"}],"data":null}"#
            .to_vec(),
    );
    let error = block_on(search(&api, "iris", &[Catalog::OpenNeuro], Some(LIMIT))).unwrap_err();
    assert_eq!(error.kind, Kind::SourceInvalid);
    assert_eq!(
        error.message,
        "openneuro.org answered the search with an error: Cannot query field \"advancedSearch\" on type \"Query\"."
    );
    assert_eq!(
        api.asked(),
        [format!("{url} apollo-require-preflight: true")]
    );
}

/// a search without a limit shows at most [`MOST`] matches, here zenodo's third among those named,
/// and counts what it left out, which is nothing when fewer were found.
#[test]
fn a_search_without_a_limit_keeps_each_catalogs_best_then_names_then_ranks() {
    let mut zenodo: serde_json::Value =
        serde_json::from_slice(&fixture_bytes("search/zenodo.json")).unwrap();
    zenodo["hits"]["hits"][2]["metadata"]["title"] = "Iris".into();
    let api = recorded().on(ZENODO, 200, serde_json::to_vec(&zenodo).unwrap());
    let result = block_on(search(&api, "iris", &Catalog::ALL, None)).unwrap();
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
            "zenodo:4135078",
            "zenodo:23233774",
            "ena:SRP455996",
            "geo:GSE318345",
            "openneuro:ds001037",
            "hf:datasets/fatdove/Iris_Database",
            "kaggle:himanshunakrani/iris-dataset",
            "zenodo:1319069",
            "ena:SRP630995",
            "geo:GSE311178",
        ]
    );
    assert_eq!(result.left_out, 5);
    let three = [Catalog::Hf, Catalog::Kaggle, Catalog::Zenodo];
    let result = block_on(search(&recorded(), "iris", &three, None)).unwrap();
    assert_eq!((result.candidates.len(), result.left_out), (9, 0));
}

/// here eight matches past the first are named as the words, and two of them give way.
#[test]
fn every_catalogs_best_stays_however_many_others_are_named_as_the_words() {
    let named = |fixture: &str, edit: &dyn Fn(&mut serde_json::Value)| {
        let mut answer: serde_json::Value =
            serde_json::from_slice(&fixture_bytes(fixture)).unwrap();
        edit(&mut answer);
        serde_json::to_vec(&answer).unwrap()
    };
    let api = recorded()
        .on(
            KAGGLE,
            200,
            named("search/kaggle.json", &|kaggle| {
                for at in [1, 2] {
                    kaggle[at]["title"] = "Iris".into();
                }
            }),
        )
        .on(
            ZENODO,
            200,
            named("search/zenodo.json", &|zenodo| {
                for at in [1, 2] {
                    zenodo["hits"]["hits"][at]["metadata"]["title"] = "Iris".into();
                }
            }),
        )
        .on(
            ENA,
            200,
            named("search/ena.json", &|ena| {
                for at in [1, 2] {
                    ena["entries"][at]["fields"]["description"] = serde_json::json!(["Iris"]);
                }
            }),
        )
        .on(
            GEO_SUMMARIES,
            200,
            named("search/geo-summary.json", &|geo| {
                for id in ["200311178", "200320313"] {
                    geo["result"][id]["title"] = "Iris".into();
                }
            }),
        );
    let result = block_on(search(&api, "iris", &Catalog::ALL, None)).unwrap();
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
            "kaggle:himanshunakrani/iris-dataset",
            "zenodo:1319069",
            "ena:SRP630995",
            "geo:GSE311178",
            "kaggle:arshid/iris-flower-dataset",
            "zenodo:4135078",
            "zenodo:23233774",
            "ena:SRP455996",
            "geo:GSE318345",
            "openneuro:ds001037",
        ]
    );
    assert_eq!(result.left_out, 5);
}
