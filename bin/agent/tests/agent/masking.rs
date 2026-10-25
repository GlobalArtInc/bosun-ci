use std::collections::BTreeMap;

use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{AgentMessage, Conclusion, JobFinished, Log};
use bosun_types::ir::{Action, Step, Template};
use prost::Message as _;
use serde_json::{Value, json};

use crate::server::{FakeServer, Stream};
use crate::{Home, assignment_of, documents, step, text};

const SECRET: &str = "it's a \"quoted\" \\ value\non two lines";

fn template(text: &str) -> Template {
    bosun_expr::template(text).expect("a template")
}

fn run(text: &str) -> Step {
    let mut run = step("");
    run.action = Action::Run {
        script: template(text),
    };
    run
}

async fn finished(stream: &mut Stream) -> JobFinished {
    stream
        .until("the end of the job", |message| match message {
            Inbound::JobFinished(finished) => Some(finished.clone()),
            Inbound::Reject(reject) => panic!("the job was rejected: {reject:?}"),
            _ => None,
        })
        .await
}

fn lines_of(stream: &Stream, execution: &str) -> Vec<String> {
    let chunks: BTreeMap<u64, &Log> = stream
        .received
        .iter()
        .filter_map(|message| match message {
            Inbound::Log(log) if log.execution_id == execution => Some((log.seq, log)),
            _ => None,
        })
        .collect();
    text(&chunks.into_values().collect::<Vec<&Log>>())
}

#[tokio::test(flavor = "multi_thread")]
async fn no_form_of_a_secret_leaves_the_agent_and_a_dependent_job_sees_the_dropped_output_empty() {
    let mut server = FakeServer::start().await;
    let home = Home::registered(&mut server, &[]).await;
    let running = home.start();
    let mut stream = server.accept().await;
    stream.welcome(server.agent_id());
    stream.idle().await;

    let mut printing = run("printf '%s\\n' \"$TOKEN\"\n\
         printf '%s\\n' \"$JSON\"\n\
         printf '%s\\n' 'it%27s%20a%20%22quoted%22%20%5C%20value%0Aon%20two%20lines'\n\
         printf '%s\\n' 'it&apos;s a &quot;quoted&quot; \\ value'\n\
         printf '%s' \"$TOKEN\" | base64\n\
         printf '.%s' \"$TOKEN\" | base64\n\
         printf '..%s' \"$TOKEN\" | base64\n\
         printf 'token<<END\\n%s\\nEND\\n' \"$TOKEN\" >> \"$GITHUB_OUTPUT\"");
    printing.key = Some("print".parse().expect("a step id"));
    printing.env = [
        ("TOKEN".to_owned(), template("${{ secrets.TOKEN }}")),
        ("JSON".to_owned(), template("${{ toJSON(secrets.TOKEN) }}")),
    ]
    .into();
    let mut build = documents(vec![printing]);
    build
        .contexts
        .secrets
        .insert("TOKEN".to_owned(), json!(SECRET));
    build.job.outputs = [
        (
            "token".to_owned(),
            template("${{ steps.print.outputs.token }}"),
        ),
        ("version".to_owned(), Template::text("1.2.3")),
    ]
    .into();
    let job = assignment_of(&build);
    let execution = job.execution_id.clone();
    stream.send(Outgoing::Assignment(job));
    let built = finished(&mut stream).await;
    assert_eq!(built.conclusion(), Conclusion::Success);
    assert_eq!(
        built.outputs,
        [("version".to_owned(), "1.2.3".to_owned())].into()
    );
    let reported: Vec<Vec<(String, String)>> = stream
        .received
        .iter()
        .filter_map(|message| match message {
            Inbound::StepFinished(step) => Some(step.outputs.clone().into_iter().collect()),
            _ => None,
        })
        .collect();
    assert_eq!(reported, [[("token".to_owned(), "***".to_owned())]]);
    let lines = lines_of(&stream, &execution);
    assert_eq!(
        lines,
        [
            "***",
            "***",
            "\"***\"",
            "***",
            "***",
            "***",
            "Lm***",
            "Li5***",
            "the output `token` of the job is not reported, since it contains a masked value",
        ]
    );
    for message in &stream.received {
        let encoded = AgentMessage {
            message: Some(message.clone()),
        }
        .encode_to_vec();
        for part in SECRET.lines() {
            assert!(
                !encoded
                    .windows(part.len())
                    .any(|window| window == part.as_bytes()),
                "`{part}` left the agent in {message:?}"
            );
        }
    }
    stream.idle().await;

    let mut test = documents(vec![run(
        "echo \"[${{ needs.build.outputs.token }}] [${{ needs.build.outputs.version }}]\"",
    )]);
    test.contexts.needs = [(
        "build".to_owned(),
        json!({ "result": "success", "outputs": built.outputs }),
    )]
    .into_iter()
    .collect::<serde_json::Map<String, Value>>();
    let job = assignment_of(&test);
    let execution = job.execution_id.clone();
    stream.send(Outgoing::Assignment(job));
    assert_eq!(
        finished(&mut stream).await.conclusion(),
        Conclusion::Success
    );
    assert_eq!(lines_of(&stream, &execution), ["[] [1.2.3]"]);
    running.stop().await;
}
