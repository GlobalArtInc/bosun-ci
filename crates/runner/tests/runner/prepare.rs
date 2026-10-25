use std::fs;

use bosun_types::status::Conclusion;

use crate::{job, run, step};

#[tokio::test(flavor = "multi_thread")]
async fn a_job_whose_workspace_cannot_be_created_fails_in_its_first_step_with_the_reason() {
    let directory = tempfile::tempdir().expect("a directory");
    let job = job(
        directory.path(),
        vec![step(3, "echo never"), step(4, "echo never")],
    );
    let parent = job.workspace.parent().expect("a parent of the workspace");
    fs::create_dir_all(parent).expect("a directory");
    fs::write(&job.workspace, "a file where the workspace belongs").expect("a file");
    let finished = run(job).await;
    assert_eq!(
        finished.conclusion,
        Conclusion::Failure,
        "{:#?}",
        finished.events
    );
    assert!(finished.started(3));
    assert_eq!(finished.outcome(3), Some(Conclusion::Failure));
    let lines = finished.lines(3);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("the job could not be prepared on this agent: "),
        "{lines:?}"
    );
    assert!(!finished.started(4), "{:#?}", finished.events);
}
