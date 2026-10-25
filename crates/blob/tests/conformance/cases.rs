use std::ops::Bound;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bosun_blob::{Error, Key, Store};
use tokio::sync::Notify;

use crate::bodies::{Failing, Stalling, bytes, read};

macro_rules! conformance {
    (@uploads $open:path, $uploads:path; $($case:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                let (backend, store) = $open().await;
                crate::cases::$case(&store, async || $uploads(&backend).await).await;
            }
        )+
    };
    ($open:path, $uploads:path) => {
        conformance!(
            $open;
            put_then_get_returns_the_body,
            an_empty_body_makes_an_empty_object,
            put_replaces_an_existing_object,
            get_of_a_missing_key_is_not_found,
            ranged_get_returns_the_bytes_of_the_range,
            ranged_get_reads_inside_a_large_object,
            ranged_get_refuses_a_range_outside_the_object,
            delete_removes_the_object,
            delete_of_a_missing_key_succeeds,
            list_returns_the_keys_under_a_prefix_in_order,
            list_by_a_prefix_no_key_can_have_is_empty,
            keys_with_reserved_characters_round_trip,
            a_deleted_key_frees_its_place,
            a_put_that_fails_halfway_leaves_no_object,
            a_put_that_fails_halfway_keeps_the_earlier_object,
            an_abandoned_put_leaves_no_object,
            an_abandoned_put_keeps_the_earlier_object,
            concurrent_puts_to_one_key_leave_one_whole_body,
        );
        conformance!(
            @uploads $open, $uploads;
            a_multipart_put_that_fails_is_aborted_and_leaves_no_object,
            an_abandoned_multipart_put_is_aborted_and_leaves_no_object,
        );
    };
    ($open:path; $($case:ident),+ $(,)?) => {
        $(
            #[tokio::test]
            async fn $case() {
                let (_backend, store) = $open().await;
                crate::cases::$case(&store).await;
            }
        )+
    };
}

const LARGE: usize = 10 * 1024 * 1024 + 12_345;

const ABORTED: Duration = Duration::from_secs(30);

pub fn key(key: &str) -> Key {
    Key::new(key).unwrap_or_else(|error| panic!("{error}"))
}

fn keys(keys: &[&str]) -> Vec<Key> {
    keys.iter().map(|name| key(name)).collect()
}

async fn put(store: &impl Store, name: &str, body: &[u8]) {
    store
        .put(&key(name), body)
        .await
        .unwrap_or_else(|error| panic!("put {name}: {error}"));
}

async fn get(store: &impl Store, name: &str) -> Vec<u8> {
    let object = store
        .get(&key(name), ..)
        .await
        .unwrap_or_else(|error| panic!("get {name}: {error}"));
    read(object).await
}

async fn list(store: &impl Store, prefix: &str) -> Vec<Key> {
    store
        .list(prefix)
        .await
        .unwrap_or_else(|error| panic!("list {prefix}: {error}"))
}

async fn assert_missing(store: &impl Store, name: &str) {
    match store.get(&key(name), ..).await {
        Err(Error::NotFound(missing)) => assert_eq!(missing, key(name)),
        other => panic!("{name} is still visible: {other:?}"),
    }
    assert!(
        !list(store, "").await.contains(&key(name)),
        "{name} is still listed"
    );
}

pub async fn put_then_get_returns_the_body(store: &impl Store) {
    let body = bytes(LARGE, 1);
    put(store, "artifacts/p/a", &body).await;

    let object = store.get(&key("artifacts/p/a"), ..).await.unwrap();
    assert_eq!(object.size(), LARGE as u64);
    assert!(read(object).await == body, "the body came back changed");
}

pub async fn an_empty_body_makes_an_empty_object(store: &impl Store) {
    put(store, "summaries/e/s.md", b"").await;

    let object = store.get(&key("summaries/e/s.md"), ..).await.unwrap();
    assert_eq!(object.size(), 0);
    assert!(read(object).await.is_empty());
    assert_eq!(list(store, "").await, keys(&["summaries/e/s.md"]));
}

pub async fn put_replaces_an_existing_object(store: &impl Store) {
    put(store, "caches/p/e", &bytes(LARGE, 2)).await;
    put(store, "caches/p/e", b"replaced").await;

    let object = store.get(&key("caches/p/e"), ..).await.unwrap();
    assert_eq!(object.size(), 8);
    assert_eq!(read(object).await, b"replaced");
    assert_eq!(list(store, "").await, keys(&["caches/p/e"]));
}

