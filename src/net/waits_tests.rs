use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use super::scripted::{reply, serve};
use super::*;
use crate::cache::Cache;

fn poll<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
}

#[tokio::test]
async fn retry_and_quota_sleeps_release_slots_and_stop_during_reacquisition() {
    for download in [false, true] {
        for status in ["429 Too Many Requests", "503 Unavailable"] {
            let (url, asked) = serve(vec![reply(status, &[("Retry-After", "5")], b"")]);
            let other = Url::parse("http://localhost:12345/healthy").unwrap();
            let (told, waiting) = tokio::sync::oneshot::channel();
            let told = Arc::new(Mutex::new(Some(told)));
            let notice = told.clone();
            let net = Net::new(
                Options {
                    offline: Offline::No,
                    jobs: 1,
                    credentials: Vec::new(),
                    split: Splitting::Never,
                },
                Arc::default(),
                Arc::default(),
                Arc::new(move |_| {
                    if let Some(told) = notice.lock().unwrap().take() {
                        let _ = told.send(());
                    }
                }),
            )
            .with_host_jobs(1);
            net.typed(&url);
            let folder = tempfile::tempdir().unwrap();
            let cache = Cache::open(folder.path().join("cache")).unwrap();
            let fetch = Fetch::new(url.clone());
            let on = |event| {
                if matches!(event, Event::Retrying { .. })
                    && let Some(told) = told.lock().unwrap().take()
                {
                    let _ = told.send(());
                }
                Ok(())
            };
            let mut requesting: Pin<Box<dyn Future<Output = Result<(), Error>> + '_>> = if download
            {
                Box::pin(async { net.download(&fetch, &cache, &on).await.map(|_| ()) })
            } else {
                Box::pin(async { net.get(&url).await.map(|_| ()) })
            };
            tokio::select! {
                result = requesting.as_mut() => panic!("the first request ended before its wait: {result:?}"),
                result = waiting => result.unwrap(),
            }
            let mut acquiring = std::pin::pin!(net.permit(&other));
            let Poll::Ready(Ok(held_by_other_host)) = poll(acquiring.as_mut()) else {
                panic!(
                    "a sleeping request still occupies the global slot: {status}, download={download}"
                );
            };
            assert_eq!(net.slots(&url).available_permits(), 1);
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(5)).await;
            assert!(poll(requesting.as_mut()).is_pending());
            assert_eq!(asked.lock().unwrap().len(), 1);
            net.stop.ask();
            let Poll::Ready(Err(error)) = poll(requesting.as_mut()) else {
                panic!("stop did not end a request waiting to retake its slots");
            };
            assert_eq!(error.kind, crate::error::Kind::Interrupted);
            assert_eq!(asked.lock().unwrap().len(), 1);
            drop(held_by_other_host);
            assert_eq!(net.jobs.available_permits(), 1);
            assert_eq!(net.slots(&url).available_permits(), 1);
            tokio::time::resume();
        }
    }
}

