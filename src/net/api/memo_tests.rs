use std::sync::Arc;

use super::*;
use crate::net::scripted::{reply, route, serve};
use crate::net::{Offline, Options, Splitting};
use crate::source::recorded::block_on;

fn net(url: &Url) -> Net {
    let mut net = Net::new(
        Options {
            offline: Offline::No,
            jobs: 4,
            credentials: Vec::new(),
            split: Splitting::Never,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    );
    net.first_wait = std::time::Duration::ZERO;
    net.typed(url);
    net
}

#[test]
fn a_successful_listing_is_requested_once_and_its_bytes_are_shared() {
    let (url, asked) = serve(vec![reply("200 OK", &[], b"listing"); 2]);
    let net = net(&url);
    let (first, second) = block_on(async {
        let first = net.get(&url).await.unwrap();
        let second = net.get(&url).await.unwrap();
        (first, second)
    });
    assert_eq!(first.text(), "listing");
    assert_eq!(second.text(), "listing");
    assert_eq!(asked.lock().unwrap().len(), 1);
    assert_eq!(first.body.as_ptr(), second.body.as_ptr());
}

#[test]
fn identical_in_flight_listings_share_the_request() {
    let (url, asked) = serve(vec![reply("200 OK", &[], b"listing"); 4]);
    let net = net(&url);
    block_on(async {
        let (a, b, c, d) =
            tokio::join!(net.get(&url), net.get(&url), net.get(&url), net.get(&url),);
        for response in [a, b, c, d] {
            assert_eq!(response.unwrap().text(), "listing");
        }
    });
    assert_eq!(asked.lock().unwrap().len(), 1);
}

#[test]
fn a_refused_listing_is_not_kept_but_a_later_success_is() {
    for status in ["401 Unauthorized", "403 Forbidden", "404 Not Found"] {
        let (url, asked) = serve(vec![
            reply(status, &[], b"refused"),
            reply("200 OK", &[], b"found"),
            reply("200 OK", &[], b"unexpected"),
        ]);
        let net = net(&url);
        block_on(async {
            assert_eq!(net.get(&url).await.unwrap().text(), "refused");
            assert_eq!(net.get(&url).await.unwrap().text(), "found");
            assert_eq!(net.get(&url).await.unwrap().text(), "found");
        });
        assert_eq!(asked.lock().unwrap().len(), 2);
    }
}

#[test]
fn listing_urls_and_page_metadata_stay_separate() {
    let (url, asked) = route(|target| {
        reply(
            "200 OK",
            &[
                ("Link", "<next?cursor=more>; rel=next"),
                ("Content-Range", "items 0-0/2"),
            ],
            target.as_bytes(),
        )
    });
    let net = net(&url);
    let first = url.join("listing?cursor=1").unwrap();
    let second = url.join("listing?cursor=2").unwrap();
    block_on(async {
        for page in [&first, &second, &first, &second] {
            let response = net.get(page).await.unwrap();
            assert_eq!(response.url, *page);
            assert_eq!(response.total, Some(2));
            assert_eq!(
                response.text(),
                format!("/listing?{}", page.query().unwrap())
            );
            assert_eq!(response.next, Some(url.join("next?cursor=more").unwrap()));
        }
    });
    assert_eq!(asked.lock().unwrap().len(), 2);
}

#[test]
fn one_try_hints_do_not_use_or_replace_the_listing_memo() {
    let (url, asked) = serve(vec![
        reply("200 OK", &[], b"listing"),
        reply("200 OK", &[], b"hint"),
        reply("200 OK", &[], b"unexpected"),
    ]);
    let net = net(&url);
    block_on(async {
        assert_eq!(net.get(&url).await.unwrap().text(), "listing");
        assert_eq!(net.get_once(&url).await.unwrap().text(), "hint");
        assert_eq!(net.get(&url).await.unwrap().text(), "listing");
    });
    assert_eq!(asked.lock().unwrap().len(), 2);
}

#[test]
fn memo_hits_cannot_bypass_offline_or_mirror_only_guards() {
    let (url, asked) = serve(vec![reply("200 OK", &[], b"listing")]);
    let mut net = net(&url);
    block_on(net.get(&url)).unwrap();
    net.offline = Offline::Flag;
    assert_eq!(
        block_on(net.get(&url)).unwrap_err().kind,
        Kind::OfflineMissing
    );
    net.offline = Offline::No;
    net = net.with_mirrors_only(vec![url.clone()]);
    assert_eq!(
        block_on(net.get(&url)).unwrap_err().kind,
        Kind::NetworkRefused
    );
    assert_eq!(asked.lock().unwrap().len(), 1);
}

#[test]
fn a_changed_cookie_scope_cannot_reuse_the_previous_listing() {
    let (url, asked) = serve(vec![
        reply("200 OK", &[], b"anonymous"),
        reply("200 OK", &[], b"signed in"),
    ]);
    let net = net(&url);
    block_on(async {
        assert_eq!(net.get(&url).await.unwrap().text(), "anonymous");
        net.wire()
            .unwrap()
            .cookies
            .add_cookie_str("login=one; Path=/", &url);
        assert_eq!(net.get(&url).await.unwrap().text(), "signed in");
        assert_eq!(net.get(&url).await.unwrap().text(), "signed in");
    });
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 2);
    assert!(!asked[0].contains("cookie:"));
    assert!(asked[1].contains("cookie: login=one"));
}

