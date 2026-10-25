use bosun_db::installations::{self, Account, Bound, Synced, Visibility};
use bosun_db::{Error, organisations, projects};
use bosun_testkit::TestDatabase;
use bosun_types::api::{Installation, Repository};
use sea_orm::{DatabaseConnection, TransactionTrait};

const INSTALLATION: u64 = 52_814_339;

const OTHER: u64 = 52_814_340;

fn repository(id: u64, name: &str, default_branch: &str) -> Repository {
    Repository {
        id,
        owner: "GlobalArtInc".to_owned(),
        name: name.to_owned(),
        default_branch: default_branch.to_owned(),
    }
}

async fn record(db: &DatabaseConnection, installation: u64, login: &str) -> bool {
    let transaction = db.begin().await.expect("a transaction");
    let mapped = installations::record(
        &transaction,
        installation,
        &Account {
            id: installation + 1000,
            login,
        },
    )
    .await
    .expect("a recorded installation");
    transaction.commit().await.expect("a commit");
    mapped.is_some()
}

async fn sync(db: &DatabaseConnection, installation: u64, repositories: &[Repository]) -> Synced {
    let transaction = db.begin().await.expect("a transaction");
    let synced = installations::sync(&transaction, installation, repositories)
        .await
        .expect("a sync");
    transaction.commit().await.expect("a commit");
    synced
}

