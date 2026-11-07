use bosun_proto::assignment::{Container, Credentials};
use bosun_types::api::{Capability, ProjectSettings, WorkspacePolicy};
use bosun_types::status::{Conclusion, JobStatus};
use sea_orm::ConnectionTrait;
use sea_orm::prelude::Uuid;

use super::World;
use super::queue::{LEASE, assigned, claim, connected_in, parse};
use super::secrets::{scopes, secret};

const HELD: [&str; 4] = ["self-hosted", "linux", "x64", "podman"];

fn source(container: &str) -> String {
    format!(
        "name: ci
on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    strategy:
      matrix:
        flavour: [slim]
    env:
      REGISTRY_USER: ci
{container}
    steps:
      - run: make
"
    )
}

const CONTAINER: &str = "    container:
      image: ${{ vars.REGISTRY }}/acme/builder-${{ matrix.flavour }}:1
      credentials:
        username: ${{ env.REGISTRY_USER }}
        password: ${{ secrets.REGISTRY_PASSWORD }}
      options: --cpus 2";

#[tokio::test]
async fn the_image_and_the_credentials_of_a_container_are_evaluated_at_the_assignment_with_the_flags_of_its_pool_and_project()
 {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let (organisation, project) = scopes(&world).await;
    secret(&world, project, "REGISTRY_PASSWORD", "hunter2").await;
    bosun_db::variables::set(db, organisation, "REGISTRY", "ghcr.io")
        .await
        .expect("a variable");
    let pool = bosun_db::pools::default_pool(db).await.expect("a pool");
    let agent = connected_in(
        &world,
        pool.id,
        "runner1-1",
        &HELD,
        &[Capability::Podman],
        replica,
    )
    .await;

    world.dispatch(&parse(&source(CONTAINER))).await;
    let claimed = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("a job");
    assert!(!claimed.allow_privileged);
    assert_eq!(claimed.workspace, WorkspacePolicy::Persist);
    let (_, documents) = assigned(&world, claimed).await;
    assert_eq!(
        documents.evaluated.container,
        Some(Container {
            image: "ghcr.io/acme/builder-slim:1".to_owned(),
            credentials: Some(Credentials {
                username: Some("ci".to_owned()),
                password: Some("hunter2".to_owned()),
            }),
        })
    );
    assert_eq!(documents.contexts.secrets["REGISTRY_PASSWORD"], "hunter2");
    assert!(documents.evaluated.needs_podman);
    assert!(!documents.evaluated.allow_privileged);
    assert!(!documents.evaluated.workspace_clean);
    assert!(!documents.evaluated.untrusted);
    let template = documents.job.container.expect("the container template");
    assert_eq!(
        template.options.expect("options").as_literal(),
        Some("--cpus 2".to_owned())
    );

    bosun_db::pools::set_allow_privileged(db, pool.id, true)
        .await
        .expect("a pool");
    let stored = bosun_db::projects::settings(db, world.workflow.project_id)
        .await
        .expect("a read")
        .expect("the settings");
    bosun_db::projects::change_settings(
        db,
        world.workflow.project_id,
        stored.version,
        &ProjectSettings {
            workspace: WorkspacePolicy::Clean,
            ..stored.settings
        },
    )
    .await
    .expect("a change")
    .expect("the change applied");
    let finished = connected_in(
        &world,
        pool.id,
        "runner1-2",
        &HELD,
        &[Capability::Podman],
        replica,
    )
    .await;
    world.dispatch(&parse(&source(CONTAINER))).await;
    let claimed = claim(db, finished.id, replica, LEASE, &[])
        .await
        .expect("a job");
    let (_, documents) = assigned(&world, claimed).await;
    assert!(documents.evaluated.allow_privileged);
    assert!(documents.evaluated.workspace_clean);

    let plain = connected_in(
        &world,
        pool.id,
        "runner1-3",
        &["self-hosted", "linux", "x64", "shell"],
        &[Capability::Shell],
        replica,
    )
    .await;
    world.dispatch(&parse(&source(""))).await;
    let claimed = claim(db, plain.id, replica, LEASE, &[])
        .await
        .expect("a job");
    let (_, documents) = assigned(&world, claimed).await;
    assert_eq!(documents.evaluated.container, None);
    assert!(!documents.evaluated.needs_podman);
    assert!(
        !documents.evaluated.allow_privileged,
        "a job without a container carries no privileged option"
    );
    assert!(documents.evaluated.workspace_clean);
}