fn response(url: &Url, status: u16, body: &[u8]) -> Response {
    Response {
        status,
        url: url.clone(),
        next: None,
        total: None,
        drive: None,
        region: None,
        keyed: None,
        authorized: None,
        body: Arc::new(body.to_vec()),
    }
}

fn key(net: &Net, url: &Url) -> memo::Key {
    block_on(net.metadata_key(url, false, &[]))
        .unwrap()
        .unwrap()
}

#[test]
fn credential_values_and_bucket_scopes_cannot_share_a_memo_entry() {
    let url = Url::parse("https://example.org/listing").unwrap();
    let mut net = net(&url);
    let anonymous = key(&net, &url);
    net.credentials.push(crate::net::Key {
        host: "example.org".to_owned(),
        port: 443,
        name: header::AUTHORIZATION,
        value: header::HeaderValue::from_static("Bearer first"),
        every_hop: false,
    });
    let first = key(&net, &url);
    net.credentials[0].value = header::HeaderValue::from_static("Bearer second");
    let second = key(&net, &url);
    let mut bucket = second.clone();
    bucket.authorized = Some(crate::net::Authorized::Token {
        bucket: "private-one".to_owned(),
        identity: "one account".to_owned(),
        lacks_scope: false,
    });
    let mut other_bucket = bucket.clone();
    other_bucket.authorized = Some(crate::net::Authorized::Token {
        bucket: "private-two".to_owned(),
        identity: "one account".to_owned(),
        lacks_scope: false,
    });
    let store = Memo::new(16 << 10);
    for (index, key) in [anonymous, first, second, bucket, other_bucket]
        .into_iter()
        .enumerate()
    {
        let memo::Found::Fetching(claim) = store.find(key.clone()) else {
            panic!("a new credential scope reused an old response");
        };
        let body = index.to_string();
        claim.keep(key.clone(), &response(&url, 200, body.as_bytes()));
        let memo::Found::Ready(found) = store.find(key) else {
            panic!("the matching scope was not retained");
        };
        assert_eq!(found.text(), body);
    }
}

#[test]
fn oversized_responses_are_shared_in_flight_but_not_retained() {
    let url = Url::parse("https://example.org/listing").unwrap();
    let key = key(&net(&url), &url);
    let store = Memo::new(2048);
    let memo::Found::Fetching(claim) = store.find(key.clone()) else {
        panic!("the small request key did not fit");
    };
    let memo::Found::Waiting(waiting) = store.find(key.clone()) else {
        panic!("the duplicate did not join the flight");
    };
    let answer = response(&url, 200, &[b'x'; 2048]);
    claim.keep(key.clone(), &answer);
    block_on(waiting.finished());
    let shared = waiting.response(&key).unwrap();
    assert!(Arc::ptr_eq(&answer.body, &shared.body));
    assert!(matches!(store.find(key), memo::Found::Fetching(_)));
}

#[test]
fn cancelling_a_memo_owner_wakes_waiters_without_caching_an_error() {
    let url = Url::parse("https://example.org/listing").unwrap();
    let key = key(&net(&url), &url);
    let store = Memo::new(4096);
    let memo::Found::Fetching(claim) = store.find(key.clone()) else {
        panic!("the owner was not admitted");
    };
    let memo::Found::Waiting(waiting) = store.find(key.clone()) else {
        panic!("the duplicate did not wait");
    };
    let mut notified = std::pin::pin!(waiting.finished());
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(notified.as_mut().poll(&mut context).is_pending());
    drop(claim);
    assert!(notified.as_mut().poll(&mut context).is_ready());
    assert!(waiting.response(&key).is_none());
    assert!(matches!(store.find(key), memo::Found::Fetching(_)));
}

