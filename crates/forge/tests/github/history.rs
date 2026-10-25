use bosun_forge::Forge;
use bosun_testkit::forge::COMPARE_FILES;

use crate::{FULL_NAME, setup};

#[tokio::test]
async fn a_reference_resolves_to_its_commit() {
    let setup = setup().await;
    let first = setup.forge.commit(FULL_NAME, "main", [("a.txt", "a")]);
    let second = setup.forge.commit(FULL_NAME, "main", [("a.txt", "b")]);
    setup.forge.tag(FULL_NAME, "v1.0", &first);
    let repository = &setup.repository;
    for (reference, expected) in [
        ("main", Some(&second)),
        ("refs/heads/main", Some(&second)),
        ("v1.0", Some(&first)),
        ("refs/tags/v1.0", Some(&first)),
        (first.as_str(), Some(&first)),
        ("nope", None),
        ("", None),
    ] {
        let resolved = setup
            .app
            .resolve(repository, reference)
            .await
            .unwrap_or_else(|error| panic!("`{reference}` does not resolve: {error}"));
        assert_eq!(resolved.as_ref(), expected, "{reference}");
    }
}

#[tokio::test]
async fn a_compare_returns_the_changed_files() {
    let setup = setup().await;
    let base = setup
        .forge
        .commit(FULL_NAME, "main", [("a.txt", "a"), ("docs/b.md", "b")]);
    let head = setup
        .forge
        .commit(FULL_NAME, "main", [("a.txt", "changed"), ("src/c.rs", "c")]);
    let comparison = setup
        .app
        .compare(&setup.repository, &base, &head)
        .await
        .unwrap_or_else(|error| panic!("the commits do not compare: {error}"));
    assert_eq!(comparison.files, ["a.txt", "src/c.rs"]);
    assert!(!comparison.truncated);
}

#[tokio::test]
async fn a_compare_beyond_the_forge_limit_is_truncated() {
    let setup = setup().await;
    let base = setup.forge.commit(FULL_NAME, "main", [("a.txt", "a")]);
    let head = setup.forge.commit(
        FULL_NAME,
        "main",
        (0..=COMPARE_FILES).map(|index| (format!("src/{index:04}.rs"), "x")),
    );
    let comparison = setup
        .app
        .compare(&setup.repository, &base, &head)
        .await
        .unwrap_or_else(|error| panic!("the commits do not compare: {error}"));
    assert_eq!(comparison.files.len(), COMPARE_FILES);
    assert!(comparison.truncated);
}

#[tokio::test]
async fn pagination_returns_every_file_of_a_pull_request() {
    let setup = setup().await;
    setup
        .forge
        .commit(FULL_NAME, "main", [("README.md", "readme")]);
    let changed: Vec<String> = (0..250).map(|index| format!("src/{index:03}.rs")).collect();
    setup.forge.commit(
        FULL_NAME,
        "feature",
        changed.iter().map(|path| (path.as_str(), "x")),
    );
    let number = setup.forge.open_pull_request(FULL_NAME, "main", "feature");
    let files = setup
        .app
        .pull_request_files(&setup.repository, number)
        .await
        .unwrap_or_else(|error| panic!("the pull request's files are not listed: {error}"));
    assert_eq!(files, changed);
}
