use std::collections::BTreeSet;

use bosun_db::roles::{self, NewBinding, Subject};
use bosun_db::{Error, organisations, projects, teams, users};
use bosun_testkit::TestDatabase;
use bosun_types::permission::Permission;
use sea_orm::SqlErr;

#[tokio::test]
async fn a_binding_gives_nothing_outside_its_organisation() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let globalart = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    let other = organisations::create(db, "other", "Other")
        .await
        .expect("another organisation");
    let elsewhere = projects::create(db, other.id, "ops", "Operations")
        .await
        .expect("a project of the other organisation");
    let outsiders = teams::create(db, other.id, "outsiders", "Outsiders")
        .await
        .expect("a team of the other organisation");
    let dana = users::create(db, "dana").await.expect("a user");
    teams::add_member(db, outsiders, dana.id)
        .await
        .expect("a member");
    let viewer = roles::builtin(db, "viewer")
        .await
        .expect("a lookup")
        .expect("the role viewer");

    for (subject, project, what) in [
        (
            Subject::User(dana.id),
            Some(elsewhere.id),
            "a project of another organisation",
        ),
        (
            Subject::Team(outsiders),
            None,
            "a team of another organisation",
        ),
    ] {
        let refused = roles::bind(
            db,
            NewBinding {
                role_id: viewer,
                subject,
                organisation_id: globalart.id,
                project_id: project,
            },
        )
        .await;
        match refused {
            Err(Error::Database(error)) => assert!(
                matches!(
                    error.sql_err(),
                    Some(SqlErr::ForeignKeyConstraintViolation(_))
                ),
                "{what}: {error}"
            ),
            other => panic!("a binding with {what} was not refused: {other:?}"),
        }
    }

    let releaser = roles::create(
        db,
        other.id,
        "releaser",
        &BTreeSet::from([Permission::RunDispatch]),
    )
    .await
    .expect("a custom role");
    roles::bind(
        db,
        NewBinding {
            role_id: releaser,
            subject: Subject::User(dana.id),
            organisation_id: globalart.id,
            project_id: None,
        },
    )
    .await
    .expect("a binding of a role of another organisation");
    let held = roles::permissions(db, dana.id)
        .await
        .expect("the permissions");
    assert!(
        held.organisations.is_empty() && held.projects.is_empty(),
        "{held:?}"
    );
}
