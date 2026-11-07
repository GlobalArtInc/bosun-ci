use axum::http::{Method, StatusCode};
use chrono::Duration;

use bosun_blob::Directory;
use bosun_services::{AddressError, AddressSigner, MemoryIndex, Services};

use crate::{Harness, block, identity};

fn tamper(signature: &str) -> String {
    let first = if signature.starts_with('A') { 'B' } else { 'A' };
    format!("{first}{}", &signature[1..])
}

fn split(address: &str) -> (&str, &str, &str) {
    let (path, query) = address.split_once('?').expect("a query");
    let (expires, signature) = query.split_once("&sig=").expect("a signature");
    (
        path,
        expires.strip_prefix("expires=").expect("an expiry"),
        signature,
    )
}

#[tokio::test]
async fn a_signed_address_is_refused_with_403_unless_it_is_intact_unexpired_and_for_this_object_and_use()
 {
    let harness = Harness::new().await;
    let who = identity(1);
    let token = harness.token(who);
    let first = harness.create(&token, who, "first").await;
    let second = harness.create(&token, who, "second").await;
    let (first_path, first_expires, first_signature) = split(&first);
    let (second_path, _, _) = split(&second);

    let intact = harness.stage(&first, &block(1), b"data").await;
    assert_eq!(intact.status, StatusCode::CREATED);

    let altered = [
        format!(
            "{first_path}?expires={}&sig={first_signature}",
            first_expires.parse::<i64>().expect("seconds") + 1
        ),
        format!(
            "{first_path}?expires={first_expires}&sig={}",
            tamper(first_signature)
        ),
        format!(
            "{first_path}?expires={first_expires}&sig={}",
            &first_signature[..20]
        ),
        format!("{first_path}?expires={first_expires}"),
        format!("{first_path}?sig={first_signature}"),
        format!("{first_path}?expires=0{first_expires}&sig={first_signature}"),
        format!(
            "{first_path}?expires={first_expires}&expires={first_expires}&sig={first_signature}"
        ),
        format!("{first_path}?expires=-1&sig={first_signature}"),
        first_path.to_owned(),
        format!("{second_path}?expires={first_expires}&sig={first_signature}"),
    ];
    for address in altered {
        let refused = harness.stage(&address, &block(1), b"data").await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{address}");
    }

    harness.advance(Duration::hours(1) - Duration::seconds(1));
    assert_eq!(
        harness.stage(&first, &block(2), b"data").await.status,
        StatusCode::CREATED
    );
    harness.advance(Duration::seconds(1));
    assert_eq!(
        harness.stage(&first, &block(3), b"data").await.status,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn an_address_for_one_purpose_is_refused_for_the_other() {
    let harness = Harness::new().await;
    let (upload, download) = harness.publish("report", b"content").await;

    let read_with_upload = harness.blob(Method::GET, &upload, "", &[], b"").await;
    assert_eq!(read_with_upload.status, StatusCode::FORBIDDEN);
    let head_with_upload = harness.blob(Method::HEAD, &upload, "", &[], b"").await;
    assert_eq!(head_with_upload.status, StatusCode::FORBIDDEN);
    let write_with_download = harness
        .blob(
            Method::PUT,
            &download,
            "",
            &[("x-ms-blob-type", "BlockBlob")],
            b"other",
        )
        .await;
    assert_eq!(write_with_download.status, StatusCode::FORBIDDEN);

    let read = harness.blob(Method::GET, &download, "", &[], b"").await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.body, b"content");

    let late_write = harness
        .blob(
            Method::PUT,
            &upload,
            "",
            &[("x-ms-blob-type", "BlockBlob")],
            b"other",
        )
        .await;
    assert_eq!(late_write.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_short_secret_or_a_public_url_that_is_not_a_bare_origin_is_refused() {
    assert_eq!(
        AddressSigner::new(&[7; 31]).err(),
        Some(AddressError::ShortSecret)
    );
    assert!(AddressSigner::new(&[7; 32]).is_ok());

    let directory = tempfile::TempDir::new().expect("a directory");
    for url in [
        "ftp://bosun.example",
        "https://bosun.example/base",
        "https://bosun.example/?x=1",
        "https://user:pass@bosun.example",
        "bosun.example",
        "",
    ] {
        let store = Directory::open(directory.path()).await.expect("a store");
        let signer = AddressSigner::new(&[7; 32]).expect("a secret");
        assert!(
            Services::new(store, MemoryIndex::new(), signer, url).is_err(),
            "{url}"
        );
    }
}
