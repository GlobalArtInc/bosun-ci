use bosun_types::status::Conclusion;

use crate::{gone, job, pid, run, step};

#[tokio::test(flavor = "multi_thread")]
async fn background_processes_a_step_detached_are_gone_after_the_job() {
    let directory = tempfile::tempdir().expect("a directory");
    let finished = run(job(
        directory.path(),
        vec![
            step(
                0,
                "nohup sleep 300 > /dev/null 2>&1 &\n\
                 echo \"nohup $!\"\n\
                 ( sleep 300 > /dev/null 2>&1 & echo \"subshell $!\" )\n",
            ),
            step(1, "echo second"),
        ],
    ))
    .await;
    assert_eq!(
        finished.conclusion,
        Conclusion::Success,
        "{:#?}",
        finished.events
    );
    assert_eq!(finished.lines(1), vec!["second"]);
    let lines = finished.lines(0);
    for label in ["nohup", "subshell"] {
        let pid = pid(&lines, label);
        assert!(
            gone(pid).await,
            "the {label} process {pid:?} outlived its job"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_background_process_lives_on_into_the_next_step() {
    let directory = tempfile::tempdir().expect("a directory");
    let finished = run(job(
        directory.path(),
        vec![
            step(
                0,
                "sleep 300 > /dev/null 2>&1 &\n\
                 echo \"$!\" > \"$RUNNER_TEMP/pid\"\n\
                 echo \"background $!\"\n",
            ),
            step(1, "kill -0 \"$(cat \"$RUNNER_TEMP/pid\")\" && echo alive"),
        ],
    ))
    .await;
    assert_eq!(
        finished.conclusion,
        Conclusion::Success,
        "{:#?}",
        finished.events
    );
    assert_eq!(finished.lines(1), vec!["alive"]);
    assert!(gone(pid(&finished.lines(0), "background")).await);
}
