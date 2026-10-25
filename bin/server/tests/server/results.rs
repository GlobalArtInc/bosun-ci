use std::time::{Duration, Instant};

use bosun_blob::{Directory, Store as _};
use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{
    Accept, Annotation, AnnotationKind, Conclusion, JobFinished, StepFinished, StepStarted,
};
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::{Connection, FakeAgent};
use bosun_testkit::forge::{FakeForge, RecordedAnnotation, Sender};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::sleep;

use crate::api::{Api, text};
use crate::hooks::{ACCOUNT, PERMISSIONS, delivered, processed};
use crate::{Launch, WAIT, owner, strings};

const REPOSITORY: &str = "GlobalArtInc/ops";

const ROMAN: Sender<'static> = Sender {
    login: "roman",
    id: 7001,
};

const STEPS: u32 = 12;

const PER_STEP: u32 = 10;

const SUMMARIES: u32 = 3;

const SUMMARY_LIMIT: usize = 65_535;

const CUT_NOTICE: &str = "\n\n---\n\nThe summaries of this job are longer than a check run holds, \
                          so they end here; the job's page on Bosun shows them whole.\n";

struct Scene {
    database: TestDatabase,
    forge: FakeForge,
    api: Api,
    store: TempDir,
    connection: Connection,
    execution: String,
    steps: Vec<String>,
}

fn workflow() -> String {
    let mut source = String::from(
        "name: ci\non: push\njobs:\n  build:\n    runs-on: [self-hosted]\n    steps:\n",
    );
    for number in 0..STEPS {
        source.push_str(&format!("      - run: echo {number}\n"));
    }
    source
}

fn summary(step: u32) -> String {
    let mut text = format!("## Step {step}\r\n\n");
    text.push_str(&"\u{e9}t\u{e9} ".repeat(5_000));
    if step + 1 < SUMMARIES {
        text.push('\n');
    }
    text
}

fn kind(number: u32) -> (AnnotationKind, &'static str) {
    match number % 3 {
        0 => (AnnotationKind::Error, "failure"),
        1 => (AnnotationKind::Warning, "warning"),
        _ => (AnnotationKind::Notice, "notice"),
    }
}

fn annotations(step: u32) -> Vec<Annotation> {
    (0..PER_STEP)
        .map(|number| Annotation {
            kind: kind(number).0.into(),
            message: format!("problem {number} of step {step}"),
            title: None,
            file: Some(format!("src/step{step}.rs")),
            line: Some(number + 1),
            end_line: Some(number + 1),
            column: None,
            end_column: None,
        })
        .collect()
}

impl Scene {
    async fn start() -> Scene {
        let database = TestDatabase::migrated().await;
        let forge = FakeForge::start().await;
        let installation = forge.install(ACCOUNT, PERMISSIONS);
        forge.create_repository(installation, REPOSITORY);
        let first = forge.commit(REPOSITORY, "main", [("README.md", "# ops\n")]);
        let store = tempfile::tempdir().expect("a directory for the store");
        let token = owner(&database.url(), "roman").await;
        let api = Api::serve(
            Launch::serve(&database.url())
                .github(&forge)
                .env("BOSUN_STORE_PATH", &store.path().to_string_lossy()),
            token,
        )
        .await;
        delivered(
            &api,
            &forge,
            "installation",
            &forge.installation_event(installation, "created"),
        )
        .await;
        processed(&database).await;
        let (status, project) = api
            .post(
                "/api/v1/projects",
                &json!({
                    "organisation": "globalartinc",
                    "slug": "ops",
                    "name": "Operations",
                    "repository": REPOSITORY
                }),
            )
            .await;
        assert_eq!(status, 201, "{project}");
        let (status, registration) = api
            .post(
                "/api/v1/registration-tokens",
                &json!({ "pool": "default", "reusable": false, "expires_at": null }),
            )
            .await;
        assert_eq!(status, 201, "{registration}");
        let agent =
            FakeAgent::register(api.address, text(&registration["token"]), "runner1", &[]).await;
        let mut connection = agent.connect(api.address).await;
        let source = workflow();
        let after = forge.commit(
            REPOSITORY,
            "main",
            [(".bosun/workflows/ci.yml", source.as_str())],
        );
        let push = forge.push_event(REPOSITORY, "refs/heads/main", &first, &after, ROMAN);
        delivered(&api, &forge, "push", &push).await;
        processed(&database).await;
        connection.idle();
        let assignment = connection.assignment().await;
        let execution = assignment.execution_id.clone();
        connection.send(Report::Accept(Accept {
            execution_id: execution.clone(),
        }));
        let sql = format!(
            "SELECT id::text AS value FROM bosun.step WHERE execution_id = '{execution}' \
             ORDER BY index"
        );
        let deadline = Instant::now() + WAIT;
        let steps = loop {
            let steps = strings(database.connection(), &sql).await;
            if steps.len() == STEPS as usize {
                break steps;
            }
            assert!(Instant::now() < deadline, "the steps were not created");
            sleep(Duration::from_millis(50)).await;
        };
        Scene {
            database,
            forge,
            api,
            store,
            connection,
            execution,
            steps,
        }
    }