#[tokio::test]
async fn the_assignment_of_a_job_of_an_untrusted_run_says_so() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let (_, project) = scopes(&world).await;
    secret(&world, project, "REGISTRY_PASSWORD", "hunter2").await;
    let forks = super::queue::pool(
        &world,
        "forks",
        bosun_types::api::TrustLevel::Untrusted,
        &[bosun_db::pools::Access::AllProjects],
    )
    .await;
    let run = world.dispatch(&parse(&source(CONTAINER))).await;
    db.execute_unprepared(&format!(
        "UPDATE bosun.run SET trusted = false WHERE id = '{run}'; \
         UPDATE bosun.job SET pool_ids = ARRAY['{forks}']::uuid[]"
    ))
    .await
    .expect("an untrusted run");
    let agent = connected_in(
        &world,
        forks,
        "runner2-1",
        &HELD,
        &[Capability::Podman],
        replica,
    )
    .await;
    let claimed = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("a job");
    let (_, documents) = assigned(&world, claimed).await;
    assert!(documents.evaluated.untrusted);
    assert!(!documents.evaluated.allow_privileged);
    assert_eq!(
        documents
            .evaluated
            .container
            .expect("a container")
            .credentials,
        Some(Credentials {
            username: Some("ci".to_owned()),
            password: Some(String::new()),
        }),
        "an untrusted run reads no secret of its project"
    );
}

#[tokio::test]
async fn a_container_whose_image_does_not_evaluate_fails_the_job_at_its_assignment() {
    let world = World::new().await;
    let db = world.db();
    let replica = Uuid::now_v7();
    let pool = bosun_db::pools::default_pool(db).await.expect("a pool");
    let agent = connected_in(
        &world,
        pool.id,
        "runner1-1",
        &HELD,
        &[Capability::Podman],
        replica,
    )
    .await;
    let run = world
        .dispatch(&parse(&source(
            "    container: ghcr.io/${{ fromJSON('{') }}",
        )))
        .await;
    let claimed = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("a job");
    let outcome =
        crate::queue::assignment(db, claimed, &super::site(), world.tokens(), world.actions())
            .await
            .expect("an assignment");
    assert!(outcome.is_none());
    let projection = world.project(run).await;
    let (_, row) = projection.job("build");
    assert_eq!(
        (row.status, row.conclusion),
        (JobStatus::Completed, Some(Conclusion::Failure))
    );
    let shown = world.run(run).await;
    let [notice] = shown.notices.as_slice() else {
        panic!("the run has the notices {:?}", shown.notices);
    };
    assert!(
        notice
            .diagnostic
            .message
            .starts_with("`container.image` of job `build`: "),
        "{}",
        notice.diagnostic.message
    );
}

async fn alias(world: &World, label: &str, capability: Option<Capability>, image: Option<&str>) {
    bosun_db::aliases::set(
        world.db(),
        bosun_db::aliases::NewAlias {
            organisation: None,
            label,
            pool: None,
            capability,
            image,
        },
    )
    .await
    .expect("an alias");
}

fn aliased(label: &str, container: &str) -> String {
    format!(
        "name: ci
on: workflow_dispatch
jobs:
  build:
    runs-on: {label}
{container}
    steps:
      - run: make
"
    )
}

async fn assignment_of(
    world: &World,
    source: &str,
) -> (Option<String>, bosun_proto::assignment::Documents) {
    let db = world.db();
    let replica = Uuid::now_v7();
    let pool = bosun_db::pools::default_pool(db).await.expect("a pool");
    let agent = connected_in(
        world,
        pool.id,
        "runner1-1",
        &HELD,
        &[Capability::Podman],
        replica,
    )
    .await;
    world.dispatch(&parse(source)).await;
    let claimed = claim(db, agent.id, replica, LEASE, &[])
        .await
        .expect("a job");
    let stored = claimed.alias_image.clone();
    assert!(claimed.needs_podman);
    (stored, assigned(world, claimed).await.1)
}

#[tokio::test]
async fn the_image_of_a_label_alias_becomes_the_container_of_the_job_unless_the_workflow_names_one()
{
    let world = World::new().await;
    alias(&world, "boxed", None, Some("ghcr.io/acme/boxed:2")).await;
    let (stored, documents) = assignment_of(&world, &aliased("boxed", "")).await;
    assert_eq!(stored.as_deref(), Some("ghcr.io/acme/boxed:2"));
    assert_eq!(
        documents.evaluated.container,
        Some(Container {
            image: "ghcr.io/acme/boxed:2".to_owned(),
            credentials: None,
        })
    );
    assert!(documents.evaluated.needs_podman);
    assert_eq!(documents.job.container, None);

    let world = World::new().await;
    alias(&world, "boxed", None, Some("ghcr.io/acme/boxed:2")).await;
    let (_, documents) = assignment_of(
        &world,
        &aliased("boxed", "    container: ghcr.io/acme/own:1"),
    )
    .await;
    assert_eq!(
        documents.evaluated.container.expect("a container").image,
        "ghcr.io/acme/own:1"
    );
}

#[tokio::test]
async fn an_alias_that_names_podman_without_an_image_gives_a_job_that_needs_podman_and_no_container()
 {
    let world = World::new().await;
    alias(&world, "bare", Some(Capability::Podman), None).await;
    let (stored, documents) = assignment_of(&world, &aliased("bare", "")).await;
    assert_eq!(stored, None);
    assert_eq!(documents.evaluated.container, None);
    assert!(documents.evaluated.needs_podman);
}
