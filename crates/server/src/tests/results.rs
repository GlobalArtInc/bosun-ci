use std::collections::BTreeMap;

use bosun_types::api::{Annotation, AnnotationKind};
use bosun_types::status::Conclusion;

use super::{World, finished, template, workflow};
use crate::agents::progress::{self, Progress, StepEnd};

const MANY: u32 = 2_500;

fn numbered(number: u32) -> Annotation {
    Annotation {
        kind: AnnotationKind::Notice,
        message: format!("notice {number}"),
        title: None,
        file: Some("src/lib.rs".to_owned()),
        line: Some(number),
        end_line: None,
        column: None,
        end_column: None,
    }
}

#[tokio::test]
async fn a_step_keeps_more_annotations_than_one_insert_holds_in_the_order_reported() {
    let world = World::new().await;
    let run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let (job, _) = world.project(run).await.job("build");
    let execution = world.start(run, job).await;
    progress::step_started(world.db(), world.agent(), execution, 0, None)
        .await
        .expect("a started step");
    let ended = progress::step_finished(
        world.db(),
        world.store(),
        world.agent(),
        execution,
        StepEnd {
            index: 0,
            at: None,
            outcome: Conclusion::Success,
            conclusion: Conclusion::Success,
            outputs: BTreeMap::new(),
            annotations: (1..=MANY).map(numbered).collect(),
            summary: None,
        },
    )
    .await
    .expect("a finished step");
    assert_eq!(ended, Progress::Applied);
    world.end(execution, finished(Conclusion::Success)).await;

    let shown = bosun_db::views::execution(world.db(), execution)
        .await
        .expect("a read")
        .expect("the execution");
    let annotations = &shown.steps[0].annotations;
    assert_eq!(
        annotations,
        &(1..=MANY).map(numbered).collect::<Vec<Annotation>>()
    );
    assert_eq!(shown.steps[0].summary_size, None);
    assert_eq!(
        bosun_db::results::of_job(world.db(), job)
            .await
            .expect("the results of the job")
            .annotations
            .len(),
        MANY as usize
    );
}