pub async fn get_of_a_missing_key_is_not_found(store: &impl Store) {
    put(store, "logs/r/a/e/s1.log.zst", b"present").await;

    assert_missing(store, "logs/r/a/e/s2.log.zst").await;
    assert_missing(store, "logs/r/a/e").await;
    assert_missing(store, "logs/r/a/e/s1.log.zst/more").await;
}

pub async fn ranged_get_returns_the_bytes_of_the_range(store: &impl Store) {
    let body = bytes(1000, 3);
    put(store, "blocks/u/b", &body).await;
    let key = key("blocks/u/b");

    let cases: [(Bound<u64>, Bound<u64>, &[u8]); 10] = [
        (Bound::Included(10), Bound::Excluded(20), &body[10..20]),
        (Bound::Included(10), Bound::Included(19), &body[10..20]),
        (Bound::Included(990), Bound::Unbounded, &body[990..]),
        (Bound::Unbounded, Bound::Excluded(5), &body[..5]),
        (Bound::Unbounded, Bound::Unbounded, &body[..]),
        (Bound::Excluded(9), Bound::Excluded(20), &body[10..20]),
        (Bound::Included(995), Bound::Excluded(2000), &body[995..]),
        (Bound::Included(7), Bound::Excluded(7), &[]),
        (Bound::Included(1000), Bound::Unbounded, &[]),
        (Bound::Included(1000), Bound::Included(u64::MAX), &[]),
    ];
    for (start, end, expected) in cases {
        let object = store.get(&key, (start, end)).await.unwrap();
        assert_eq!(object.size(), 1000, "{start:?}..{end:?}");
        assert!(
            read(object).await == expected,
            "{start:?}..{end:?} returned other bytes"
        );
    }

    let object = store.get(&key, 100..200).await.unwrap();
    assert!(read(object).await == body[100..200]);
}

pub async fn ranged_get_reads_inside_a_large_object(store: &impl Store) {
    let body = bytes(LARGE, 4);
    put(store, "logs/r/a/e/s.log.zst", &body).await;

    let object = store
        .get(&key("logs/r/a/e/s.log.zst"), 3_000_001..7_654_321)
        .await
        .unwrap();
    assert_eq!(object.size(), LARGE as u64);
    assert!(read(object).await == body[3_000_001..7_654_321]);
}

pub async fn ranged_get_refuses_a_range_outside_the_object(store: &impl Store) {
    put(store, "a/full", &bytes(1000, 5)).await;
    put(store, "a/empty", b"").await;

    let cases = [
        (
            "a/full",
            Bound::Included(1001),
            Bound::Unbounded,
            1001,
            None,
        ),
        (
            "a/full",
            Bound::Included(20),
            Bound::Excluded(10),
            20,
            Some(10),
        ),
        (
            "a/full",
            Bound::Included(2000),
            Bound::Excluded(3000),
            2000,
            Some(3000),
        ),
        (
            "a/full",
            Bound::Excluded(u64::MAX),
            Bound::Unbounded,
            u64::MAX,
            None,
        ),
        ("a/empty", Bound::Included(1), Bound::Unbounded, 1, None),
    ];
    for (name, from, to, refused_start, refused_end) in cases {
        match store.get(&key(name), (from, to)).await {
            Err(Error::InvalidRange {
                key: refused,
                start,
                end,
                size,
            }) => {
                assert_eq!(refused, key(name));
                assert_eq!((start, end), (refused_start, refused_end));
                assert_eq!(size, if name == "a/full" { 1000 } else { 0 });
            }
            other => panic!("{name} {from:?}..{to:?} was not refused: {other:?}"),
        }
    }

    let object = store.get(&key("a/empty"), 0..).await.unwrap();
    assert!(read(object).await.is_empty());
}

pub async fn delete_removes_the_object(store: &impl Store) {
    put(store, "terminals/s.cast", b"recording").await;
    put(store, "terminals/t.cast", b"kept").await;

    store.delete(&key("terminals/s.cast")).await.unwrap();

    assert_missing(store, "terminals/s.cast").await;
    assert_eq!(get(store, "terminals/t.cast").await, b"kept");
    assert_eq!(list(store, "").await, keys(&["terminals/t.cast"]));
}

