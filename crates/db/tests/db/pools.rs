use bosun_db::Error;
use bosun_db::aliases::{self, NewAlias};
use bosun_db::entity::pool;
use bosun_db::pools::{self, Access, NewPool, Placement};
use bosun_testkit::TestDatabase;
use bosun_types::api::{Capability, PoolAccess, PoolOrganisation, PoolProject, TrustLevel};
use bosun_types::id::PoolId;
use sea_orm::{DatabaseConnection, EntityTrait, PaginatorTrait};

#[tokio::test]
async fn the_trusted_pool_default_is_seeded_once_and_open_to_all_projects() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let default = pools::default_pool(db).await.expect("the default pool");
    assert_eq!(default.name, "default");
    assert_eq!(default.trust, TrustLevel::Trusted);
    assert_eq!(
        default.access,
        PoolAccess {
            all_projects: true,
            organisations: Vec::new(),
            projects: Vec::new(),
        }
    );
    assert_eq!(pool::Entity::find().count(db).await.expect("a count"), 1);
}

#[tokio::test]
async fn a_pool_keeps_its_access_list_and_its_name_is_taken_once() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let globalart = bosun_db::organisations::create(db, "globalart", "GlobalArt")
        .await
        .expect("an organisation");
    let ownlate = bosun_db::organisations::create(db, "ownlate", "OwnLate")
        .await
        .expect("an organisation");
    let keel = bosun_db::projects::create(db, ownlate.id, "keel", "Keel")
        .await
        .expect("a project");
    let created = pools::create(
        db,
        NewPool {
            name: "linux",
            trust: TrustLevel::Untrusted,
            access: &[
                Access::Organisation(globalart.id),
                Access::Project(ownlate.id, keel.id),
                Access::Organisation(globalart.id),
            ],
        },
    )
    .await
    .expect("a pool");
    assert_eq!(
        (created.name.as_str(), created.trust),
        ("linux", TrustLevel::Untrusted)
    );
    assert_eq!(
        created.access,
        PoolAccess {
            all_projects: false,
            organisations: vec![PoolOrganisation {
                id: globalart.id,
                slug: "globalart".to_owned(),
            }],
            projects: vec![PoolProject {
                id: keel.id,
                organisation_id: ownlate.id,
                path: "ownlate/keel".to_owned(),
            }],
        }
    );
    assert_eq!(
        pools::named(db, "linux").await.expect("a lookup"),
        Some(created.clone())
    );
    assert_eq!(
        pools::find(db, created.id).await.expect("a lookup"),
        Some(created.clone())
    );
    let listed = pools::list(db, None, 10).await.expect("a list");
    assert_eq!(
        listed
            .iter()
            .map(|pool| pool.name.as_str())
            .collect::<Vec<_>>(),
        ["default", "linux"]
    );
    let after = pools::list(db, Some(listed[0].id.as_uuid()), 10)
        .await
        .expect("a list");
    assert_eq!(after, [created]);

    let again = pools::create(
        db,
        NewPool {
            name: "linux",
            trust: TrustLevel::Trusted,
            access: &[Access::AllProjects],
        },
    )
    .await;
    assert!(
        matches!(&again, Err(Error::PoolTaken(name)) if name == "linux"),
        "{again:?}"
    );
}

struct Tenancy {
    database: TestDatabase,
    globalart: bosun_types::api::Organisation,
    keel: bosun_types::api::Project,
    ops: bosun_types::api::Project,
    default: PoolId,
    linux: PoolId,
    keel_only: PoolId,
}

impl Tenancy {
    async fn new() -> Tenancy {
        let database = TestDatabase::migrated().await;
        let db = database.connection();
        let globalart = bosun_db::organisations::create(db, "globalart", "GlobalArt")
            .await
            .expect("an organisation");
        let keel = bosun_db::projects::create(db, globalart.id, "keel", "Keel")
            .await
            .expect("a project");
        let ops = bosun_db::projects::create(db, globalart.id, "ops", "Operations")
            .await
            .expect("a project");
        let default = pools::default_pool(db).await.expect("the default pool").id;
        let linux = Tenancy::pool(db, "linux", &[Access::Organisation(globalart.id)]).await;
        let keel_only =
            Tenancy::pool(db, "keel-only", &[Access::Project(globalart.id, keel.id)]).await;
        Tenancy {
            database,
            globalart,
            keel,
            ops,
            default,
            linux,
            keel_only,
        }
    }

    async fn pool(db: &DatabaseConnection, name: &str, access: &[Access]) -> PoolId {
        pools::create(
            db,
            NewPool {
                name,
                trust: TrustLevel::Trusted,
                access,
            },
        )
        .await
        .expect("a pool")
        .id
    }

    fn db(&self) -> &DatabaseConnection {
        self.database.connection()
    }

    async fn alias(
        &self,
        organisation: bool,
        label: &str,
        pool: Option<PoolId>,
        capability: Option<Capability>,
    ) -> bool {
        aliases::set(
            self.db(),
            NewAlias {
                organisation: organisation.then_some(self.globalart.id),
                label,
                pool,
                capability,
                image: None,
            },
        )
        .await
        .expect("an alias")
        .created
    }

