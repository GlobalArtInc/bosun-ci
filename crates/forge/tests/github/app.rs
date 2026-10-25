use bosun_forge::{Account, App, Error, Forge, GitHubApp};
use bosun_testkit::forge::{APP_ID, APP_SLUG, BOT_ID, FOREIGN_KEY};
use url::Url;

use crate::setup;

#[tokio::test]
async fn the_app_names_its_slug_and_its_bot() {
    let setup = setup().await;
    let app = setup
        .app
        .app()
        .await
        .unwrap_or_else(|error| panic!("the App has no identity: {error}"));
    assert_eq!(
        app,
        App {
            slug: APP_SLUG.to_owned(),
            bot: Account {
                id: BOT_ID,
                login: format!("{APP_SLUG}[bot]"),
            },
        }
    );
}

#[tokio::test]
async fn a_jwt_signed_with_another_key_is_refused() {
    let setup = setup().await;
    let foreign = GitHubApp::new(APP_ID, FOREIGN_KEY.as_bytes(), &setup.forge.api_url())
        .unwrap_or_else(|error| panic!("a PKCS#8 key does not load: {error}"));
    let refused = foreign.app().await;
    assert!(
        matches!(refused, Err(Error::Status { status: 401, .. })),
        "{refused:?}"
    );
    let refused = foreign.file(&setup.repository, "main", "README.md").await;
    assert!(
        matches!(refused, Err(Error::Status { status: 401, .. })),
        "{refused:?}"
    );
    assert!(setup.forge.tokens().is_empty());
}

#[tokio::test]
async fn a_key_that_is_not_an_rsa_private_key_is_refused() {
    let api = Url::parse("https://api.github.com").unwrap_or_else(|error| panic!("{error}"));
    let refused = GitHubApp::new(
        APP_ID,
        b"-----BEGIN PRIVATE KEY-----\n-----END PRIVATE KEY-----\n",
        &api,
    );
    assert!(matches!(refused, Err(Error::Config(_))));
    let refused = GitHubApp::new(APP_ID, b"not a key", &api);
    assert!(matches!(refused, Err(Error::Config(_))));
}
