use bosun_forge::{CheckRun, CheckStatus, CommitStatus, Error, Forge, StatusState};
use bosun_testkit::forge::{RecordedCheckRun, RecordedStatus};
use bosun_types::status::Conclusion;

use crate::{FULL_NAME, setup};

const DETAILS: &str = "https://ci.example.org/runs/1";

#[tokio::test]
async fn a_check_run_is_created_and_updated() {
    let setup = setup().await;
    let commit = setup
        .forge
        .commit(FULL_NAME, "main", [("README.md", "app")]);
    let mut check_run = CheckRun {
        name: "Build (linux, 18)".to_owned(),
        details_url: Some(DETAILS.to_owned()),
        status: CheckStatus::Queued,
    };
    let id = setup
        .app
        .create_check_run(&setup.repository, &commit, &check_run)
        .await
        .unwrap_or_else(|error| panic!("the check run is not created: {error}"));
    let mut expected = RecordedCheckRun {
        id,
        repository: FULL_NAME.to_owned(),
        head_sha: commit.clone(),
        name: check_run.name.clone(),
        details_url: Some(DETAILS.to_owned()),
        status: "queued".to_owned(),
        conclusion: None,
    };
    assert_eq!(setup.forge.check_runs(), [expected.clone()]);
    for (status, recorded, conclusion) in [
        (CheckStatus::InProgress, "in_progress", None),
        (
            CheckStatus::Completed(Conclusion::TimedOut),
            "completed",
            Some("timed_out"),
        ),
    ] {
        check_run.status = status;
        setup
            .app
            .update_check_run(&setup.repository, id, &check_run)
            .await
            .unwrap_or_else(|error| panic!("the check run is not updated: {error}"));
        expected.status = recorded.to_owned();
        expected.conclusion = conclusion.map(str::to_owned);
        assert_eq!(setup.forge.check_runs(), [expected.clone()]);
    }
    let missing = setup
        .app
        .update_check_run(&setup.repository, id + 1, &check_run)
        .await;
    assert!(
        matches!(missing, Err(Error::Status { status: 404, .. })),
        "{missing:?}"
    );
}

#[tokio::test]
async fn a_commit_status_is_created() {
    let setup = setup().await;
    let commit = setup
        .forge
        .commit(FULL_NAME, "main", [("README.md", "app")]);
    let status = CommitStatus {
        context: "bosun/ci/build".to_owned(),
        state: StatusState::Failure,
        description: Some("the build failed".to_owned()),
        target_url: Some(DETAILS.to_owned()),
    };
    setup
        .app
        .create_status(&setup.repository, &commit, &status)
        .await
        .unwrap_or_else(|error| panic!("the status is not created: {error}"));
    assert_eq!(
        setup.forge.statuses(),
        [RecordedStatus {
            repository: FULL_NAME.to_owned(),
            sha: commit.clone(),
            context: "bosun/ci/build".to_owned(),
            state: "failure".to_owned(),
            description: Some("the build failed".to_owned()),
            target_url: Some(DETAILS.to_owned()),
        }]
    );
    let unknown = setup
        .app
        .create_status(&setup.repository, &"0".repeat(40), &status)
        .await;
    assert!(
        matches!(unknown, Err(Error::Status { status: 422, .. })),
        "{unknown:?}"
    );
}
