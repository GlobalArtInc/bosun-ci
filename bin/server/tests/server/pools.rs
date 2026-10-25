use std::net::SocketAddr;
use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{Accept, Capability, Conclusion, JobFinished};
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::{Connection, FakeAgent};
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, timeout};

use crate::api::text;
use crate::{Launch, Server, WAIT, client, message, owner, strings};

const SERVER: &str = "[server]\nlisten = \"127.0.0.1:0\"\n\n[database]\nmax_connections = 4\n";

const QUIET: Duration = Duration::from_millis(500);

const SOURCE: &str = "name: ci\non: workflow_dispatch\njobs:\n";

struct Site {
    database: TestDatabase,
    owner: String,
    server: Server,
    address: SocketAddr,
}

impl Site {
    async fn start(settings: &str) -> Site {
        let database = TestDatabase::migrated().await;
        let owner = owner(&database.url(), "roman").await;
        let mut server = Launch::serve(&database.url())
            .config(&format!("{SERVER}{settings}"))
            .spawn();
        let address = server.listening().await;
        let site = Site {
            database,
            owner,
            server,
            address,
        };
        for (organisation, projects) in
            [("globalart", ["ops", "keel"]), ("ownlate", ["site", "app"])]
        {
            site.created(
                "/api/v1/organisations",
                &json!({ "slug": organisation, "name": organisation }),
            )
            .await;
            for project in projects {
                site.created(
                    "/api/v1/projects",
                    &json!({ "organisation": organisation, "slug": project, "name": project }),
                )
                .await;
            }
        }
        site
    }

