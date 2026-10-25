use std::fs::{self, File};
use std::io::ErrorKind;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use bosun_testkit::TestDatabase;

use crate::proxy::Proxy;
use crate::{Launch, eventually, get};

#[tokio::test]
async fn readyz_fails_while_the_database_is_unreachable_and_recovers_when_it_returns() {
    let database = TestDatabase::migrated().await;
    let proxy = Proxy::start(&database.url()).await;
    let mut server = Launch::serve(&proxy.url(&database.url())).spawn();
    let address = server.listening().await;
    assert_eq!(eventually(address, "/readyz", 200).await["status"], "ready");

    proxy.stall();
    let unready = eventually(address, "/readyz", 503).await;
    assert_eq!(unready["status"], "unready");
    assert_eq!(unready["reason"], "the database does not answer");
    assert_eq!(get(address, "/healthz").await.status(), 200);

    proxy.forward();
    assert_eq!(eventually(address, "/readyz", 200).await["status"], "ready");
}

#[tokio::test]
async fn readyz_fails_while_the_object_store_does_not_answer_and_recovers_when_it_does() {
    let database = TestDatabase::migrated().await;
    let directory = tempfile::tempdir().expect("a directory for the store");
    let store = directory.path().join("store");
    let mut server = Launch::serve(&database.url())
        .env("BOSUN_STORE_PATH", &store.to_string_lossy())
        .spawn();
    let address = server.listening().await;
    assert_eq!(eventually(address, "/readyz", 200).await["status"], "ready");

    let looping = Looping::new(&store);
    let unready = eventually(address, "/readyz", 503).await;
    drop(looping);
    assert_eq!(unready["status"], "unready");
    assert_eq!(unready["reason"], "the object store does not answer");
    assert_eq!(eventually(address, "/readyz", 200).await["status"], "ready");
}

struct Looping {
    objects: PathBuf,
    aside: PathBuf,
}

impl Looping {
    fn new(store: &Path) -> Looping {
        let objects = store.join("objects");
        let aside = store.join("objects.aside");
        fs::rename(&objects, &aside).expect("the objects of the store moved aside");
        let looping = Looping { objects, aside };
        symlink("objects", &looping.objects).expect("a link to itself in place of the objects");
        let error = File::open(looping.objects.join("probes").join("readiness"))
            .expect_err("a key of the store opened through a link to itself");
        assert!(
            !matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory),
            "a link to itself reads as a missing object: {error}"
        );
        looping
    }
}

impl Drop for Looping {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.objects);
        let _ = fs::rename(&self.aside, &self.objects);
    }
}