#[test]
fn a_small_memo_evicts_old_bodies_and_never_retains_a_refusal() {
    let url = Url::parse("https://example.org/listing").unwrap();
    let net = net(&url);
    let store = Memo::new(4096);
    let mut keys = Vec::new();
    for index in 0..8 {
        let url = url.join(&index.to_string()).unwrap();
        let key = key(&net, &url);
        let memo::Found::Fetching(claim) = store.find(key.clone()) else {
            panic!("an entry was not admitted after eviction");
        };
        claim.keep(key.clone(), &response(&url, 200, &[b'x'; 512]));
        keys.push(key);
    }
    assert!(matches!(
        store.find(keys[0].clone()),
        memo::Found::Fetching(_)
    ));
    assert!(matches!(store.find(keys[7].clone()), memo::Found::Ready(_)));
    let failed = key(&net, &url);
    let memo::Found::Fetching(claim) = store.find(failed.clone()) else {
        panic!("the refusal test did not get a fresh request");
    };
    claim.keep(failed.clone(), &response(&url, 403, b"refused"));
    assert!(matches!(store.find(failed), memo::Found::Fetching(_)));
}

#[test]
fn a_stop_ends_even_a_cached_listing() {
    let (url, asked) = serve(vec![reply("200 OK", &[], b"listing")]);
    let net = net(&url);
    block_on(net.get(&url)).unwrap();
    net.stop.ask();
    assert_eq!(block_on(net.get(&url)).unwrap_err().kind, Kind::Interrupted);
    assert_eq!(asked.lock().unwrap().len(), 1);
}

#[test]
fn refused_then_signed_listings_are_never_reused_as_anonymous_or_other_keys() {
    let (base, asked) = serve(vec![
        reply("403 Forbidden", &[], b"private"),
        reply("200 OK", &[], b"first account"),
        reply("403 Forbidden", &[], b"private"),
        reply("200 OK", &[], b"second account"),
    ]);
    let url = base.join("private-bucket/?list-type=2").unwrap();
    let bucket = crate::source::Bucket::parse(&format!("s3:{base}private-bucket")).unwrap();
    let endpoint = base.origin().ascii_serialization();
    let signing = |access: &'static str| {
        let endpoint = endpoint.clone();
        crate::source::Signing::new(
            vec![bucket.clone()],
            Arc::new(move |name: &str| match name {
                "AWS_ACCESS_KEY_ID" => Some(access.into()),
                "AWS_SECRET_ACCESS_KEY" => Some("test-secret".into()),
                "AWS_ENDPOINT_URL_S3" => Some(endpoint.clone().into()),
                _ => None,
            }),
        )
    };
    let mut net = net(&url).with_signing(signing("AKIAFIRST"));
    let anonymous = key(&net, &url);
    let first = block_on(net.get(&url)).unwrap();
    assert_eq!(first.text(), "first account");
    assert!(matches!(
        first.keyed,
        Some(crate::source::Keyed::Signed { .. })
    ));
    assert!(matches!(
        net.metadata.find(anonymous),
        memo::Found::Fetching(_)
    ));
    net = net.with_signing(signing("AKIASECOND"));
    let second = block_on(net.get(&url)).unwrap();
    assert_eq!(second.text(), "second account");
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 4);
    assert!(!asked[0].contains("authorization:"));
    assert!(asked[1].contains("Credential=AKIAFIRST/"));
    assert!(!asked[2].contains("authorization:"));
    assert!(asked[3].contains("Credential=AKIASECOND/"));
}

