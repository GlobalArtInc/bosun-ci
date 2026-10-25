use bosun_forge::{Error, Forge};
use chrono::{DateTime, TimeDelta, Utc};

use crate::{FULL_NAME, setup};

fn reset() -> DateTime<Utc> {
    let later = Utc::now() + TimeDelta::minutes(30);
    DateTime::from_timestamp(later.timestamp(), 0)
        .unwrap_or_else(|| panic!("{later} is not a timestamp"))
}

#[tokio::test]
async fn a_rate_limited_response_becomes_the_error_with_its_reset_time() {
    let setup = setup().await;
    let commit = setup
        .forge
        .commit(FULL_NAME, "main", [("README.md", "app")]);
    let reset = reset();
    setup.forge.limit_rate(setup.installation, 0, reset);
    let limited = setup
        .app
        .file(&setup.repository, &commit, "README.md")
        .await;
    match limited {
        Err(Error::RateLimited { reset: told }) => assert_eq!(told, reset),
        other => panic!("a rate-limited read gave {other:?}"),
    }
}

#[tokio::test]
async fn a_not_modified_response_serves_the_cached_body_without_counting() {
    let setup = setup().await;
    let commit = setup.forge.commit(
        FULL_NAME,
        "main",
        [("README.md", "app"), ("src/main.rs", "fn main() {}")],
    );
    let reset = reset();
    setup.forge.limit_rate(setup.installation, 1, reset);
    for _ in 0..3 {
        let file = setup
            .app
            .file(&setup.repository, &commit, "README.md")
            .await
            .unwrap_or_else(|error| panic!("a conditional read failed: {error}"))
            .unwrap_or_else(|| panic!("the file is missing"));
        assert_eq!(file.content, b"app");
    }
    let limited = setup
        .app
        .file(&setup.repository, &commit, "src/main.rs")
        .await;
    assert!(
        matches!(limited, Err(Error::RateLimited { reset: told }) if told == reset),
        "{limited:?}"
    );
}