    async fn place(
        &self,
        project: &bosun_types::api::Project,
        group: Option<&str>,
        labels: &[&str],
    ) -> Placement {
        let labels: Vec<String> = labels.iter().map(|label| (*label).to_owned()).collect();
        pools::place(self.db(), self.globalart.id, project.id, group, &labels)
            .await
            .expect("a placement")
    }
}

#[tokio::test]
async fn a_placement_without_aliases_keeps_the_labels_and_names_the_pools_open_to_the_project() {
    let tenancy = Tenancy::new().await;
    assert_eq!(
        tenancy
            .place(&tenancy.keel, None, &["Self-Hosted", "self-hosted", "X64"])
            .await,
        Placement {
            labels: vec!["self-hosted".to_owned(), "x64".to_owned()],
            pools: vec![tenancy.default, tenancy.linux, tenancy.keel_only],
            capabilities: Vec::new(),
        }
    );
    assert_eq!(
        tenancy
            .place(&tenancy.ops, None, &["self-hosted"])
            .await
            .pools,
        [tenancy.default, tenancy.linux]
    );
    assert_eq!(
        tenancy
            .place(&tenancy.ops, Some("keel-only"), &["self-hosted"])
            .await
            .pools,
        Vec::<PoolId>::new()
    );
    assert_eq!(
        tenancy
            .place(&tenancy.keel, Some("keel-only"), &["self-hosted"])
            .await
            .pools,
        [tenancy.keel_only]
    );
}

#[tokio::test]
async fn an_alias_replaces_its_label_by_its_pool_and_capability_and_the_organisations_wins() {
    let tenancy = Tenancy::new().await;
    assert!(
        tenancy
            .alias(false, "ubuntu-latest", Some(tenancy.default), None)
            .await
    );
    assert_eq!(
        tenancy.place(&tenancy.keel, None, &["Ubuntu-Latest"]).await,
        Placement {
            labels: Vec::new(),
            pools: vec![tenancy.default],
            capabilities: Vec::new(),
        }
    );
    assert!(
        tenancy
            .alias(
                true,
                "ubuntu-latest",
                Some(tenancy.linux),
                Some(Capability::Shell)
            )
            .await
    );
    assert_eq!(
        tenancy
            .place(&tenancy.keel, None, &["ubuntu-latest", "x64"])
            .await,
        Placement {
            labels: vec!["x64".to_owned()],
            pools: vec![tenancy.linux],
            capabilities: vec![Capability::Shell],
        }
    );
    assert!(
        !tenancy
            .alias(true, "ubuntu-latest", Some(tenancy.keel_only), None)
            .await,
        "setting the alias again created another one"
    );
    assert_eq!(
        tenancy.place(&tenancy.ops, None, &["ubuntu-latest"]).await,
        Placement {
            labels: Vec::new(),
            pools: Vec::new(),
            capabilities: Vec::new(),
        },
        "an alias to a pool closed to the project places the job nowhere"
    );
    let listed = aliases::list(tenancy.db(), None, 10).await.expect("a list");
    assert_eq!(
        listed
            .iter()
            .map(|alias| (
                alias.label.as_str(),
                alias.organisation_id,
                alias.pool_id,
                alias.capability
            ))
            .collect::<Vec<_>>(),
        [
            ("ubuntu-latest", None, Some(tenancy.default), None),
            (
                "ubuntu-latest",
                Some(tenancy.globalart.id),
                Some(tenancy.keel_only),
                None
            ),
        ]
    );
}

#[tokio::test]
async fn several_aliases_meet_in_one_pool_and_need_every_capability_they_name() {
    let tenancy = Tenancy::new().await;
    tenancy
        .alias(false, "ubuntu-latest", None, Some(Capability::Podman))
        .await;
    tenancy
        .alias(false, "ubuntu-24.04", Some(tenancy.linux), None)
        .await;
    tenancy.alias(false, "anywhere", None, None).await;
    assert_eq!(
        tenancy
            .place(
                &tenancy.keel,
                None,
                &["ubuntu-latest", "ubuntu-24.04", "anywhere", "gpu"]
            )
            .await,
        Placement {
            labels: vec!["gpu".to_owned()],
            pools: vec![tenancy.linux],
            capabilities: vec![Capability::Podman],
        }
    );
    tenancy
        .alias(
            false,
            "keel",
            Some(tenancy.keel_only),
            Some(Capability::Shell),
        )
        .await;
    assert_eq!(
        tenancy
            .place(&tenancy.keel, None, &["ubuntu-24.04", "keel"])
            .await,
        Placement {
            labels: Vec::new(),
            pools: Vec::new(),
            capabilities: vec![Capability::Shell],
        }
    );
    assert_eq!(
        tenancy
            .place(&tenancy.keel, Some("linux"), &["anywhere"])
            .await
            .pools,
        [tenancy.linux]
    );
}