#[test]
fn refreshed_google_tokens_and_scopes_change_the_metadata_identity() {
    for drive in [false, true] {
        let folder = tempfile::tempdir().unwrap();
        let credentials = folder.path().join("google.json");
        std::fs::write(
        &credentials,
        br#"{"type":"authorized_user","client_id":"test-client","client_secret":"test-secret","refresh_token":"test-refresh"}"#,
    ).unwrap();
        let (endpoint, asked) = serve(vec![
            reply(
                "200 OK",
                &[],
                br#"{"access_token":"first-token","expires_in":3600,"token_type":"Bearer"}"#,
            ),
            reply(
                "200 OK",
                &[],
                br#"{"access_token":"second-token","expires_in":3600,"token_type":"Bearer"}"#,
            ),
        ]);
        let named = credentials.into_os_string();
        let google = crate::net::Google::new(
            vec![crate::source::GcsBucket::parse("private-bucket").unwrap()],
            Arc::new(move |name: &str| {
                (name == "GOOGLE_APPLICATION_CREDENTIALS").then(|| named.clone())
            }),
        )
        .with_drive(vec![
            crate::source::DriveId::parse("private_folder_123456789").unwrap(),
        ])
        .with_token_endpoint(Some(endpoint));
        let url = Url::parse(if drive {
            "https://www.googleapis.com/drive/v3/files/private_folder_123456789?fields=id"
        } else {
            "https://storage.googleapis.com/storage/v1/b/private-bucket/o"
        })
        .unwrap();
        let net = net(&url).with_google(google);
        let anonymous = key(&net, &url);
        let located = net.google.locate(&url).unwrap();
        if drive {
            assert_eq!(
                block_on(net.drive_refused("private_folder_123456789")).unwrap(),
                crate::net::Private::Ready
            );
        } else {
            assert!(net.google.refused(&located).unwrap());
        }
        let first = key(&net, &url);
        assert!(first != anonymous);
        let store = Memo::new(16 << 10);
        let memo::Found::Fetching(claim) = store.find(first.clone()) else {
            panic!("the first token did not get its own request");
        };
        claim.keep(
            first.clone(),
            &response(&url, 200, b"first token's listing"),
        );
        block_on(async {
            let token = net.google.bearer(&net, &located).await.unwrap().unwrap();
            net.google.forget(&located, &token).await;
        });
        let refreshed = key(&net, &url);
        assert!(refreshed != first);
        assert!(matches!(store.find(refreshed), memo::Found::Fetching(_)));
        assert_eq!(asked.lock().unwrap().len(), 2);
    }
}
#[test]
fn a_cookie_change_during_a_request_invalidates_its_memo_identity() {
    let url = Url::parse("https://example.org/listing").unwrap();
    let net = net(&url);
    let before = key(&net, &url);
    assert!(before.sent.current(net.wire().unwrap()));
    net.wire()
        .unwrap()
        .cookies
        .add_cookie_str("login=one; Path=/", &url);
    assert!(!before.sent.current(net.wire().unwrap()));
    let after = key(&net, &url);
    assert!(after.sent.current(net.wire().unwrap()));
    assert!(before != after);
}

#[test]
fn a_gone_download_does_not_reuse_the_listing_that_led_to_it() {
    for mirror in [false, true] {
        let (url, asked) = serve(vec![
            reply("200 OK", &[], b"old link"),
            reply("404 Not Found", &[], b"gone"),
            reply("200 OK", &[], b"new link"),
        ]);
        let net = net(&url);
        let folder = tempfile::tempdir().unwrap();
        let cache = crate::cache::Cache::open(folder.path().join("cache")).unwrap();
        let mut fetch = crate::net::Fetch::new(url.join("gone").unwrap());
        fetch.mirror = mirror;
        block_on(async {
            assert_eq!(net.get(&url).await.unwrap().text(), "old link");
            let error = net.download(&fetch, &cache, &|_| Ok(())).await.unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceUnresolved);
            let expected = if mirror { "old link" } else { "new link" };
            assert_eq!(net.get(&url).await.unwrap().text(), expected);
        });
        assert_eq!(asked.lock().unwrap().len(), if mirror { 2 } else { 3 });
    }
}

#[test]
fn an_invalidated_flight_cannot_repopulate_the_memo() {
    let url = Url::parse("https://example.org/listing").unwrap();
    let net = net(&url);
    let identity = key(&net, &url);
    let store = Memo::new(16 << 10);
    let memo::Found::Fetching(old) = store.find(identity.clone()) else {
        panic!("the first request did not own its listing");
    };
    store.invalidate();
    old.keep(identity.clone(), &response(&url, 200, b"old link"));
    let memo::Found::Fetching(fresh) = store.find(identity.clone()) else {
        panic!("an invalidated request kept its stale answer");
    };
    fresh.keep(identity.clone(), &response(&url, 200, b"new link"));
    let memo::Found::Ready(found) = store.find(identity) else {
        panic!("the fresh answer was not retained");
    };
    assert_eq!(found.text(), "new link");
}

