use bosun_forge::{Forge, InstalledRepository};
use bosun_testkit::forge::FakeForge;

use crate::{EVERYTHING, FULL_NAME, NAME, OWNER, setup};

#[tokio::test]
async fn an_installation_lists_every_repository_it_can_see_across_pages() {
    let setup = setup().await;
    let mut expected = vec![InstalledRepository {
        id: setup.repository_id,
        owner: OWNER.to_owned(),
        name: NAME.to_owned(),
        default_branch: "main".to_owned(),
    }];
    for index in 0..120 {
        let name = format!("service-{index:03}");
        let id = setup
            .forge
            .create_repository(setup.installation, &format!("{OWNER}/{name}"));
        expected.push(InstalledRepository {
            id,
            owner: OWNER.to_owned(),
            name,
            default_branch: "main".to_owned(),
        });
    }
    let elsewhere = setup.forge.install("elsewhere", EVERYTHING);
    setup.forge.create_repository(elsewhere, "elsewhere/hidden");

    let mut listed = setup
        .app
        .installation_repositories(setup.installation)
        .await
        .unwrap_or_else(|error| panic!("the repositories are not listed: {error}"));
    listed.sort_by_key(|repository| repository.id);
    assert_eq!(listed, expected);
}

#[tokio::test]
async fn a_renamed_repository_is_listed_under_its_new_name_and_its_id() {
    let setup = setup().await;
    let before = setup
        .app
        .installation_repositories(setup.installation)
        .await
        .unwrap_or_else(|error| panic!("the repositories are not listed: {error}"));
    assert_eq!(before[0].name, NAME);
    setup.forge.rename_repository(FULL_NAME, "renamed");
    let after = setup
        .app
        .installation_repositories(setup.installation)
        .await
        .unwrap_or_else(|error| panic!("the repositories are not listed: {error}"));
    assert_eq!(
        after,
        [InstalledRepository {
            id: setup.repository_id,
            owner: OWNER.to_owned(),
            name: "renamed".to_owned(),
            default_branch: "main".to_owned(),
        }]
    );
    let commit = setup
        .forge
        .commit(&format!("{OWNER}/renamed"), "main", [("README.md", "app")]);
    let mut renamed = setup.repository.clone();
    renamed.name = "renamed".to_owned();
    assert!(
        setup
            .app
            .file(&renamed, &commit, "README.md")
            .await
            .unwrap_or_else(|error| panic!("the renamed repository is not readable: {error}"))
            .is_some()
    );
}

#[tokio::test]
async fn the_fake_holds_back_its_answers_by_the_delay_a_test_sets() {
    let forge = FakeForge::start().await;
    let installation = forge.install(OWNER, EVERYTHING);
    forge.create_repository(installation, FULL_NAME);
    let app = bosun_forge::GitHubApp::new(
        bosun_testkit::forge::APP_ID,
        bosun_testkit::forge::APP_KEY.as_bytes(),
        &forge.api_url(),
    )
    .unwrap_or_else(|error| panic!("the App does not start: {error}"));
    let delay = std::time::Duration::from_millis(300);
    forge.delay(delay);
    let started = std::time::Instant::now();
    app.installation_repositories(installation)
        .await
        .unwrap_or_else(|error| panic!("the repositories are not listed: {error}"));
    assert!(started.elapsed() >= delay * 2, "{:?}", started.elapsed());
}
