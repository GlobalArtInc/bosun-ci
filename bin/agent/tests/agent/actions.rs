use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{Conclusion, Log};
use bosun_proto::assignment;
use bosun_types::ir::{Action, ActionReference, Step, Template};
use flate2::Compression as Level;
use flate2::write::GzEncoder;
use lzma_rust2::{XzOptions, XzWriter};
use serde_json::json;

use crate::server::{CREDENTIAL, Download, FakeServer, Stream, logs};
use crate::{Home, assignment_of, documents, step, text};

const COMMIT: &str = "5b0c0de0c0ffee5b0c0de0c0ffee5b0c0de0c0ff";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat/fixtures/actions")
}

fn node() -> PathBuf {
    let found = env::var_os("PATH")
        .and_then(|path| {
            env::split_paths(&path)
                .map(|directory| directory.join("node"))
                .find(|candidate| candidate.is_file())
        })
        .unwrap_or_else(|| panic!("the tests of Node actions need `node` on the PATH"));
    fs::canonicalize(&found).unwrap_or_else(|error| panic!("{}: {error}", found.display()))
}

fn version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let output = Command::new(node())
            .arg("--version")
            .output()
            .expect("node runs");
        let printed = String::from_utf8(output.stdout).expect("UTF-8");
        let version = printed.trim().trim_start_matches('v').to_owned();
        assert!(
            version.starts_with("24."),
            "the tests of Node actions need Node 24 on the PATH, not {version}"
        );
        version
    })
}

fn platform() -> String {
    let os = match env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os}-{arch}")
}

fn tarball() -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Level::fast()));
    let prefix = format!("bosun-fixtures-actions-{}", &COMMIT[..7]);
    add(&mut builder, &fixtures(), &prefix);
    builder
        .into_inner()
        .expect("a tar archive")
        .finish()
        .expect("a tarball")
}

fn add<W: Write>(builder: &mut tar::Builder<W>, directory: &Path, prefix: &str) {
    for entry in fs::read_dir(directory).expect("the fixture actions") {
        let path = entry.expect("an entry").path();
        let name = path
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned();
        if name == "node_modules" {
            continue;
        }
        let inside = format!("{prefix}/{name}");
        if path.is_dir() {
            add(builder, &path, &inside);
        } else {
            builder
                .append_path_with_name(&path, &inside)
                .expect("a file of the fixture actions");
        }
    }
}

fn node_archive() -> &'static [u8] {
    static ARCHIVE: OnceLock<Vec<u8>> = OnceLock::new();
    ARCHIVE.get_or_init(|| {
        let mut writer =
            XzWriter::new(Vec::new(), XzOptions::with_preset(0)).expect("an xz writer");
        {
            let mut builder = tar::Builder::new(&mut writer);
            builder
                .append_path_with_name(
                    node(),
                    format!("node-v{}-{}/bin/node", version(), platform()),
                )
                .expect("the node of the host");
            builder.finish().expect("a tar archive");
        }
        writer.finish().expect("an xz archive")
    })
}

fn reference(name: &str) -> ActionReference {
    ActionReference {
        owner: "bosun-fixtures".to_owned(),
        repository: "actions".to_owned(),
        path: Some(name.to_owned()),
        r#ref: "v1".to_owned(),
    }
}

fn resolved(name: &str) -> assignment::Action {
    let file = format!("{name}/action.yml");
    let source = fs::read_to_string(fixtures().join(&file)).expect("the metadata");
    let parsed = bosun_workflow::action(&file, &source)
        .unwrap_or_else(|diagnostics| panic!("{file}: {diagnostics:#?}"));
    assignment::Action {
        uses: reference(name),
        commit: COMMIT.to_owned(),
        download: download(),
        metadata: parsed.metadata,
        node: version().to_owned(),
    }
}

fn download() -> String {
    format!("/_runtime/actions/bosun-fixtures/actions/{COMMIT}.tar.gz")
}

fn runtime() -> String {
    format!("/_runtime/tools/node/{}/{}.tar.xz", version(), platform())
}

fn uses(name: &str, id: Option<&str>, with: &[(&str, &str)]) -> Step {
    let mut uses = step("");
    uses.key = id.map(|id| id.parse().expect("a step id"));
    uses.action = Action::Uses(reference(name));
    uses.with = with
        .iter()
        .map(|(input, value)| {
            (
                (*input).to_owned(),
                bosun_expr::template(value).expect("a template"),
            )
        })
        .collect();
    uses
}

struct Ran {
    reports: Vec<String>,
    conclusion: Conclusion,
    logs: BTreeMap<u32, Vec<String>>,
}