#[test]
fn drive_item_scopes_cannot_share_a_metadata_entry() {
    let url =
        Url::parse("https://www.googleapis.com/drive/v3/files/first_file_123456789?fields=id")
            .unwrap();
    let other =
        Url::parse("https://www.googleapis.com/drive/v3/files/second_file_123456789?fields=id")
            .unwrap();
    let net = net(&url);
    let first = key(&net, &url);
    let mut second = key(&net, &other);
    second.sent = first.sent.clone();
    let store = Memo::new(16 << 10);
    let memo::Found::Fetching(claim) = store.find(first.clone()) else {
        panic!("the first item was not admitted");
    };
    claim.keep(first, &response(&url, 200, b"first item"));
    assert!(matches!(store.find(second), memo::Found::Fetching(_)));
}

#[test]
fn azure_sas_values_and_anonymous_requests_cannot_share_a_metadata_entry() {
    let (local, asked) = serve(vec![
        reply("403 Forbidden", &[], b"private"),
        reply("200 OK", &[], b"listing"),
        reply("403 Forbidden", &[], b"private"),
        reply("200 OK", &[], b"listing"),
    ]);
    let url =
        Url::parse("https://lab.blob.core.windows.net/raw?restype=container&comp=list").unwrap();
    let azure = |sas: &'static str| {
        crate::net::Azure::new(
            vec![crate::source::AzureContainer::parse("azure:lab/raw").unwrap()],
            Arc::new(move |name: &str| (name == "AZURE_STORAGE_SAS_TOKEN").then(|| sas.into())),
        )
    };
    let mut net = net(&url).with_azure(azure("sv=2022-11-02&sr=c&sp=rl&sig=first"));
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let authorize = |net: &Net| {
        let sent = block_on(net.send(&url, |request| {
            let request = request.build().unwrap();
            let mut target = local.join(request.url().path()).unwrap();
            target.set_query(request.url().query());
            client.get(target).headers(request.headers().clone())
        }));
        assert!(sent.is_ok(), "the local Azure handshake failed");
    };
    let anonymous = key(&net, &url);
    authorize(&net);
    let first = key(&net, &url);
    assert!(first != anonymous);
    let store = Memo::new(16 << 10);
    let memo::Found::Fetching(claim) = store.find(first.clone()) else {
        panic!("the first SAS did not own its listing");
    };
    claim.keep(first.clone(), &response(&url, 200, b"first SAS"));
    assert!(matches!(store.find(anonymous), memo::Found::Fetching(_)));
    net = net.with_azure(azure("sv=2022-11-02&sr=c&sp=rl&sig=second"));
    authorize(&net);
    let second = key(&net, &url);
    assert!(second != first);
    assert!(matches!(store.find(second), memo::Found::Fetching(_)));
    let asked = asked.lock().unwrap();
    assert_eq!(asked.len(), 4);
    assert!(!asked[0].contains("sig="));
    assert!(asked[1].contains("sig=first"));
    assert!(!asked[2].contains("sig="));
    assert!(asked[3].contains("sig=second"));
    assert!(
        asked
            .iter()
            .all(|request| request.contains("x-ms-version:"))
    );
}

/// a query sent by GET with a header the server asks for, as a GraphQL server's guard against
/// cross-site requests does, carries that header and goes by its link and headers as any
/// request does: each query gets its own answer, the same query again in a run gets the one
/// already read, and a plain GET of the same link is asked anew, without the header.
#[test]
fn a_query_sent_with_headers_never_gets_the_answer_to_another() {
    let (url, asked) = route(|target| reply("200 OK", &[], target.as_bytes()));
    let net = net(&url);
    let preflight: &[(&str, &str)] = &[("apollo-require-preflight", "true")];
    let first = url.join("graphql?query=one").unwrap();
    let second = url.join("graphql?query=two").unwrap();
    block_on(async {
        for (link, said) in [
            (&first, "/graphql?query=one"),
            (&second, "/graphql?query=two"),
            (&first, "/graphql?query=one"),
        ] {
            assert_eq!(net.get_with(link, preflight).await.unwrap().text(), said);
        }
        assert_eq!(net.get(&first).await.unwrap().text(), "/graphql?query=one");
    });
    let heads: Vec<String> = asked
        .lock()
        .unwrap()
        .iter()
        .map(|head| head.to_ascii_lowercase())
        .collect();
    assert_eq!(heads.len(), 3, "{heads:?}");
    assert!(
        heads[..2]
            .iter()
            .all(|head| head.contains("apollo-require-preflight: true")),
        "{heads:?}"
    );
    assert!(!heads[2].contains("apollo-require-preflight"), "{heads:?}");
}
