use std::fs;
use std::path::Path;
use std::process;
use std::sync::Arc;

use bosun_blob::{Directory, Error, Store};
use tempfile::TempDir;
use tokio::sync::Notify;

use crate::bodies::{Failing, Stalling, bytes, read};
use crate::cases::key;

async fn open() -> (TempDir, Directory) {
    let root = tempfile::tempdir().expect("a temporary directory");
    let store = Directory::open(root.path().join("store"))
        .await
        .unwrap_or_else(|error| panic!("{error}"));
    (root, store)
}

fn entries(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

async fn staged(root: &TempDir) -> Vec<String> {
    entries(&root.path().join("store/staging"))
}

conformance!(open, staged);

#[tokio::test]
async fn two_stores_on_one_root_share_their_objects() {
    let (root, first) = open().await;
    let second = Directory::open(root.path().join("store")).await.unwrap();

    first
        .put(&key("logs/r/a/e/s.log.zst"), &b"line"[..])
        .await
        .unwrap();
    let object = second.get(&key("logs/r/a/e/s.log.zst"), ..).await.unwrap();
    assert_eq!(read(object).await, b"line");
    assert_eq!(
        second.list("logs/").await.unwrap(),
        [key("logs/r/a/e/s.log.zst")]
    );

    second.delete(&key("logs/r/a/e/s.log.zst")).await.unwrap();
    assert!(matches!(
        first.get(&key("logs/r/a/e/s.log.zst"), ..).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn delete_removes_the_directories_it_empties() {
    let (root, store) = open().await;
    let objects = root.path().join("store/objects");
    store.put(&key("a/b/c"), &b"c"[..]).await.unwrap();
    store.put(&key("a/d"), &b"d"[..]).await.unwrap();
    assert_eq!(fs::read(objects.join("a/b/c")).unwrap(), b"c");

    store.delete(&key("a/b/c")).await.unwrap();
    assert_eq!(entries(&objects.join("a")), ["d"]);

    store.delete(&key("a/d")).await.unwrap();
    assert!(entries(&objects).is_empty());
}

#[tokio::test]
async fn failed_and_abandoned_puts_leave_nothing_in_staging() {
    let (root, store) = open().await;
    let staging = root.path().join("store/staging");

    let failed = store
        .put(&key("a/failed"), Failing::after(bytes(1024 * 1024, 1)))
        .await;
    assert!(failed.is_err());

    let stalled = Arc::new(Notify::new());
    let body = Stalling::after(bytes(1024 * 1024, 2), stalled.clone());
    let target = key("a/abandoned");
    let mut upload = Box::pin(store.put(&target, body));
    tokio::select! {
        result = &mut upload => panic!("the put finished without its body: {result:?}"),
        () = stalled.notified() => {}
    }
    assert_eq!(entries(&staging).len(), 1);
    drop(upload);

    store.put(&key("a/stored"), &b"stored"[..]).await.unwrap();
    assert!(entries(&staging).is_empty());
    assert_eq!(entries(&root.path().join("store/objects/a")), ["stored"]);
}

#[tokio::test]
async fn files_left_in_staging_are_neither_objects_nor_overwritten() {
    let (root, store) = open().await;
    let staging = root.path().join("store/staging");
    let left: Vec<String> = (0..256).map(|n| format!("{}-{n}", process::id())).collect();
    for name in &left {
        fs::write(staging.join(name), name).unwrap();
    }

    store.put(&key("a/stored"), &b"stored"[..]).await.unwrap();

    assert_eq!(store.list("").await.unwrap(), [key("a/stored")]);
    let mut expected = left.clone();
    expected.sort();
    assert_eq!(entries(&staging), expected);
    for name in &left {
        assert_eq!(fs::read_to_string(staging.join(name)).unwrap(), *name);
    }
}

#[tokio::test]
async fn a_key_below_an_object_or_above_others_is_refused() {
    let (root, store) = open().await;
    store.put(&key("a/b"), &b"object"[..]).await.unwrap();
    store.put(&key("x/y/z"), &b"below"[..]).await.unwrap();

    let below = store.put(&key("a/b/c"), &b"refused"[..]).await;
    assert!(matches!(below, Err(Error::Io { .. })), "{below:?}");
    let above = store.put(&key("x/y"), &b"refused"[..]).await;
    assert!(matches!(above, Err(Error::Io { .. })), "{above:?}");

    let object = store.get(&key("a/b"), ..).await.unwrap();
    assert_eq!(read(object).await, b"object");
    let object = store.get(&key("x/y/z"), ..).await.unwrap();
    assert_eq!(read(object).await, b"below");
    assert_eq!(store.list("").await.unwrap(), [key("a/b"), key("x/y/z")]);
    assert!(entries(&root.path().join("store/staging")).is_empty());
}

#[tokio::test]
async fn open_refuses_a_root_it_cannot_use() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("store");
    fs::write(&file, b"not a directory").unwrap();

    match Directory::open(&file).await {
        Err(Error::Io { target, .. }) => assert!(target.starts_with(&*file.to_string_lossy())),
        other => panic!("a file was opened as a store: {other:?}"),
    }
}
