use bosun_runner::Event;
use bosun_types::status::Conclusion;

use crate::{job, run, step};

#[tokio::test(flavor = "multi_thread")]
async fn the_output_of_a_step_comes_before_the_end_of_the_step() {
    let directory = tempfile::tempdir().expect("a directory");
    let steps = (0..100)
        .map(|index| step(index, &format!("echo out {index}\necho err {index} >&2")))
        .collect();
    let finished = run(job(directory.path(), steps)).await;
    assert_eq!(finished.conclusion, Conclusion::Success);
    let mut ended = Vec::new();
    for event in &finished.events {
        match event {
            Event::Line { index, text, .. } => assert!(
                !ended.contains(index),
                "step {index} printed `{}` after it ended",
                String::from_utf8_lossy(text)
            ),
            Event::StepFinished { index, .. } => ended.push(*index),
            Event::StepStarted { .. } => {}
        }
    }
    for index in 0..100 {
        assert_eq!(
            finished.lines(index),
            vec![format!("out {index}"), format!("err {index}")]
        );
    }
}
