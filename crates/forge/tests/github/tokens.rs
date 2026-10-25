use std::collections::BTreeMap;
use std::time::Duration;

use bosun_forge::{Access, Error, Forge, InstallationToken, Permissions};
use bosun_testkit::forge::MintedToken;
use chrono::{TimeDelta, Utc};

use crate::{EVERYTHING, FULL_NAME, Setup, setup};

const OTHER: &str = "acme/other";

fn scope(permissions: &[(&str, Access)]) -> Permissions {
    permissions
        .iter()
        .map(|(name, access)| ((*name).to_owned(), *access))
        .collect()
}

fn record(setup: &Setup, token: &InstallationToken) -> MintedToken {
    setup
        .forge
        .tokens()
        .into_iter()
        .find(|minted| minted.token == token.token)
        .unwrap_or_else(|| panic!("the fake forge has no record of the token"))
}

async fn get(setup: &Setup, token: &str, path: &str) -> u16 {
    reqwest::Client::new()
        .get(format!("{}/{path}", setup.forge.api_url()))
        .bearer_auth(token)
        .header("user-agent", "bosun-test")
        .send()
        .await
        .unwrap_or_else(|error| panic!("the fake forge is not reachable: {error}"))
        .status()
        .as_u16()
}

#[tokio::test]
async fn a_minted_token_carries_exactly_the_requested_scope() {
    let setup = setup().await;
    setup.forge.create_repository(setup.installation, OTHER);
    setup
        .forge
        .commit(FULL_NAME, "main", [("README.md", "app")]);
    setup.forge.commit(OTHER, "main", [("README.md", "other")]);
    let token = setup
        .app
        .mint_token(
            setup.installation,
            &["app".to_owned()],
            &scope(&[("contents", Access::Read), ("issues", Access::Write)]),
        )
        .await
        .unwrap_or_else(|error| panic!("the token is not minted: {error}"));
    let minted = record(&setup, &token);
    assert_eq!(minted.installation, setup.installation);
    assert_eq!(minted.repositories, ["app"]);
    assert_eq!(
        minted.permissions,
        BTreeMap::from([
            ("contents".to_owned(), "read".to_owned()),
            ("issues".to_owned(), "write".to_owned()),
        ])
    );
    assert!(!minted.revoked);
    assert!((minted.expires_at - token.expires_at).num_seconds().abs() <= 1);
    assert!(token.expires_at - Utc::now() > TimeDelta::minutes(59));
    assert_eq!(setup.forge.tokens().len(), 1);
    assert_eq!(
        get(
            &setup,
            &token.token,
            &format!("repos/{FULL_NAME}/contents/README.md")
        )
        .await,
        200
    );
    assert_eq!(
        get(
            &setup,
            &token.token,
            &format!("repos/{OTHER}/contents/README.md")
        )
        .await,
        404
    );
    assert_eq!(
        get(
            &setup,
            &token.token,
            &format!("repos/{FULL_NAME}/pulls/1/files")
        )
        .await,
        403
    );
}

#[tokio::test]
async fn a_revoked_token_is_refused() {
    let setup = setup().await;
    setup
        .forge
        .commit(FULL_NAME, "main", [("README.md", "app")]);
    let token = setup
        .app
        .mint_token(
            setup.installation,
            &["app".to_owned()],
            &scope(&[("contents", Access::Read)]),
        )
        .await
        .unwrap_or_else(|error| panic!("the token is not minted: {error}"));
    let path = format!("repos/{FULL_NAME}/contents/README.md");
    assert_eq!(get(&setup, &token.token, &path).await, 200);
    setup
        .app
        .revoke_token(&token.token)
        .await
        .unwrap_or_else(|error| panic!("the token is not revoked: {error}"));
    assert!(record(&setup, &token).revoked);
    assert_eq!(get(&setup, &token.token, &path).await, 401);
    let again = setup.app.revoke_token(&token.token).await;
    assert!(
        matches!(again, Err(Error::Status { status: 401, .. })),
        "{again:?}"
    );
}