    async fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        let response = client()
            .post(format!("http://{}{path}", self.address))
            .bearer_auth(&self.owner)
            .json(body)
            .send()
            .await
            .unwrap_or_else(|error| panic!("POST {path}: {error}"));
        let status = response.status().as_u16();
        (status, response.json().await.expect("a JSON body"))
    }

    async fn created(&self, path: &str, body: &Value) -> Value {
        let (status, body) = self.post(path, body).await;
        assert_eq!(status, 201, "POST {path}: {body}");
        body
    }

    async fn get(&self, path: &str) -> Value {
        let response = client()
            .get(format!("http://{}{path}", self.address))
            .bearer_auth(&self.owner)
            .send()
            .await
            .unwrap_or_else(|error| panic!("GET {path}: {error}"));
        assert_eq!(response.status(), 200, "GET {path}");
        response.json().await.expect("a JSON body")
    }

    async fn pool(&self, name: &str, trust: &str, access: Value) -> Value {
        self.created(
            "/api/v1/pools",
            &json!({ "name": name, "trust": trust, "access": access }),
        )
        .await
    }

    async fn alias(&self, label: &str, organisation: Option<&str>, pool: Option<&str>) {
        self.alias_with(&json!({
            "label": label,
            "organisation": organisation,
            "pool": pool,
            "capability": null,
            "image": null
        }))
        .await;
    }

    async fn alias_with(&self, body: &Value) -> (u16, Value) {
        let (status, alias) = self.post("/api/v1/label-aliases", body).await;
        assert!(status == 201 || status == 200, "{alias}");
        (status, alias)
    }

    async fn agent(
        &self,
        pool: &str,
        name: &str,
        labels: &[&str],
        capabilities: &[Capability],
    ) -> FakeAgent {
        let token = self
            .created(
                "/api/v1/registration-tokens",
                &json!({ "pool": pool, "reusable": false, "expires_at": null }),
            )
            .await;
        FakeAgent::register(self.address, text(&token["token"]), name, labels)
            .await
            .with_capabilities(capabilities)
    }

    async fn workflow(&self, project: &str, jobs: &[(&str, &str)]) -> String {
        let mut source = SOURCE.to_owned();
        for (key, runs_on) in jobs {
            source.push_str(&format!(
                "  {key}:\n    runs-on: {runs_on}\n    steps:\n      - run: \"true\"\n"
            ));
        }
        let workflow = self
            .created(
                &format!("/api/v1/projects/{project}/workflows"),
                &json!({ "name": "ci", "source": source }),
            )
            .await;
        text(&workflow["id"]).to_owned()
    }

    async fn dispatch(&self, workflow: &str) -> String {
        let run = self
            .created(
                &format!("/api/v1/workflows/{workflow}/dispatch"),
                &json!({ "ref": null, "inputs": {} }),
            )
            .await;
        text(&run["id"]).to_owned()
    }

    async fn values(&self, sql: &str) -> Vec<String> {
        strings(self.database.connection(), sql).await
    }

    async fn job(&self, run: &str, key: &str) -> String {
        self.values(&format!(
            "SELECT j.id::text AS value FROM bosun.job j \
             JOIN bosun.attempt a ON a.id = j.attempt_id \
             WHERE a.run_id = '{run}' AND j.key = '{key}'"
        ))
        .await
        .pop()
        .unwrap_or_else(|| panic!("no job `{key}` in run {run}"))
    }

    async fn untrusted(&self, run: &str) {
        self.database
            .connection()
            .execute_unprepared(&format!(
                "UPDATE bosun.run SET trusted = false WHERE id = '{run}'"
            ))
            .await
            .expect("the run is marked untrusted");
    }

    async fn until(&self, sql: &str, expected: &[&str]) {
        let deadline = Instant::now() + WAIT;
        loop {
            let found = self.values(sql).await;
            if found == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{sql} gave {found:?} and not {expected:?} within {WAIT:?}"
            );
            sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn takes(connection: &mut Connection) -> String {
    let assignment = connection.assignment().await;
    connection.send(Report::Accept(Accept {
        execution_id: assignment.execution_id.clone(),
    }));
    connection.send(Report::JobFinished(JobFinished {
        execution_id: assignment.execution_id,
        conclusion: Conclusion::Success.into(),
        outputs: Default::default(),
    }));
    assignment.job_id
}

async fn offered_nothing(connection: &mut Connection, what: &str) {
    assert!(
        timeout(QUIET, connection.assignment()).await.is_err(),
        "{what} was offered a job"
    );
}

#[tokio::test]
async fn an_aliased_label_goes_to_the_agents_of_its_pool_and_the_organisations_alias_wins() {
    let site = Site::start("").await;
    site.pool(
        "linux",
        "trusted",
        json!({ "all_projects": true, "organisations": [], "projects": [] }),
    )
    .await;
    site.pool(
        "hosted",
        "trusted",
        json!({ "all_projects": true, "organisations": [], "projects": [] }),
    )
    .await;
    site.alias("Ubuntu-Latest", None, Some("hosted")).await;
    site.alias("ubuntu-latest", Some("globalart"), Some("linux"))
        .await;
    let impostor = site
        .agent(
            "default",
            "runner0-1",
            &["ubuntu-latest"],
            &[Capability::Shell],
        )
        .await;
    let hosted = site
        .agent("hosted", "runner1-1", &[], &[Capability::Shell])
        .await;
    let linux = site
        .agent("linux", "runner2-1", &[], &[Capability::Shell])
        .await;
    let mut on_impostor = impostor.connect(site.address).await;
    let mut on_hosted = hosted.connect(site.address).await;
    let mut on_linux = linux.connect(site.address).await;
    for connection in [&on_impostor, &on_hosted, &on_linux] {
        connection.idle();
    }

    let ops = site
        .workflow("globalart/ops", &[("build", "ubuntu-latest")])
        .await;
    let run = site.dispatch(&ops).await;
    let build = site.job(&run, "build").await;
    assert_eq!(takes(&mut on_linux).await, build);
    offered_nothing(
        &mut on_impostor,
        "the agent of the pool default with the label",
    )
    .await;
    offered_nothing(&mut on_hosted, "the agent of the installation's alias").await;
    let linux_id = text(&site.get("/api/v1/pools").await["items"][1]["id"]).to_owned();
    assert_eq!(
        site.values(&format!(
            "SELECT concat_ws(' ', cardinality(labels), array_to_string(pool_ids, ','), \
             cardinality(capabilities)) AS value FROM bosun.job WHERE id = '{build}'"
        ))
        .await,
        [format!("0 {linux_id} 0")]
    );

    let site_workflow = site
        .workflow("ownlate/site", &[("build", "[ubuntu-latest]")])
        .await;
    let other = site.dispatch(&site_workflow).await;
    assert_eq!(takes(&mut on_hosted).await, site.job(&other, "build").await);
    on_linux.idle();
    offered_nothing(&mut on_linux, "the agent of the organisation's alias").await;
    offered_nothing(
        &mut on_impostor,
        "the agent of the pool default with the label",
    )
    .await;
}

#[tokio::test]
async fn a_label_without_an_alias_or_an_agent_is_cancelled_with_no_agent_after_max_queue_time() {
    let site = Site::start("\n[limits]\nmax_queue_time = \"2s\"\n").await;
    let fake = site
        .agent("default", "runner1-1", &[], &[Capability::Shell])
        .await;
    let mut connection = fake.connect(site.address).await;
    connection.idle();
    let workflow = site
        .workflow("globalart/ops", &[("build", "ubuntu-latest")])
        .await;
    let run = site.dispatch(&workflow).await;
    site.until(
        &format!(
            "SELECT concat_ws(' ', status, conclusion) AS value FROM bosun.run WHERE id = '{run}'"
        ),
        &["completed cancelled"],
    )
    .await;
    assert_eq!(
        site.values(&format!(
            "SELECT concat_ws(' ', array_to_string(labels, ','), status, conclusion) AS value \
             FROM bosun.job WHERE id = '{}'",
            site.job(&run, "build").await
        ))
        .await,
        ["ubuntu-latest completed cancelled"]
    );
    assert_eq!(
        site.values(&format!(
            "SELECT data ->> 'reason' AS value FROM bosun.run_event WHERE run_id = '{run}' \
             AND kind = 'job.status' AND data ->> 'to' = 'completed'"
        ))
        .await,
        ["no_agent"]
    );
    offered_nothing(&mut connection, "the agent without the label").await;
}

#[tokio::test]
async fn a_pools_access_list_keeps_the_jobs_of_other_projects_away_from_its_agents() {
    let site = Site::start("").await;
    let pool = site
        .pool(
            "keel-only",
            "trusted",
            json!({ "all_projects": false, "organisations": [], "projects": ["globalart/keel"] }),
        )
        .await;
    assert_eq!(pool["access"]["projects"][0]["path"], "globalart/keel");
    let keel_agent = site
        .agent("keel-only", "runner1-1", &[], &[Capability::Shell])
        .await;
    let mut connection = keel_agent.connect(site.address).await;
    let ops = site
        .workflow("globalart/ops", &[("build", "[self-hosted]")])
        .await;
    let keel = site
        .workflow("globalart/keel", &[("build", "[self-hosted]")])
        .await;
    let older = site.dispatch(&ops).await;
    let newer = site.dispatch(&keel).await;
    connection.idle();
    assert_eq!(
        takes(&mut connection).await,
        site.job(&newer, "build").await
    );
    connection.idle();
    offered_nothing(&mut connection, "the agent of a pool closed to the project").await;
    let ops_build = site.job(&older, "build").await;
    assert_eq!(
        site.values(&format!(
            "SELECT status AS value FROM bosun.job WHERE id = '{ops_build}'"
        ))
        .await,
        ["queued"]
    );

    let grouped = site
        .workflow(
            "ownlate/app",
            &[("build", "{ group: keel-only, labels: [self-hosted] }")],
        )
        .await;
    let run = site.dispatch(&grouped).await;
    offered_nothing(
        &mut connection,
        "the agent of the group a project may not use",
    )
    .await;
    assert_eq!(
        site.values(&format!(
            "SELECT concat_ws(' ', status, cardinality(pool_ids)) AS value FROM bosun.job \
             WHERE id = '{}'",
            site.job(&run, "build").await
        ))
        .await,
        ["queued 0"]
    );
}

#[tokio::test]
async fn trusted_and_untrusted_pools_take_only_the_jobs_of_runs_of_their_own_trust() {
    let site = Site::start("").await;
    let all = json!({ "all_projects": true, "organisations": [], "projects": [] });
    site.pool("inside", "trusted", all.clone()).await;
    site.pool("forks", "untrusted", all).await;
    let inside = site
        .agent("inside", "runner1-1", &["build"], &[Capability::Podman])
        .await;
    let outside = site
        .agent("forks", "runner2-1", &["build"], &[Capability::Podman])
        .await;
    let mut on_inside = inside.connect(site.address).await;
    let mut on_outside = outside.connect(site.address).await;
    site.until(
        "SELECT concat_ws(' ', name, array_to_string(labels, ','), \
         array_to_string(capabilities, ',')) AS value FROM bosun.agent ORDER BY name",
        &[
            "runner1-1 self-hosted,linux,x64,podman,build podman",
            "runner2-1 self-hosted,linux,x64,podman,build podman",
        ],
    )
    .await;

    let workflow = site
        .workflow("globalart/ops", &[("build", "[self-hosted, build]")])
        .await;
    let first = site.dispatch(&workflow).await;
    let second = site.dispatch(&workflow).await;
    site.untrusted(&second).await;
    let third = site.dispatch(&workflow).await;
    site.untrusted(&third).await;

    on_outside.idle();
    assert_eq!(
        takes(&mut on_outside).await,
        site.job(&second, "build").await,
        "the untrusted pool took the older job of a trusted run"
    );
    on_inside.idle();
    assert_eq!(takes(&mut on_inside).await, site.job(&first, "build").await);
    on_inside.idle();
    offered_nothing(&mut on_inside, "the agent of the trusted pool").await;
    on_outside.idle();
    assert_eq!(
        takes(&mut on_outside).await,
        site.job(&third, "build").await
    );
    site.until(
        "SELECT concat_ws(' ', a.name, r.trusted) AS value FROM bosun.execution e \
         JOIN bosun.agent a ON a.id = e.agent_id JOIN bosun.job j ON j.id = e.job_id \
         JOIN bosun.attempt t ON t.id = j.attempt_id JOIN bosun.run r ON r.id = t.run_id \
         ORDER BY r.number",
        &["runner1-1 t", "runner2-1 f", "runner2-1 f"],
    )
    .await;
}

#[tokio::test]
async fn an_agent_of_an_untrusted_pool_is_listed_without_shell_and_offered_no_job_that_needs_it() {
    let mut site = Site::start("").await;
    site.pool(
        "forks",
        "untrusted",
        json!({ "all_projects": false, "organisations": ["globalart"], "projects": [] }),
    )
    .await;
    site.alias_with(&json!({
        "label": "on-the-host",
        "organisation": null,
        "pool": null,
        "capability": "shell",
        "image": null
    }))
    .await;
    let fake = site
        .agent(
            "forks",
            "runner3-1",
            &["build", "Shell"],
            &[Capability::Shell, Capability::Podman],
        )
        .await;
    let mut connection = fake.connect(site.address).await;
    let warned = site
        .server
        .expect("the warning about shell", |line| {
            message(line).starts_with("the agent announced shell in an untrusted pool")
        })
        .await;
    assert_eq!(
        (&warned["name"], &warned["pool"]),
        (&json!("runner3-1"), &json!("forks"))
    );
    site.until(
        "SELECT array_to_string(capabilities, ',') AS value FROM bosun.agent",
        &["podman"],
    )
    .await;
    let listed = site.get("/api/v1/agents").await;
    let agent = &listed["items"][0];
    assert_eq!(agent["capabilities"], json!(["podman"]));
    assert_eq!(
        agent["labels"],
        json!(["self-hosted", "linux", "x64", "podman", "build"])
    );

    let workflow = site
        .workflow(
            "globalart/ops",
            &[
                ("by-label", "[self-hosted, shell]"),
                ("by-alias", "on-the-host"),
                ("either", "[self-hosted, build]"),
            ],
        )
        .await;
    let run = site.dispatch(&workflow).await;
    site.untrusted(&run).await;
    connection.idle();
    assert_eq!(takes(&mut connection).await, site.job(&run, "either").await);
    connection.idle();
    offered_nothing(&mut connection, "the agent without shell").await;
    assert_eq!(
        site.values(&format!(
            "SELECT concat_ws(' ', j.key, j.status, \
             NULLIF(array_to_string(j.capabilities, ','), '')) \
             AS value FROM bosun.job j JOIN bosun.attempt a ON a.id = j.attempt_id \
             WHERE a.run_id = '{run}' AND j.key <> 'either' ORDER BY j.key"
        ))
        .await,
        ["by-alias queued shell", "by-label queued"]
    );
}

#[tokio::test]
async fn pools_and_aliases_refuse_what_their_rules_do_not_allow() {
    let site = Site::start("").await;
    let (status, refused) = site
        .post(
            "/api/v1/pools",
            &json!({
                "name": "Linux",
                "trust": "trusted",
                "access": {
                    "all_projects": false,
                    "organisations": ["nobody"],
                    "projects": ["globalart/nothing", "ops"]
                }
            }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    let fields: Vec<&str> = refused["error"]["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|field| text(&field["field"]))
        .collect();
    assert_eq!(
        fields,
        [
            "name",
            "access.organisations.0",
            "access.projects.0",
            "access.projects.1"
        ]
    );
    let (status, refused) = site
        .post(
            "/api/v1/pools",
            &json!({
                "name": "empty",
                "trust": "trusted",
                "access": { "all_projects": false, "organisations": [], "projects": [] }
            }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["error"]["fields"][0]["field"], "access");
    let (status, taken) = site
        .post(
            "/api/v1/pools",
            &json!({
                "name": "default",
                "trust": "untrusted",
                "access": { "all_projects": true, "organisations": [], "projects": [] }
            }),
        )
        .await;
    assert_eq!((status, &taken["error"]["code"]), (409, &json!("conflict")));

    let (status, refused) = site
        .post(
            "/api/v1/label-aliases",
            &json!({
                "label": "ubuntu latest",
                "organisation": "nobody",
                "pool": "nowhere",
                "capability": "shell",
                "image": "docker.io/library/ubuntu:24.04"
            }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    let fields: Vec<&str> = refused["error"]["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|field| text(&field["field"]))
        .collect();
    assert_eq!(fields, ["label", "organisation", "pool", "capability"]);

    let image = json!({
        "label": "Ubuntu-22.04",
        "organisation": "globalart",
        "pool": "default",
        "capability": null,
        "image": "docker.io/library/ubuntu:22.04"
    });
    let (status, first) = site.alias_with(&image).await;
    assert_eq!(status, 201);
    assert_eq!(
        (&first["label"], &first["capability"]),
        (&json!("ubuntu-22.04"), &json!("podman"))
    );
    let (status, again) = site.alias_with(&image).await;
    assert_eq!(status, 200);
    assert_eq!(again["id"], first["id"]);
    let listed = site.get("/api/v1/label-aliases").await;
    assert_eq!(listed["items"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        listed["items"][0]["image"],
        "docker.io/library/ubuntu:22.04"
    );
}