pub async fn delete_of_a_missing_key_succeeds(store: &impl Store) {
    put(store, "x/y", b"present").await;

    store.delete(&key("x/z")).await.unwrap();
    store.delete(&key("x")).await.unwrap();
    store.delete(&key("x/y/z")).await.unwrap();
    store.delete(&key("x/y")).await.unwrap();
    store.delete(&key("x/y")).await.unwrap();

    assert_missing(store, "x/y").await;
}

pub async fn list_returns_the_keys_under_a_prefix_in_order(store: &impl Store) {
    let all = [
        "a",
        "a-b/c",
        "ab",
        "logs/r1/a/s1",
        "logs/r1/a/s2",
        "logs/r1/b",
        "logs/r10/a/s1",
        "logs/r2/a",
        "summaries/e/s.md",
    ];
    for name in all.iter().rev() {
        put(store, name, name.as_bytes()).await;
    }

    let cases: [(&str, &[&str]); 12] = [
        ("", &all),
        ("a", &["a", "a-b/c", "ab"]),
        ("a/", &[]),
        ("a-b/", &["a-b/c"]),
        (
            "logs/r1",
            &["logs/r1/a/s1", "logs/r1/a/s2", "logs/r1/b", "logs/r10/a/s1"],
        ),
        ("logs/r1/", &["logs/r1/a/s1", "logs/r1/a/s2", "logs/r1/b"]),
        ("logs/r1/a/s", &["logs/r1/a/s1", "logs/r1/a/s2"]),
        ("logs/r1/a/s1", &["logs/r1/a/s1"]),
        ("logs/r1/a/s1/", &[]),
        ("logs/r3", &[]),
        ("logs/r3/", &[]),
        ("summaries/e/s.m", &["summaries/e/s.md"]),
    ];
    for (prefix, expected) in cases {
        assert_eq!(
            list(store, prefix).await,
            keys(expected),
            "prefix {prefix:?}"
        );
    }
}

pub async fn list_by_a_prefix_no_key_can_have_is_empty(store: &impl Store) {
    put(store, "a/b", b"present").await;

    for prefix in [
        "/", "/a", "a//", "./", "../", "a/../", "a/./b", "\0", "a/\n",
    ] {
        assert_eq!(list(store, prefix).await, keys(&[]), "prefix {prefix:?}");
    }
}

pub async fn keys_with_reserved_characters_round_trip(store: &impl Store) {
    let names = [
        "a b/c+d",
        "p%41/%2F",
        "q?x=1&y=2#z",
        "s:t;u,v/!*'()~$@[]",
        "\u{e9}/\u{fc}/\u{65e5}\u{672c}",
    ];
    for name in names {
        put(store, name, name.as_bytes()).await;
    }
    for name in names {
        assert_eq!(get(store, name).await, name.as_bytes(), "{name}");
    }

    let mut all = names.to_vec();
    all.sort();
    assert_eq!(list(store, "").await, keys(&all));
    assert_eq!(list(store, "p%4").await, keys(&["p%41/%2F"]));
    assert_eq!(list(store, "q?x=1&").await, keys(&["q?x=1&y=2#z"]));

    for name in names {
        store.delete(&key(name)).await.unwrap();
    }
    assert_eq!(list(store, "").await, keys(&[]));
}

pub async fn a_deleted_key_frees_its_place(store: &impl Store) {
    put(store, "x/y/z", b"deep").await;
    store.delete(&key("x/y/z")).await.unwrap();
    put(store, "x/y", b"shallow").await;
    assert_eq!(get(store, "x/y").await, b"shallow");

    store.delete(&key("x/y")).await.unwrap();
    put(store, "x/y/z", b"deep again").await;
    assert_eq!(get(store, "x/y/z").await, b"deep again");
    assert_eq!(list(store, "x").await, keys(&["x/y/z"]));
}

pub async fn a_put_that_fails_halfway_leaves_no_object(store: &impl Store) {
    let half = bytes(LARGE / 2, 6);
    let result = store
        .put(&key("artifacts/p/broken"), Failing::after(half))
        .await;

    assert!(
        matches!(result, Err(Error::Io { .. })),
        "the put did not fail: {result:?}"
    );
    assert_missing(store, "artifacts/p/broken").await;
    assert_eq!(list(store, "").await, keys(&[]));
}

