use bosun_db::{Error, organisations, projects};
use bosun_testkit::TestDatabase;
use bosun_types::api::{ForgeAccess, ForgePermission, ForkPolicy, ProjectSettings, Reporting};

use crate::fixture;

#[tokio::test]
async fn organisations_and_projects_are_found_by_id_and_by_slug() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let globalart = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    let other = organisations::create(db, "other", "Other")
        .await
        .expect("another organisation");
    assert_eq!(
        (globalart.slug.as_str(), globalart.name.as_str()),
        ("globalart", "GlobalArt")
    );
    assert_eq!(
        organisations::find(db, globalart.id)
            .await
            .expect("a lookup"),
        Some(globalart.clone())
    );
    assert_eq!(
        organisations::find_by_slug(db, "globalart")
            .await
            .expect("a lookup"),
        Some(globalart.clone())
    );
    assert_eq!(
        organisations::find_by_slug(db, "nobody")
            .await
            .expect("a lookup"),
        None
    );

    let ops = projects::create(db, globalart.id, "ops", "Operations")
        .await
        .expect("a project");
    let elsewhere = projects::create(db, other.id, "ops", "Operations elsewhere")
        .await
        .expect("a project of the same slug in another organisation");
    assert_eq!(ops.organisation_id, globalart.id);
    assert_eq!(
        projects::find(db, ops.id).await.expect("a lookup"),
        Some(ops.clone())
    );
    assert_eq!(
        projects::find_by_slugs(db, "globalart", "ops")
            .await
            .expect("a lookup"),
        Some(ops)
    );
    assert_eq!(
        projects::find_by_slugs(db, "other", "ops")
            .await
            .expect("a lookup"),
        Some(elsewhere)
    );
    for (organisation, project) in [("globalart", "keel"), ("nobody", "ops")] {
        assert_eq!(
            projects::find_by_slugs(db, organisation, project)
                .await
                .expect("a lookup"),
            None,
            "{organisation}/{project}"
        );
    }
}

#[tokio::test]
async fn a_slug_is_taken_once_per_organisation_and_once_per_installation() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let globalart = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    let again = organisations::create(db, "globalart", "Again").await;
    assert!(
        matches!(&again, Err(Error::OrganisationTaken(slug)) if slug == "globalart"),
        "{again:?}"
    );
    projects::create(db, globalart.id, "ops", "Operations")
        .await
        .expect("a project");
    let again = projects::create(db, globalart.id, "ops", "Again").await;
    assert!(
        matches!(&again, Err(Error::ProjectTaken(slug)) if slug == "ops"),
        "{again:?}"
    );
}

#[tokio::test]
async fn settings_start_at_the_defaults_and_change_only_from_their_current_version() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let globalart = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    let ops = projects::create(db, globalart.id, "ops", "Operations")
        .await
        .expect("a project");
    let first = projects::settings(db, ops.id)
        .await
        .expect("a read")
        .expect("the settings of the project");
    assert_eq!(first.version, 1);
    assert_eq!(
        serde_json::to_value(&first.settings).expect("JSON"),
        fixture("api/project-settings.json")
    );

    let wanted = ProjectSettings {
        github_workflows: true,
        fork_policy: ForkPolicy::Approval,
        token_permissions: [
            (ForgePermission::Contents, ForgeAccess::Write),
            (ForgePermission::PullRequests, ForgeAccess::Read),
        ]
        .into(),
        report: Reporting::Statuses,
    };
    let other = ProjectSettings {
        fork_policy: ForkPolicy::Automatic,
        ..first.settings.clone()
    };
    let (one, two) = tokio::join!(
        projects::change_settings(db, ops.id, first.version, &wanted),
        projects::change_settings(db, ops.id, first.version, &other),
    );
    let (one, two) = (one.expect("a change"), two.expect("a change"));
    assert!(
        one.is_some() != two.is_some(),
        "two changes from one version both applied, or neither: {:?} {:?}",
        one.as_ref().map(|changed| &changed.settings),
        two.as_ref().map(|changed| &changed.settings),
    );
    let applied = one.or(two).expect("the change that applied");
    assert_eq!(applied.version, 2);
    let stored = projects::settings(db, ops.id)
        .await
        .expect("a read")
        .expect("the settings of the project");
    assert_eq!(stored.version, 2);
    assert_eq!(stored.settings, applied.settings);

    let stale = projects::change_settings(db, ops.id, first.version, &wanted)
        .await
        .expect("a change");
    assert!(
        stale.is_none(),
        "a change from version 1 applied to version 2"
    );
    let unchanged = projects::settings(db, ops.id)
        .await
        .expect("a read")
        .expect("the settings of the project");
    assert_eq!(unchanged.version, 2);
    assert_eq!(unchanged.settings, applied.settings);
}
