use bosun_forge::{
    ANNOTATIONS_PER_REQUEST, CheckOutput, CheckRun, CheckStatus, CommitStatus, Error, Forge,
    StatusState,
};
use bosun_testkit::forge::{RecordedAnnotation, RecordedCheckRun, RecordedOutput, RecordedStatus};
use bosun_types::api::{Annotation, AnnotationKind};
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
        output: None,
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
        output: None,
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

fn annotation(kind: AnnotationKind, message: &str) -> Annotation {
    Annotation {
        kind,
        message: message.to_owned(),
        title: None,
        file: None,
        line: None,
        end_line: None,
        column: None,
        end_column: None,
    }
}

#[tokio::test]
async fn the_output_of_a_check_run_replaces_its_summary_and_appends_its_annotations() {
    let setup = setup().await;
    let commit = setup
        .forge
        .commit(FULL_NAME, "main", [("README.md", "app")]);
    let located = Annotation {
        title: Some("t".repeat(300)),
        file: Some("src/app.ts".to_owned()),
        line: Some(4),
        end_line: Some(4),
        column: Some(2),
        end_column: Some(9),
        ..annotation(AnnotationKind::Error, "expected `;`")
    };
    let spanning = Annotation {
        file: Some("src/app.ts".to_owned()),
        line: Some(7),
        end_line: Some(12),
        column: Some(3),
        end_column: Some(1),
        ..annotation(AnnotationKind::Warning, "unused import")
    };
    let mut check_run = CheckRun {
        name: "build".to_owned(),
        details_url: None,
        status: CheckStatus::InProgress,
        output: Some(CheckOutput {
            title: "build".to_owned(),
            summary: "## First".to_owned(),
            annotations: vec![located, spanning],
        }),
    };
    let id = setup
        .app
        .create_check_run(&setup.repository, &commit, &check_run)
        .await
        .unwrap_or_else(|error| panic!("the check run is not created: {error}"));
    check_run.status = CheckStatus::Completed(Conclusion::Failure);
    check_run.output = Some(CheckOutput {
        title: "build".to_owned(),
        summary: "## Second".to_owned(),
        annotations: vec![Annotation {
            line: Some(0),
            ..annotation(AnnotationKind::Notice, &"\u{e9}".repeat(40_000))
        }],
    });
    setup
        .app
        .update_check_run(&setup.repository, id, &check_run)
        .await
        .unwrap_or_else(|error| panic!("the check run is not updated: {error}"));

    let recorded = setup.forge.check_runs();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].conclusion.as_deref(), Some("failure"));
    assert_eq!(
        recorded[0].output,
        Some(RecordedOutput {
            title: "build".to_owned(),
            summary: "## Second".to_owned(),
            annotations: vec![
                RecordedAnnotation {
                    path: "src/app.ts".to_owned(),
                    start_line: 4,
                    end_line: 4,
                    start_column: Some(2),
                    end_column: Some(9),
                    annotation_level: "failure".to_owned(),
                    message: "expected `;`".to_owned(),
                    title: Some("t".repeat(255)),
                },
                RecordedAnnotation {
                    path: "src/app.ts".to_owned(),
                    start_line: 7,
                    end_line: 12,
                    start_column: None,
                    end_column: None,
                    annotation_level: "warning".to_owned(),
                    message: "unused import".to_owned(),
                    title: None,
                },
                RecordedAnnotation {
                    path: ".github".to_owned(),
                    start_line: 1,
                    end_line: 1,
                    start_column: None,
                    end_column: None,
                    annotation_level: "notice".to_owned(),
                    message: "\u{e9}".repeat(32_767),
                    title: None,
                },
            ],
        })
    );

    check_run.output = Some(CheckOutput {
        title: "build".to_owned(),
        summary: "## Third".to_owned(),
        annotations: vec![
            annotation(AnnotationKind::Notice, "one too many");
            ANNOTATIONS_PER_REQUEST + 1
        ],
    });
    let refused = setup
        .app
        .update_check_run(&setup.repository, id, &check_run)
        .await;
    assert!(
        matches!(refused, Err(Error::Status { status: 422, .. })),
        "{refused:?}"
    );
}
