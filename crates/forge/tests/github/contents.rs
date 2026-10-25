use bosun_forge::{Entry, EntryKind, File, Forge};

use crate::{FULL_NAME, setup};

const HELLO: &str = "hello\n";

const HELLO_BLOB: &str = "ce013625030ba8dba906f756967f9e9ca394464a";

#[tokio::test]
async fn a_directory_at_a_commit_lists_names_and_blob_hashes() {
    let setup = setup().await;
    let commit = setup.forge.commit(
        FULL_NAME,
        "main",
        [
            (".bosun/workflows/ci.yml", HELLO),
            (".bosun/workflows/shared/lint.yml", "lint\n"),
            ("README.md", "readme\n"),
        ],
    );
    let entries = setup
        .app
        .directory(&setup.repository, &commit, ".bosun/workflows")
        .await
        .unwrap_or_else(|error| panic!("the directory is not listed: {error}"))
        .unwrap_or_else(|| panic!("the directory is missing"));
    let names: Vec<(&str, EntryKind)> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry.kind))
        .collect();
    assert_eq!(
        names,
        [
            ("ci.yml", EntryKind::File),
            ("shared", EntryKind::Directory)
        ]
    );
    assert_eq!(
        entries[0],
        Entry {
            name: "ci.yml".to_owned(),
            kind: EntryKind::File,
            sha: HELLO_BLOB.to_owned(),
        }
    );
    let root = setup
        .app
        .directory(&setup.repository, &commit, "")
        .await
        .unwrap_or_else(|error| panic!("the root is not listed: {error}"))
        .unwrap_or_else(|| panic!("the root is missing"));
    let names: Vec<&str> = root.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, [".bosun", "README.md"]);
}

#[tokio::test]
async fn a_file_at_a_commit_has_its_content_and_blob_hash() {
    let setup = setup().await;
    let first = setup
        .forge
        .commit(FULL_NAME, "main", [(".bosun/workflows/ci.yml", HELLO)]);
    let second = setup.forge.commit(
        FULL_NAME,
        "main",
        [(".bosun/workflows/ci.yml", "changed\n")],
    );
    let file = setup
        .app
        .file(&setup.repository, &first, ".bosun/workflows/ci.yml")
        .await
        .unwrap_or_else(|error| panic!("the file is not read: {error}"));
    assert_eq!(
        file,
        Some(File {
            sha: HELLO_BLOB.to_owned(),
            content: HELLO.as_bytes().to_vec(),
        })
    );
    let file = setup
        .app
        .file(&setup.repository, &second, ".bosun/workflows/ci.yml")
        .await
        .unwrap_or_else(|error| panic!("the file is not read: {error}"))
        .unwrap_or_else(|| panic!("the file is missing"));
    assert_eq!(file.content, b"changed\n");
    assert_ne!(file.sha, HELLO_BLOB);
}

#[tokio::test]
async fn what_is_not_there_is_none() {
    let setup = setup().await;
    let commit = setup
        .forge
        .commit(FULL_NAME, "main", [(".bosun/workflows/ci.yml", HELLO)]);
    let repository = &setup.repository;
    let missing = setup.app.file(repository, &commit, "nope.yml").await;
    assert!(matches!(missing, Ok(None)), "{missing:?}");
    let directory = setup.app.file(repository, &commit, ".bosun").await;
    assert!(matches!(directory, Ok(None)), "{directory:?}");
    let missing = setup.app.directory(repository, &commit, "nope").await;
    assert!(matches!(missing, Ok(None)), "{missing:?}");
    let file = setup
        .app
        .directory(repository, &commit, ".bosun/workflows/ci.yml")
        .await;
    assert!(matches!(file, Ok(None)), "{file:?}");
}

#[tokio::test]
async fn a_file_above_the_inline_limit_is_read_from_its_blob() {
    let setup = setup().await;
    let large: Vec<u8> = (0..(1 << 20) + 1)
        .map(|index| (index % 251) as u8)
        .collect();
    let commit = setup
        .forge
        .commit(FULL_NAME, "main", [("large.bin", large.as_slice())]);
    let file = setup
        .app
        .file(&setup.repository, &commit, "large.bin")
        .await
        .unwrap_or_else(|error| panic!("the large file is not read: {error}"))
        .unwrap_or_else(|| panic!("the large file is missing"));
    assert_eq!(file.content.len(), large.len());
    assert!(file.content == large);
}