#[tokio::test]
async fn a_token_without_a_scope_is_not_minted() {
    let setup = setup().await;
    let unscoped = setup
        .app
        .mint_token(
            setup.installation,
            &[],
            &scope(&[("contents", Access::Read)]),
        )
        .await;
    assert!(matches!(unscoped, Err(Error::Unscoped)), "{unscoped:?}");
    let unscoped = setup
        .app
        .mint_token(setup.installation, &["app".to_owned()], &Permissions::new())
        .await;
    assert!(matches!(unscoped, Err(Error::Unscoped)), "{unscoped:?}");
    assert!(setup.forge.tokens().is_empty());
}

#[tokio::test]
async fn a_scope_beyond_the_installation_is_refused() {
    let setup = setup().await;
    let elsewhere = setup
        .app
        .mint_token(
            setup.installation,
            &["elsewhere".to_owned()],
            &scope(&[("contents", Access::Read)]),
        )
        .await;
    assert!(
        matches!(elsewhere, Err(Error::Status { status: 422, .. })),
        "{elsewhere:?}"
    );
    let administration = setup
        .app
        .mint_token(
            setup.installation,
            &["app".to_owned()],
            &scope(&[("administration", Access::Write)]),
        )
        .await;
    assert!(
        matches!(administration, Err(Error::Status { status: 422, .. })),
        "{administration:?}"
    );
    assert!(setup.forge.tokens().is_empty());
}

#[tokio::test]
async fn the_installation_token_is_reused_while_it_is_fresh() {
    let setup = setup().await;
    setup
        .forge
        .commit(FULL_NAME, "main", [("README.md", "app")]);
    for _ in 0..3 {
        setup
            .app
            .file(&setup.repository, "main", "README.md")
            .await
            .unwrap_or_else(|error| panic!("the file is not read: {error}"));
    }
    let tokens = setup.forge.tokens();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].repositories, ["app"]);
    assert_eq!(tokens[0].permissions.len(), EVERYTHING.len());
}

#[tokio::test]
async fn an_installation_token_close_to_expiry_is_replaced() {
    let setup = setup().await;
    setup.forge.set_token_lifetime(Duration::from_secs(60));
    setup
        .forge
        .commit(FULL_NAME, "main", [("README.md", "app")]);
    for _ in 0..2 {
        setup
            .app
            .file(&setup.repository, "main", "README.md")
            .await
            .unwrap_or_else(|error| panic!("the file is not read: {error}"));
    }
    assert_eq!(setup.forge.tokens().len(), 2);
}

#[tokio::test]
async fn an_installation_holds_what_the_forge_says_and_a_second_read_is_conditional() {
    let setup = setup().await;
    let held = setup
        .app
        .installation_permissions(setup.installation)
        .await
        .unwrap_or_else(|error| panic!("the permissions are not read: {error}"));
    assert_eq!(
        held,
        scope(&[
            ("checks", Access::Write),
            ("contents", Access::Write),
            ("issues", Access::Write),
            ("metadata", Access::Read),
            ("pull_requests", Access::Write),
            ("statuses", Access::Write),
        ])
    );
    let again = setup
        .app
        .installation_permissions(setup.installation)
        .await
        .unwrap_or_else(|error| panic!("the permissions are not read again: {error}"));
    assert_eq!(again, held);
    let path = format!("/api/v3/app/installations/{}", setup.installation);
    let statuses: Vec<u16> = setup
        .forge
        .requests()
        .into_iter()
        .filter(|request| request.path == path)
        .map(|request| request.status)
        .collect();
    assert_eq!(statuses, [200, 304]);
    assert!(setup.forge.tokens().is_empty());

    let administering = setup.forge.install(
        "other",
        &[("administration", "admin"), ("metadata", "read")],
    );
    assert_eq!(
        setup
            .app
            .installation_permissions(administering)
            .await
            .unwrap_or_else(|error| panic!("the permissions are not read: {error}")),
        scope(&[
            ("administration", Access::Write),
            ("metadata", Access::Read)
        ])
    );
    let missing = setup
        .app
        .installation_permissions(administering + 1000)
        .await;
    assert!(
        matches!(missing, Err(Error::Status { status: 404, .. })),
        "{missing:?}"
    );
}