#[tokio::test]
async fn an_installation_sees_the_repositories_of_its_last_listing_by_their_ids() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let globalart = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    assert_eq!(
        installations::visibility(db, globalart.id, "GlobalArtInc", "ops")
            .await
            .expect("a lookup"),
        Visibility::NoInstallation
    );

    assert!(!record(db, INSTALLATION, "GlobalArtInc").await);
    let transaction = db.begin().await.expect("a transaction");
    installations::map(&transaction, INSTALLATION, globalart.id)
        .await
        .expect("a mapping");
    transaction.commit().await.expect("a commit");
    assert!(record(db, INSTALLATION, "GlobalArtInc").await);
    assert_eq!(
        organisations::find(db, globalart.id)
            .await
            .expect("a lookup")
            .and_then(|organisation| organisation.installation),
        Some(Installation {
            id: INSTALLATION,
            account: "GlobalArtInc".to_owned(),
        })
    );

    let ops = repository(1, "ops", "main");
    assert_eq!(
        sync(
            db,
            INSTALLATION,
            &[ops.clone(), repository(2, "keel", "trunk")]
        )
        .await,
        Synced {
            visible: 2,
            hidden: 0
        }
    );
    assert_eq!(
        installations::visibility(db, globalart.id, "globalartinc", "OPS")
            .await
            .expect("a lookup"),
        Visibility::Visible(ops)
    );

    let renamed = repository(1, "operations", "release");
    assert_eq!(
        sync(db, INSTALLATION, std::slice::from_ref(&renamed)).await,
        Synced {
            visible: 1,
            hidden: 1
        }
    );
    for (name, expected) in [
        ("operations", Visibility::Visible(renamed)),
        (
            "ops",
            Visibility::Hidden {
                account: "GlobalArtInc".to_owned(),
            },
        ),
        (
            "keel",
            Visibility::Hidden {
                account: "GlobalArtInc".to_owned(),
            },
        ),
    ] {
        assert_eq!(
            installations::visibility(db, globalart.id, "GlobalArtInc", name)
                .await
                .expect("a lookup"),
            expected,
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_bound_project_follows_its_repository_and_a_repository_binds_one_project() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let globalart = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    record(db, INSTALLATION, "GlobalArtInc").await;
    let ops = repository(1, "ops", "main");
    sync(db, INSTALLATION, std::slice::from_ref(&ops)).await;
    let project = projects::create(db, globalart.id, "ops", "Operations")
        .await
        .expect("a project");
    assert_eq!(project.repository, None);
    let bound = projects::bind(db, project.id, Some(&ops))
        .await
        .expect("a binding")
        .expect("the project");
    assert_eq!(bound.repository, Some(ops.clone()));

    let other = projects::create(db, globalart.id, "other", "Other")
        .await
        .expect("a project");
    let refused = projects::bind(db, other.id, Some(&ops)).await;
    assert!(
        matches!(&refused, Err(Error::RepositoryBound(name)) if name == "GlobalArtInc/ops"),
        "{refused:?}"
    );

    let renamed = repository(1, "operations", "trunk");
    sync(db, INSTALLATION, std::slice::from_ref(&renamed)).await;
    for found in [
        projects::find(db, project.id).await.expect("a lookup"),
        projects::find_by_slugs(db, "globalart", "ops")
            .await
            .expect("a lookup"),
    ] {
        assert_eq!(
            found.and_then(|project| project.repository),
            Some(renamed.clone())
        );
    }

    let transaction = db.begin().await.expect("a transaction");
    assert!(
        installations::remove(&transaction, INSTALLATION)
            .await
            .expect("a removal")
    );
    transaction.commit().await.expect("a commit");
    assert_eq!(
        projects::find(db, project.id)
            .await
            .expect("a lookup")
            .and_then(|project| project.repository),
        Some(renamed)
    );
    let unbound = projects::bind(db, project.id, None)
        .await
        .expect("an unbinding")
        .expect("the project");
    assert_eq!(unbound.repository, None);
}

#[tokio::test]
async fn an_owner_binds_an_installation_that_no_other_organisation_holds() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let globalart = organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    let ownlate = organisations::create(db, "ownlate", "OwnLate")
        .await
        .expect("an organisation");
    record(db, INSTALLATION, "GlobalArtInc").await;
    record(db, OTHER, "OwnLate").await;

    let bind = |organisation, installation| async move {
        let transaction = db.begin().await.expect("a transaction");
        let bound = installations::bind(&transaction, organisation, installation)
            .await
            .expect("a binding");
        transaction.commit().await.expect("a commit");
        bound
    };
    let installation = |organisation| async move {
        organisations::find(db, organisation)
            .await
            .expect("a lookup")
            .expect("the organisation")
            .installation
            .map(|installation| installation.id)
    };
    assert_eq!(bind(globalart.id, Some(7)).await, Bound::Unknown(7));
    assert_eq!(bind(globalart.id, Some(INSTALLATION)).await, Bound::Bound);
    assert_eq!(bind(globalart.id, Some(INSTALLATION)).await, Bound::Bound);
    assert_eq!(
        bind(ownlate.id, Some(INSTALLATION)).await,
        Bound::Taken {
            installation: INSTALLATION,
            organisation: "globalart".to_owned()
        }
    );
    assert_eq!(bind(globalart.id, Some(OTHER)).await, Bound::Bound);
    assert_eq!(installation(globalart.id).await, Some(OTHER));
    assert_eq!(bind(ownlate.id, Some(INSTALLATION)).await, Bound::Bound);
    assert_eq!(installation(ownlate.id).await, Some(INSTALLATION));
    assert_eq!(bind(globalart.id, None).await, Bound::Bound);
    assert_eq!(installation(globalart.id).await, None);
    assert_eq!(installation(ownlate.id).await, Some(INSTALLATION));

    assert_eq!(
        organisations::create_unless_taken(db, "ownlate", "OwnLate")
            .await
            .expect("a creation"),
        None
    );
    let created = organisations::create_unless_taken(db, "globalartinc", "GlobalArtInc")
        .await
        .expect("a creation")
        .expect("a new organisation");
    assert_eq!(
        organisations::find_by_slug(db, "globalartinc")
            .await
            .expect("a lookup")
            .map(|organisation| (organisation.id, organisation.name)),
        Some((created, "GlobalArtInc".to_owned()))
    );
}
