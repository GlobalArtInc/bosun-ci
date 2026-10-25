use std::collections::BTreeMap;
use std::io::Read;

use bosun_forge::{Error, Forge, Repository};
use bosun_testkit::forge::RecordedDownload;
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

async fn unpacked(mut download: bosun_forge::Download) -> Vec<u8> {
    let mut packed = Vec::new();
    download
        .read_to_end(&mut packed)
        .await
        .unwrap_or_else(|error| panic!("the tarball does not stream: {error}"));
    packed
}

#[tokio::test]
async fn a_public_repository_resolves_and_downloads_without_credentials_and_for_another_account() {
    let setup = setup().await;
    let owner = setup.forge.install("actions", &[("contents", "read")]);
    setup.forge.create_repository(owner, "actions/checkout");
    setup.forge.publish("actions/checkout");
    let commit = setup.forge.commit(
        "actions/checkout",
        "main",
        [("action.yml", "name: checkout\n")],
    );
    setup.forge.tag("actions/checkout", "v4", &commit);
    let foreign = Repository {
        installation: setup.installation,
        owner: "actions".to_owned(),
        name: "checkout".to_owned(),
    };
    for resolved in [
        setup.app.resolve_public("actions", "checkout", "v4").await,
        setup.app.resolve(&foreign, "v4").await,
    ] {
        assert_eq!(
            resolved.unwrap_or_else(|error| panic!("`v4` does not resolve: {error}")),
            Some(commit.clone())
        );
    }
    let anonymous = unpacked(
        setup
            .app
            .public_tarball("actions", "checkout", &commit)
            .await
            .unwrap_or_else(|error| panic!("the public tarball is not downloaded: {error}")),
    )
    .await;
    let installed = unpacked(
        setup
            .app
            .tarball(&foreign, &commit)
            .await
            .unwrap_or_else(|error| panic!("the tarball is not downloaded: {error}")),
    )
    .await;
    assert_eq!(anonymous, installed);
    let downloaded = RecordedDownload {
        repository: "actions/checkout".to_owned(),
        commit,
    };
    assert_eq!(setup.forge.downloads(), [downloaded.clone(), downloaded]);
    let minted: Vec<u64> = setup
        .forge
        .tokens()
        .iter()
        .map(|token| token.installation)
        .collect();
    assert_eq!(minted, [setup.installation]);
}

#[tokio::test]
async fn a_private_repository_neither_resolves_nor_downloads_without_credentials() {
    let setup = setup().await;
    let commit = setup.forge.commit(FULL_NAME, "main", [("a.txt", "a")]);
    let resolved = setup
        .app
        .resolve_public(OWNER, NAME, "main")
        .await
        .unwrap_or_else(|error| panic!("the request failed: {error}"));
    assert_eq!(resolved, None);
    let refused = setup.app.public_tarball(OWNER, NAME, &commit).await;
    assert!(
        matches!(refused, Err(Error::Status { status: 404, .. })),
        "{refused:?}"
    );
    assert!(setup.forge.tokens().is_empty());
    assert!(setup.forge.downloads().is_empty());
}