pub async fn a_put_that_fails_halfway_keeps_the_earlier_object(store: &impl Store) {
    let earlier = bytes(LARGE, 7);
    put(store, "caches/p/e", &earlier).await;

    let result = store
        .put(&key("caches/p/e"), Failing::after(bytes(LARGE / 2, 8)))
        .await;

    assert!(result.is_err(), "the put did not fail");
    assert!(get(store, "caches/p/e").await == earlier);
}

pub async fn an_abandoned_put_leaves_no_object(store: &impl Store) {
    let stalled = Arc::new(Notify::new());
    let body = Stalling::after(bytes(LARGE / 2, 9), stalled.clone());
    let target = key("artifacts/p/abandoned");
    let mut upload = Box::pin(store.put(&target, body));
    tokio::select! {
        result = &mut upload => panic!("the put finished without its body: {result:?}"),
        () = stalled.notified() => {}
    }

    assert_missing(store, "artifacts/p/abandoned").await;
    drop(upload);
    assert_missing(store, "artifacts/p/abandoned").await;
    assert_eq!(list(store, "").await, keys(&[]));
}

pub async fn an_abandoned_put_keeps_the_earlier_object(store: &impl Store) {
    let earlier = bytes(LARGE, 10);
    put(store, "caches/p/e", &earlier).await;

    let stalled = Arc::new(Notify::new());
    let body = Stalling::after(bytes(LARGE / 2, 11), stalled.clone());
    let target = key("caches/p/e");
    let mut upload = Box::pin(store.put(&target, body));
    tokio::select! {
        result = &mut upload => panic!("the put finished without its body: {result:?}"),
        () = stalled.notified() => {}
    }

    assert!(get(store, "caches/p/e").await == earlier);
    drop(upload);
    assert!(get(store, "caches/p/e").await == earlier);
}

pub async fn concurrent_puts_to_one_key_leave_one_whole_body(store: &impl Store) {
    let bodies: Vec<Vec<u8>> = (0..4)
        .map(|seed| bytes(2 * 1024 * 1024, 20 + seed))
        .collect();
    let target = key("artifacts/p/raced");

    let (first, second, third, fourth) = tokio::join!(
        store.put(&target, &bodies[0][..]),
        store.put(&target, &bodies[1][..]),
        store.put(&target, &bodies[2][..]),
        store.put(&target, &bodies[3][..]),
    );
    for result in [first, second, third, fourth] {
        result.unwrap();
    }

    let stored = get(store, "artifacts/p/raced").await;
    assert!(
        bodies.contains(&stored),
        "the object is not one of the bodies"
    );
}

pub async fn a_multipart_put_that_fails_is_aborted_and_leaves_no_object(
    store: &impl Store,
    uploads: impl AsyncFn() -> Vec<String>,
) {
    let result = store
        .put(&key("artifacts/p/failed"), Failing::after(bytes(LARGE, 12)))
        .await;

    assert!(
        matches!(result, Err(Error::Io { .. })),
        "the put did not fail: {result:?}"
    );
    assert_missing(store, "artifacts/p/failed").await;
    assert_eq!(
        uploads().await,
        Vec::<String>::new(),
        "the failed put left its upload"
    );
    assert_eq!(list(store, "").await, keys(&[]));
}

pub async fn an_abandoned_multipart_put_is_aborted_and_leaves_no_object(
    store: &impl Store,
    uploads: impl AsyncFn() -> Vec<String>,
) {
    let stalled = Arc::new(Notify::new());
    let body = Stalling::after(bytes(LARGE, 13), stalled.clone());
    let target = key("artifacts/p/abandoned");
    let mut upload = Box::pin(store.put(&target, body));
    tokio::select! {
        result = &mut upload => panic!("the put finished without its body: {result:?}"),
        () = stalled.notified() => {}
    }

    assert_eq!(
        uploads().await.len(),
        1,
        "the put has no upload in progress"
    );
    assert_missing(store, "artifacts/p/abandoned").await;
    drop(upload);
    let deadline = Instant::now() + ABORTED;
    loop {
        let left = uploads().await;
        if left.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the abandoned upload was not aborted within {ABORTED:?}: {left:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_missing(store, "artifacts/p/abandoned").await;
    assert_eq!(list(store, "").await, keys(&[]));
}