async fn ran(stream: &mut Stream, steps: Vec<Step>, actions: &[&str]) -> Ran {
    let mut documents = documents(steps);
    documents.actions = actions.iter().map(|name| resolved(name)).collect();
    documents.contexts.github = json!({ "job": "build", "workspace": "" })
        .as_object()
        .cloned()
        .expect("an object");
    let job = assignment_of(&documents);
    let execution = job.execution_id.clone();
    let first = stream.received.len();
    stream.send(Outgoing::Assignment(job));
    let mut reports = Vec::new();
    let conclusion = loop {
        match stream.next().await {
            Inbound::StepStarted(started) => {
                reports.push(format!("started {}", started.step_index))
            }
            Inbound::StepFinished(finished) => reports.push(format!(
                "finished {} {:?}",
                finished.step_index,
                finished.conclusion()
            )),
            Inbound::JobFinished(finished) => {
                assert_eq!(finished.execution_id, execution);
                break finished.conclusion();
            }
            Inbound::Accept(_) | Inbound::Log(_) => {}
            other => panic!("an unexpected message during the job: {other:?}"),
        }
    };
    stream.idle().await;
    let chunks = logs(&stream.received[first..]);
    let mut by_step: BTreeMap<u32, Vec<&Log>> = BTreeMap::new();
    for log in chunks.values() {
        by_step.entry(log.step_index).or_default().push(log);
    }
    Ran {
        reports,
        conclusion,
        logs: by_step
            .into_iter()
            .map(|(index, chunks)| (index, text(&chunks)))
            .collect(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_agent_fetches_the_actions_and_the_node_of_a_job_once_and_runs_pre_main_and_post() {
    let mut server = FakeServer::start().await;
    server.serve(&download(), tarball());
    server.serve(&runtime(), node_archive().to_vec());
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let tools = home.dir().join("added-tools").display().to_string();
    let steps = || {
        vec![
            uses(
                "first",
                Some("first"),
                &[
                    ("value", "round trip"),
                    ("directory", &tools),
                    ("secret", "a-secret-of-the-action"),
                ],
            ),
            uses("second", None, &[]),
            step("echo \"variable $FIRST_VALUE\""),
        ]
    };
    let first = ran(&mut stream, steps(), &["first", "second"]).await;
    assert_eq!(first.conclusion, Conclusion::Success, "{:#?}", first.logs);
    assert_eq!(
        first.reports,
        (0..7)
            .flat_map(|index| [
                format!("started {index}"),
                format!("finished {index} Success")
            ])
            .collect::<Vec<_>>()
    );
    assert_eq!(first.logs[&0], ["pre first"]);
    assert_eq!(first.logs[&1], ["pre second"]);
    assert!(
        first.logs[&2].contains(&"secret ***".to_owned()),
        "{:?}",
        first.logs[&2]
    );
    let root = home
        .dir()
        .join(format!("work/_actions/bosun-fixtures/actions/{COMMIT}"));
    assert!(
        first.logs[&2].contains(&format!(
            "GITHUB_ACTION_PATH={}",
            root.join("first").display()
        )),
        "{:?}",
        first.logs[&2]
    );
    assert_eq!(
        first.logs[&3],
        [
            "##[warning]the action `bosun-fixtures/actions/second@v1` requires the input `name`, \
             which the step does not supply"
                .to_owned(),
            "main second".to_owned(),
            "state saved by the pre step of second".to_owned(),
            format!("node v{}", version()),
            "name ".to_owned(),
        ]
    );
    assert_eq!(first.logs[&4], ["variable round trip"]);
    assert_eq!(
        first.logs[&5],
        [
            "post second",
            "state saved by the pre step of second, saved by the main step of second"
        ]
    );
    assert_eq!(
        first.logs[&6],
        [
            "post first",
            "state saved by the pre step of first, saved by the main step of first"
        ]
    );
    let fetched = |path: String| Download {
        path,
        authorization: Some(format!("Bearer {CREDENTIAL}")),
    };
    assert_eq!(
        server.downloads(),
        [fetched(download()), fetched(runtime())]
    );
    assert!(root.join("first/action.yml").is_file());
    assert!(home.dir().join("externals/node24/bin/node").is_file());
    assert_eq!(
        fs::read_to_string(home.dir().join("externals/node24.version")).expect("the marker"),
        format!("{} {}\n", version(), platform())
    );

    let again = ran(&mut stream, steps(), &["first", "second"]).await;
    assert_eq!(again.conclusion, Conclusion::Success, "{:#?}", again.logs);
    assert_eq!(server.downloads().len(), 2, "a second job fetched again");
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_action_the_server_does_not_serve_fails_the_job_in_its_first_step() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let mut never = step("echo never");
    never.name = Some(Template::text("never"));
    let failed = ran(
        &mut stream,
        vec![uses("failure", None, &[]), never],
        &["failure"],
    )
    .await;
    assert_eq!(failed.conclusion, Conclusion::Failure);
    assert_eq!(failed.reports, ["started 0", "finished 0 Failure"]);
    assert_eq!(
        failed.logs[&0],
        [format!(
            "the job could not be prepared on this agent: {}{} answered 404 Not Found: the fake \
             server keeps nothing at {}",
            server.url(),
            download(),
            download()
        )]
    );
    assert!(
        !home
            .dir()
            .join(format!("work/_actions/bosun-fixtures/actions/{COMMIT}"))
            .exists()
    );
    running.stop().await;
}
