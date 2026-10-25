use std::collections::BTreeMap;
use std::io::Read;

use bosun_forge::{Error, Forge};
use flate2::read::GzDecoder;
use tokio::io::AsyncReadExt;

use crate::{FULL_NAME, NAME, OWNER, setup};

#[tokio::test]
async fn a_tarball_holds_the_tree_of_its_commit() {
    let setup = setup().await;
    let commit = setup.forge.commit(
        FULL_NAME,
        "main",
        [
            ("action.yml", "name: fixture\n"),
            ("dist/index.js", "run()\n"),
        ],
    );
    setup
        .forge
        .commit(FULL_NAME, "main", [("action.yml", "name: later\n")]);
    let mut download = setup
        .app
        .tarball(&setup.repository, &commit)
        .await
        .unwrap_or_else(|error| panic!("the tarball is not downloaded: {error}"));
    let mut packed = Vec::new();
    download
        .read_to_end(&mut packed)
        .await
        .unwrap_or_else(|error| panic!("the tarball does not stream: {error}"));
    let mut archive = tar::Archive::new(GzDecoder::new(packed.as_slice()));
    let mut files = BTreeMap::new();
    for entry in archive
        .entries()
        .unwrap_or_else(|error| panic!("the tarball is not a tar archive: {error}"))
    {
        let mut entry = entry.unwrap_or_else(|error| panic!("a tar entry is broken: {error}"));
        if entry.header().entry_type().is_dir() {
            continue;
        }
        let path = entry
            .path()
            .unwrap_or_else(|error| panic!("a tar entry has no path: {error}"))
            .to_string_lossy()
            .into_owned();
        let mut content = String::new();
        entry
            .read_to_string(&mut content)
            .unwrap_or_else(|error| panic!("{path} is not text: {error}"));
        files.insert(path, content);
    }
    let prefix = format!("{OWNER}-{NAME}-{}", &commit[..7]);
    assert_eq!(
        files,
        BTreeMap::from([
            (format!("{prefix}/action.yml"), "name: fixture\n".to_owned()),
            (format!("{prefix}/dist/index.js"), "run()\n".to_owned()),
        ])
    );
}

#[tokio::test]
async fn a_tarball_of_an_unknown_commit_is_an_error() {
    let setup = setup().await;
    setup
        .forge
        .commit(FULL_NAME, "main", [("README.md", "app")]);
    let missing = setup.app.tarball(&setup.repository, &"0".repeat(40)).await;
    assert!(
        matches!(missing, Err(Error::Status { status: 404, .. })),
        "{missing:?}"
    );
}
