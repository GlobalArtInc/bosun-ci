use bosun_runner::Tracking;
use bosun_types::status::Conclusion;

use crate::{gone, job, pid, run, step};

const REQUIRE: &str = "BOSUN_TEST_REQUIRE_CGROUPS";

#[tokio::test(flavor = "multi_thread")]
async fn a_process_that_leaves_the_process_group_of_its_step_dies_with_the_cgroup_of_its_job() {
    let root = match Tracking::probe() {
        Tracking::Cgroups(root) => root,
        Tracking::ProcessGroups(reason)
            if std::env::var_os(REQUIRE).is_some_and(|set| set == "1") =>
        {
            panic!("{REQUIRE} is set, but this test process cannot create cgroups: {reason}");
        }
        Tracking::ProcessGroups(reason) => {
            eprintln!(
                "this test process cannot create cgroups, so jobs here are tracked by process \
                 group and there is no cgroup to check; set {REQUIRE}=1 to fail instead: {reason}"
            );
            return;
        }
    };
    let directory = tempfile::tempdir().expect("a directory");
    let job = job(
        directory.path(),
        vec![step(
            "setsid sleep 300 > /dev/null 2>&1 < /dev/null &\necho \"setsid $!\"",
        )],
    );
    let cgroup = root.join(format!("bosun-job-{}", job.id));
    let finished = run(job).await;
    assert_eq!(
        finished.conclusion,
        Conclusion::Success,
        "{:#?}",
        finished.events
    );
    let pid = pid(&finished.lines(0), "setsid");
    assert!(gone(pid).await, "the process {pid:?} outlived its job");
    assert!(!cgroup.exists(), "{} was left behind", cgroup.display());
}
