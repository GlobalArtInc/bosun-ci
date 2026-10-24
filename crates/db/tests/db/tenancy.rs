use bosun_db::{Error, organisations, projects};
use bosun_testkit::TestDatabase;

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