    fn finish_step(&self, step: u32, summary: String, annotations: Vec<Annotation>) {
        self.connection.send(Report::StepFinished(StepFinished {
            execution_id: self.execution.clone(),
            step_index: step,
            completed_at: None,
            outcome: Conclusion::Failure.into(),
            conclusion: Conclusion::Failure.into(),
            outputs: Default::default(),
            annotations,
            summary,
        }));
    }

    async fn run_job(&self) {
        for step in 0..STEPS {
            self.connection.send(Report::StepStarted(StepStarted {
                execution_id: self.execution.clone(),
                step_index: step,
                started_at: None,
            }));
            let written = if step < SUMMARIES {
                summary(step)
            } else {
                String::new()
            };
            self.finish_step(step, written, annotations(step));
            if step == 0 {
                self.finish_step(0, "a summary sent again".to_owned(), annotations(99));
            }
        }
        self.connection.send(Report::JobFinished(JobFinished {
            execution_id: self.execution.clone(),
            conclusion: Conclusion::Failure.into(),
            outputs: Default::default(),
        }));
    }

    async fn reported(&self) {
        let deadline = Instant::now() + WAIT;
        loop {
            let pending = bosun_db::outbox::pending(self.database.connection())
                .await
                .expect("the outbox backlog");
            let completed = self
                .forge
                .check_runs()
                .iter()
                .any(|check_run| check_run.status == "completed");
            if pending == 0 && completed {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the job was not reported within {WAIT:?}: {:#?}",
                self.forge.check_runs()
            );
            sleep(Duration::from_millis(50)).await;
        }
    }

    async fn summary_of(&self, step: usize) -> (u16, Option<String>, Vec<u8>) {
        let response = self
            .api
            .request(
                reqwest::Method::GET,
                &format!(
                    "/api/v1/executions/{}/steps/{}/summary",
                    self.execution, self.steps[step]
                ),
            )
            .send()
            .await
            .expect("a response");
        let status = response.status().as_u16();
        let kind = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let body = response.bytes().await.expect("a body").to_vec();
        (status, kind, body)
    }
}

#[tokio::test]
async fn a_job_with_120_annotations_shows_them_all_and_the_forge_receives_them_in_three_requests() {
    let scene = Scene::start().await;
    scene.run_job().await;
    scene.reported().await;

    let (status, execution) = scene
        .api
        .get(&format!("/api/v1/executions/{}", scene.execution))
        .await;
    assert_eq!(status, 200, "{execution}");
    let shown: Vec<(String, u64, String)> = execution["steps"]
        .as_array()
        .expect("the steps")
        .iter()
        .flat_map(|step| step["annotations"].as_array().cloned().unwrap_or_default())
        .map(|annotation| {
            (
                text(&annotation["file"]).to_owned(),
                annotation["line"].as_u64().expect("a line"),
                text(&annotation["message"]).to_owned(),
            )
        })
        .collect();
    let expected: Vec<(String, u64, String)> = (0..STEPS)
        .flat_map(|step| {
            (0..PER_STEP).map(move |number| {
                (
                    format!("src/step{step}.rs"),
                    u64::from(number + 1),
                    format!("problem {number} of step {step}"),
                )
            })
        })
        .collect();
    assert_eq!(shown.len(), 120);
    assert_eq!(shown, expected);

    let batches: Vec<usize> = scene
        .forge
        .requests()
        .iter()
        .filter(|request| request.path.contains("/check-runs"))
        .filter_map(|request| request.body.as_ref())
        .filter_map(|body| {
            body.pointer("/output/annotations")
                .and_then(Value::as_array)
        })
        .map(Vec::len)
        .collect();
    assert_eq!(batches, [50, 50, 20]);
    let check_runs = scene.forge.check_runs();
    assert_eq!(check_runs.len(), 1, "{check_runs:#?}");
    assert_eq!(check_runs[0].conclusion.as_deref(), Some("failure"));
    let output = check_runs[0].output.as_ref().expect("an output");
    let received: Vec<RecordedAnnotation> = (0..STEPS)
        .flat_map(|step| {
            (0..PER_STEP).map(move |number| RecordedAnnotation {
                path: format!("src/step{step}.rs"),
                start_line: u64::from(number + 1),
                end_line: u64::from(number + 1),
                start_column: None,
                end_column: None,
                annotation_level: kind(number).1.to_owned(),
                message: format!("problem {number} of step {step}"),
                title: None,
            })
        })
        .collect();
    assert_eq!(output.annotations, received);
}

#[tokio::test]
async fn a_step_summary_is_stored_once_served_byte_for_byte_and_cut_in_the_check_run() {
    let scene = Scene::start().await;
    scene.run_job().await;
    scene.reported().await;

    for step in 0..SUMMARIES {
        let (status, kind, body) = scene.summary_of(step as usize).await;
        assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
        assert_eq!(kind.as_deref(), Some("text/markdown; charset=utf-8"));
        assert_eq!(
            body,
            summary(step).into_bytes(),
            "the summary of step {step}"
        );
    }
    let (status, _, body) = scene.summary_of(SUMMARIES as usize).await;
    assert_eq!(status, 404);
    let answer: Value = serde_json::from_slice(&body).expect("an error envelope");
    assert_eq!(
        answer["error"]["message"],
        format!(
            "step `{}` of execution `{}` has no summary",
            scene.steps[SUMMARIES as usize], scene.execution
        )
    );

    let (status, execution) = scene
        .api
        .get(&format!("/api/v1/executions/{}", scene.execution))
        .await;
    assert_eq!(status, 200, "{execution}");
    let sizes: Vec<Value> = execution["steps"]
        .as_array()
        .expect("the steps")
        .iter()
        .map(|step| step["summary_size"].clone())
        .collect();
    let expected: Vec<Value> = (0..STEPS)
        .map(|step| {
            if step < SUMMARIES {
                json!(summary(step).len())
            } else {
                Value::Null
            }
        })
        .collect();
    assert_eq!(sizes, expected);
    let rows = strings(
        scene.database.connection(),
        "SELECT count(*)::text AS value FROM bosun.step_summary",
    )
    .await;
    assert_eq!(rows, [SUMMARIES.to_string()]);
    let objects = Directory::open(scene.store.path())
        .await
        .expect("the store")
        .list(&format!("summaries/{}/", scene.execution))
        .await
        .expect("a listing");
    let mut expected: Vec<String> = scene.steps[..SUMMARIES as usize]
        .iter()
        .map(|step| format!("summaries/{}/{step}.md", scene.execution))
        .collect();
    expected.sort();
    let listed: Vec<String> = objects.iter().map(ToString::to_string).collect();
    assert_eq!(listed, expected);

    let joined: String = (0..SUMMARIES)
        .map(|step| {
            let mut text = summary(step);
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text
        })
        .collect();
    assert!(joined.len() > SUMMARY_LIMIT);
    let mut end = SUMMARY_LIMIT - CUT_NOTICE.len();
    while !joined.is_char_boundary(end) {
        end -= 1;
    }
    let cut = format!("{}{CUT_NOTICE}", &joined[..end]);
    let check_runs = scene.forge.check_runs();
    let output = check_runs[0].output.as_ref().expect("an output");
    assert_eq!(output.title, "build");
    assert!(output.summary.len() <= SUMMARY_LIMIT);
    assert_eq!(output.summary, cut);
}

#[tokio::test]
async fn the_summary_of_a_step_answers_as_missing_for_what_does_not_exist() {
    let scene = Scene::start().await;
    let other = uuid::Uuid::now_v7();
    for (path, message) in [
        (
            format!(
                "/api/v1/executions/{}/steps/{other}/summary",
                scene.execution
            ),
            format!(
                "there is no step `{other}` in execution `{}`",
                scene.execution
            ),
        ),
        (
            format!(
                "/api/v1/executions/{other}/steps/{}/summary",
                scene.steps[0]
            ),
            format!(
                "there is no step `{}` in execution `{other}`",
                scene.steps[0]
            ),
        ),
        (
            format!("/api/v1/executions/{}/steps/first/summary", scene.execution),
            format!(
                "there is no step `first` in execution `{}`",
                scene.execution
            ),
        ),
    ] {
        let (status, answer) = scene.api.get(&path).await;
        assert_eq!(
            (status, &answer["error"]["message"]),
            (404, &json!(message))
        );
    }
}
