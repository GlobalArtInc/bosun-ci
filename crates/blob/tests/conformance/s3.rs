use bosun_blob::{Error, S3, S3Settings, Store};
use bosun_testkit::TestBucket;
use tokio::task::JoinSet;

use crate::bodies::read;
use crate::cases::key;

async fn open() -> (TestBucket, S3) {
    let bucket = TestBucket::create().await;
    let store = S3::new(bucket.settings().clone()).unwrap_or_else(|error| panic!("{error}"));
    (bucket, store)
}

async fn in_progress(bucket: &TestBucket) -> Vec<String> {
    bucket
        .store()
        .uploads()
        .await
        .unwrap_or_else(|error| panic!("the uploads in progress: {error}"))
        .iter()
        .map(ToString::to_string)
        .collect()
}

conformance!(open, in_progress);

#[tokio::test]
async fn a_listing_longer_than_a_page_returns_every_key() {
    let (_bucket, store) = open().await;
    let names: Vec<String> = (0..1003).map(|n| format!("logs/r/{n:04}")).collect();
    for batch in names.chunks(32) {
        let mut puts = JoinSet::new();
        for name in batch {
            let store = store.clone();
            let name = name.clone();
            puts.spawn(async move { store.put(&key(&name), name.as_bytes()).await });
        }
        while let Some(put) = puts.join_next().await {
            put.expect("a put ran").expect("a put succeeded");
        }
    }
    store.put(&key("logs/s"), &b"outside"[..]).await.unwrap();

    let listed = store.list("logs/r/").await.unwrap();
    assert_eq!(
        listed,
        names.iter().map(|name| key(name)).collect::<Vec<_>>()
    );
    assert_eq!(store.list("logs/").await.unwrap().len(), 1004);
}

#[tokio::test]
async fn a_wrong_secret_is_refused_with_the_answer_of_the_store() {
    let (bucket, store) = open().await;
    store.put(&key("a/b"), &b"stored"[..]).await.unwrap();
    let wrong = S3::new(S3Settings {
        secret_access_key: "not-the-secret".to_owned(),
        ..bucket.settings().clone()
    })
    .unwrap();

    for result in [
        wrong.put(&key("a/c"), &b"refused"[..]).await.map(|()| None),
        wrong
            .get(&key("a/b"), ..)
            .await
            .map(|object| Some(object.size())),
        wrong.delete(&key("a/b")).await.map(|()| None),
        wrong.list("a/").await.map(|keys| Some(keys.len() as u64)),
    ] {
        match result {
            Err(Error::Request { problem, .. }) => assert!(
                problem.starts_with("the store answered 403 Forbidden"),
                "{problem}"
            ),
            other => panic!("a request with the wrong secret was not refused: {other:?}"),
        }
    }
    let object = store.get(&key("a/b"), ..).await.unwrap();
    assert_eq!(read(object).await, b"stored");
}

#[tokio::test]
async fn a_missing_bucket_is_an_error_and_not_a_missing_object() {
    let (bucket, _store) = open().await;
    let missing = S3::new(S3Settings {
        bucket: format!("{}-missing", bucket.settings().bucket),
        ..bucket.settings().clone()
    })
    .unwrap();

    for result in [
        missing.get(&key("probes/readiness"), ..).await.map(|_| ()),
        missing.delete(&key("probes/start")).await,
        missing.put(&key("probes/start"), &b""[..]).await,
        missing.list("").await.map(|_| ()),
    ] {
        match result {
            Err(Error::Request { problem, .. }) => {
                assert!(problem.contains("NoSuchBucket"), "{problem}");
            }
            other => panic!("a missing bucket was not reported: {other:?}"),
        }
    }
}