#[tokio::test]
async fn retry_slots_are_retaken_and_acquisition_errors_leave_none_held() {
    for closed in [false, true] {
        let net = Net::new(
            Options {
                offline: Offline::No,
                jobs: 1,
                credentials: Vec::new(),
                split: Splitting::Never,
            },
            Arc::default(),
            Arc::default(),
            Arc::new(|_| {}),
        )
        .with_host_jobs(1);
        let waiting = Url::parse("https://waiting.example/file").unwrap();
        let healthy = Url::parse("https://healthy.example/file").unwrap();
        let mut permits = Some(net.permit(&waiting).await.unwrap());
        Net::release_for_wait(&mut permits);
        assert!(permits.is_none());
        let busy = net.permit(&healthy).await.unwrap();
        {
            let mut retaking = std::pin::pin!(net.keep_permit(&waiting, &mut permits));
            assert!(poll(retaking.as_mut()).is_pending());
            if closed {
                net.jobs.close();
            }
            drop(busy);
            let result = retaking.await;
            if closed {
                assert_eq!(result.unwrap_err().kind, Kind::Interrupted);
            } else {
                result.unwrap();
                assert_eq!(net.jobs.available_permits(), 0);
                assert_eq!(net.slots(&waiting).available_permits(), 0);
            }
        }
        assert_eq!(permits.is_some(), !closed);
        drop(permits);
        assert_eq!(net.jobs.available_permits(), 1);
        assert_eq!(net.slots(&waiting).available_permits(), 1);
        assert_eq!(net.slots(&healthy).available_permits(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn a_quota_learned_while_retaking_slots_is_checked_before_sending() {
    let net = Net::new(
        Options {
            offline: Offline::No,
            jobs: 2,
            credentials: Vec::new(),
            split: Splitting::Never,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    )
    .with_host_jobs(2);
    let url = Url::parse("https://waiting.example/file").unwrap();
    let other = Url::parse("https://healthy.example/file").unwrap();
    let now = || tokio::time::Instant::now().into_std();
    let after = |seconds: &'static str| {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static(seconds));
        headers
    };
    let older = net.paces.admit(host(&url), now()).unwrap();
    let old_slots = net.permit(&url).await.unwrap();
    let first = net.paces.admit(host(&url), now()).unwrap();
    pace::Paces::heard(
        first,
        pace::limits(
            StatusCode::TOO_MANY_REQUESTS,
            &after("1"),
            SystemTime::now(),
        ),
        now(),
    );
    let mut slots = Some(net.permit(&url).await.unwrap());
    {
        let mut holding = Holding::new(&url, &mut slots);
        let mut starting = std::pin::pin!(net.pace_holding(&url, Some(&mut holding)));
        assert!(poll(starting.as_mut()).is_pending());
        let healthy = net.permit(&other).await.unwrap();
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(poll(starting.as_mut()).is_pending());
        pace::Paces::heard(
            older,
            pace::limits(
                StatusCode::TOO_MANY_REQUESTS,
                &after("5"),
                SystemTime::now(),
            ),
            now(),
        );
        drop(old_slots);
        assert!(
            poll(starting.as_mut()).is_pending(),
            "a newly free slot bypassed the newer Retry-After floor"
        );
        assert_eq!(net.jobs.available_permits(), 1);
        assert_eq!(net.slots(&url).available_permits(), 2);
        tokio::time::advance(Duration::from_secs(4)).await;
        assert!(poll(starting.as_mut()).is_pending());
        tokio::time::advance(Duration::from_secs(1)).await;
        let Poll::Ready(Ok(ticket)) = poll(starting.as_mut()) else {
            panic!("the request was not admitted after the later floor");
        };
        pace::Paces::heard(
            ticket,
            pace::limits(
                StatusCode::OK,
                &reqwest::header::HeaderMap::new(),
                SystemTime::now(),
            ),
            now(),
        );
        drop(healthy);
    }
    drop(slots);
    assert_eq!(net.jobs.available_permits(), 2);
    assert_eq!(net.slots(&url).available_permits(), 2);
}

#[tokio::test(start_paused = true)]
async fn retaking_slots_does_not_consume_a_hosts_spacing_twice() {
    let net = Net::new(
        Options {
            offline: Offline::No,
            jobs: 1,
            credentials: Vec::new(),
            split: Splitting::Never,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    )
    .with_host_jobs(1);
    let url = Url::parse("https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi").unwrap();
    let other = Url::parse("https://healthy.example/file").unwrap();
    let now = || tokio::time::Instant::now().into_std();
    let mut slots = Some(net.permit(&url).await.unwrap());
    {
        let mut holding = Holding::new(&url, &mut slots);
        let first = net.pace_holding(&url, Some(&mut holding)).await.unwrap();
        pace::Paces::heard(first, Vec::new(), now());
        let mut following = std::pin::pin!(net.pace_holding(&url, Some(&mut holding)));
        assert!(poll(following.as_mut()).is_pending());
        assert_eq!(net.jobs.available_permits(), 1);
        let busy = net.permit(&other).await.unwrap();
        tokio::time::advance(Duration::from_millis(334)).await;
        assert!(poll(following.as_mut()).is_pending());
        drop(busy);
        let Poll::Ready(Ok(ticket)) = poll(following.as_mut()) else {
            panic!("retaking slots consumed another spacing interval without sending");
        };
        pace::Paces::heard(ticket, Vec::new(), now());
    }
    drop(slots);
    assert_eq!(net.jobs.available_permits(), 1);
    let blocked = net.paces.admit(host(&url), now()).unwrap_err();
    assert_eq!(blocked.until, now() + Duration::from_millis(334));
    assert!(!blocked.announce);
}

#[tokio::test(start_paused = true)]
async fn a_one_try_hint_releases_slots_if_a_quota_is_learned_before_it_sends() {
    let (url, asked) = serve(vec![reply("200 OK", &[], b"hint")]);
    let net = Net::new(
        Options {
            offline: Offline::No,
            jobs: 2,
            credentials: Vec::new(),
            split: Splitting::Never,
        },
        Arc::default(),
        Arc::default(),
        Arc::new(|_| {}),
    )
    .with_host_jobs(2);
    net.typed(&url);
    let other = Url::parse("https://healthy.example/file").unwrap();
    let third = Url::parse("https://also-healthy.example/file").unwrap();
    let now = || tokio::time::Instant::now().into_std();
    let older = net.paces.admit(host(&url), now()).unwrap();
    let old_slots = net.permit(&url).await.unwrap();
    let busy = net.permit(&other).await.unwrap();
    let mut hint = std::pin::pin!(net.get_once(&url));
    assert!(poll(hint.as_mut()).is_pending());
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    pace::Paces::heard(
        older,
        pace::limits(StatusCode::TOO_MANY_REQUESTS, &headers, SystemTime::now()),
        now(),
    );
    drop(old_slots);
    assert!(poll(hint.as_mut()).is_pending());
    let mut acquiring = std::pin::pin!(net.permit(&third));
    let Poll::Ready(Ok(healthy)) = poll(acquiring.as_mut()) else {
        panic!("a one-try hint kept a global slot while waiting for the newly learned quota");
    };
    assert_eq!(net.slots(&url).available_permits(), 2);
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(poll(hint.as_mut()).is_pending());
    net.stop.ask();
    let Poll::Ready(Err(error)) = poll(hint.as_mut()) else {
        panic!("the stopped hint did not leave its reacquisition wait");
    };
    assert_eq!(error.kind, Kind::Interrupted);
    assert!(asked.lock().unwrap().is_empty());
    drop(healthy);
    drop(busy);
    assert_eq!(net.jobs.available_permits(), 2);
    assert_eq!(net.slots(&url).available_permits(), 2);
}
