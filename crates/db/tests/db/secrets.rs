use std::collections::{BTreeMap, BTreeSet};

use bosun_db::secrets::{Scope, Sealed};
use bosun_testkit::TestDatabase;
use bosun_types::id::{OrganisationId, ProjectId};
use sea_orm::ConnectionTrait;

use crate::strings;

fn sealed(byte: u8, kek_id: &str) -> Sealed {
    Sealed {
        ciphertext: vec![byte; 20],
        data_key: vec![byte; 60],
        kek_id: kek_id.to_owned(),
    }
}

async fn tenancy<C: ConnectionTrait>(db: &C) -> (OrganisationId, ProjectId, ProjectId) {
    let organisation = bosun_db::organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    let keel = bosun_db::projects::create(db, organisation.id, "keel", "Keel")
        .await
        .expect("a project");
    let ops = bosun_db::projects::create(db, organisation.id, "ops", "Operations")
        .await
        .expect("a project");
    (organisation.id, keel.id, ops.id)
}

#[tokio::test]
async fn a_secret_is_set_once_per_scope_and_name_and_a_project_secret_shadows_the_organisations() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let (organisation, keel, ops) = tenancy(db).await;
    let of_organisation = Scope {
        organisation_id: organisation,
        project_id: None,
    };
    let of_keel = Scope {
        organisation_id: organisation,
        project_id: Some(keel),
    };
    let first = bosun_db::secrets::set(db, of_organisation, "A", &sealed(1, "k1"))
        .await
        .expect("a secret");
    assert!(first.created);
    let again = bosun_db::secrets::set(db, of_organisation, "A", &sealed(2, "k1"))
        .await
        .expect("a secret");
    assert!(!again.created);
    assert_eq!(again.secret.id, first.secret.id);
    bosun_db::secrets::set(db, of_keel, "A", &sealed(3, "k1"))
        .await
        .expect("a secret");
    bosun_db::secrets::set(db, of_organisation, "B", &sealed(4, "k2"))
        .await
        .expect("a secret");

    let names = |found: Vec<bosun_db::secrets::Stored>| -> BTreeMap<String, u8> {
        found
            .into_iter()
            .map(|stored| (stored.name, stored.sealed.ciphertext[0]))
            .collect()
    };
    assert_eq!(
        names(
            bosun_db::secrets::in_scope(db, organisation, keel, None)
                .await
                .expect("the secrets of keel")
        ),
        BTreeMap::from([("A".to_owned(), 3), ("B".to_owned(), 4)])
    );
    assert_eq!(
        names(
            bosun_db::secrets::in_scope(db, organisation, ops, None)
                .await
                .expect("the secrets of ops")
        ),
        BTreeMap::from([("A".to_owned(), 2), ("B".to_owned(), 4)])
    );
    assert_eq!(
        names(
            bosun_db::secrets::in_scope(
                db,
                organisation,
                keel,
                Some(&BTreeSet::from(["B".to_owned()]))
            )
            .await
            .expect("one secret of keel")
        ),
        BTreeMap::from([("B".to_owned(), 4)])
    );
    let listed: Vec<String> = bosun_db::secrets::list(db, of_keel, None, 10)
        .await
        .expect("the secrets of keel itself")
        .into_iter()
        .map(|secret| secret.name)
        .collect();
    assert_eq!(listed, ["A"]);

    let stale = bosun_db::secrets::under_another_kek(db, "k2", None, 10)
        .await
        .expect("the secrets under k1");
    assert_eq!(stale.len(), 2);
    assert!(
        bosun_db::secrets::rewrap(db, &stale[0], vec![9; 60], "k2")
            .await
            .expect("a rewrap")
    );
    assert!(
        !bosun_db::secrets::rewrap(db, &stale[0], vec![8; 60], "k2")
            .await
            .expect("a rewrap of a changed row"),
        "a rewrap replaced a data key it did not read"
    );
    assert_eq!(
        strings(
            db,
            "SELECT concat_ws(' ', name, kek_id, project_id IS NULL) AS value FROM bosun.secret \
             ORDER BY id"
        )
        .await,
        ["A k2 t", "A k1 f", "B k2 t"]
    );

    assert!(
        bosun_db::secrets::delete(db, of_keel, "A")
            .await
            .expect("a delete")
    );
    assert!(
        !bosun_db::secrets::delete(db, of_keel, "A")
            .await
            .expect("a second delete")
    );
}

#[tokio::test]
async fn a_project_variable_shadows_the_organisations_and_a_deleted_project_takes_its_own_along() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let (organisation, keel, ops) = tenancy(db).await;
    let of_organisation = Scope {
        organisation_id: organisation,
        project_id: None,
    };
    let of_keel = Scope {
        organisation_id: organisation,
        project_id: Some(keel),
    };
    bosun_db::variables::set(db, of_organisation, "WHERE", "organisation")
        .await
        .expect("a variable");
    bosun_db::variables::set(db, of_organisation, "ONLY", "organisation")
        .await
        .expect("a variable");
    let set = bosun_db::variables::set(db, of_keel, "WHERE", "keel")
        .await
        .expect("a variable");
    assert!(set.created);
    let replaced = bosun_db::variables::set(db, of_keel, "WHERE", "keel again")
        .await
        .expect("a variable");
    assert!(!replaced.created);
    assert_eq!(replaced.variable.value, "keel again");
    assert_eq!(
        bosun_db::variables::in_scope(db, organisation, keel)
            .await
            .expect("the variables of keel"),
        BTreeMap::from([
            ("ONLY".to_owned(), "organisation".to_owned()),
            ("WHERE".to_owned(), "keel again".to_owned()),
        ])
    );
    assert_eq!(
        bosun_db::variables::in_scope(db, organisation, ops)
            .await
            .expect("the variables of ops")["WHERE"],
        "organisation"
    );
    db.execute_unprepared(&format!("DELETE FROM bosun.project WHERE id = '{keel}'"))
        .await
        .expect("a deleted project");
    assert_eq!(
        strings(
            db,
            "SELECT name || '=' || value AS value FROM bosun.variable ORDER BY id"
        )
        .await,
        ["WHERE=organisation", "ONLY=organisation"]
    );
    assert!(
        !bosun_db::variables::delete(db, of_keel, "WHERE")
            .await
            .expect("a delete")
    );
}
