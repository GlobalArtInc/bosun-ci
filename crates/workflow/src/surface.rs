use crate::yaml::Value;

pub(crate) struct Shape {
    pub(crate) noun: &'static str,
    pub(crate) now: &'static [&'static str],
    pub(crate) later: &'static [(&'static str, &'static str)],
    pub(crate) required: &'static [&'static str],
}

pub(crate) enum Known {
    Now,
    Later(&'static str),
    Unknown(Option<&'static str>),
}

impl Shape {
    pub(crate) fn find(&self, name: &str, value: &Value) -> Known {
        if self.now.contains(&name) {
            return Known::Now;
        }
        match self.later.iter().find(|(key, _)| *key == name) {
            Some(&(_, milestone)) => Known::Later(arrives(name, value, milestone)),
            None => Known::Unknown(suggest(
                name,
                self.now
                    .iter()
                    .copied()
                    .chain(self.later.iter().map(|&(key, _)| key)),
            )),
        }
    }
}

fn arrives(name: &str, value: &Value, milestone: &'static str) -> &'static str {
    match (name, value) {
        ("uses", Value::String(reference))
            if reference.starts_with("./") || reference.starts_with("docker://") =>
        {
            "M2"
        }
        _ => milestone,
    }
}

pub(crate) const WORKFLOW: Shape = Shape {
    noun: "key",
    now: &["name", "on", "env", "jobs"],
    later: &[
        ("run-name", "M1"),
        ("permissions", "M1"),
        ("defaults", "M1"),
        ("concurrency", "M1"),
        ("cache-mode", "M2"),
        ("priority", "M2"),
        ("notify", "M5"),
    ],
    required: &["on", "jobs"],
};

pub(crate) const EVENTS: Shape = Shape {
    noun: "event",
    now: &["workflow_dispatch"],
    later: &[
        ("push", "M1"),
        ("pull_request", "M1"),
        ("pull_request_target", "M1"),
        ("schedule", "M1"),
        ("merge_group", "M1"),
        ("repository_dispatch", "M1"),
        ("branch_protection_rule", "M1"),
        ("check_run", "M1"),
        ("check_suite", "M1"),
        ("create", "M1"),
        ("delete", "M1"),
        ("deployment", "M1"),
        ("deployment_status", "M1"),
        ("discussion", "M1"),
        ("discussion_comment", "M1"),
        ("fork", "M1"),
        ("gollum", "M1"),
        ("issue_comment", "M1"),
        ("issues", "M1"),
        ("label", "M1"),
        ("milestone", "M1"),
        ("page_build", "M1"),
        ("project", "M1"),
        ("project_card", "M1"),
        ("project_column", "M1"),
        ("public", "M1"),
        ("pull_request_review", "M1"),
        ("pull_request_review_comment", "M1"),
        ("registry_package", "M1"),
        ("release", "M1"),
        ("status", "M1"),
        ("watch", "M1"),
        ("workflow_call", "M2"),
        ("workflow_run", "M2"),
        ("upstream", "M2"),
        ("plugin", "M5"),
    ],
    required: &[],
};

pub(crate) const WORKFLOW_DISPATCH: Shape = Shape {
    noun: "key",
    now: &[],
    later: &[("inputs", "M1")],
    required: &[],
};

pub(crate) const JOB: Shape = Shape {
    noun: "key",
    now: &["name", "needs", "runs-on", "env", "steps"],
    later: &[
        ("if", "M1"),
        ("permissions", "M1"),
        ("concurrency", "M1"),
        ("outputs", "M1"),
        ("defaults", "M1"),
        ("timeout-minutes", "M1"),
        ("continue-on-error", "M1"),
        ("strategy", "M1"),
        ("container", "M2"),
        ("services", "M2"),
        ("cache-mode", "M2"),
        ("uses", "M2"),
        ("with", "M2"),
        ("secrets", "M2"),
        ("cache-volumes", "M2"),
        ("priority", "M2"),
        ("environment", "M3"),
        ("retry", "M3"),
        ("debug", "M3"),
        ("gate", "M3"),
    ],
    required: &["runs-on", "steps"],
};

pub(crate) const RUNS_ON: Shape = Shape {
    noun: "key",
    now: &["group", "labels"],
    later: &[],
    required: &[],
};

pub(crate) const STEP: Shape = Shape {
    noun: "key",
    now: &["id", "name", "run", "shell", "working-directory", "env"],
    later: &[
        ("if", "M1"),
        ("uses", "M1"),
        ("with", "M1"),
        ("continue-on-error", "M1"),
        ("timeout-minutes", "M1"),
        ("retry", "M3"),
        ("pause", "M3"),
    ],
    required: &["run"],
};

pub(crate) fn suggest<'a>(
    name: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let limit = name.chars().count().max(3) / 3;
    candidates
        .into_iter()
        .map(|candidate| (distance(name, candidate), candidate))
        .filter(|&(distance, _)| distance <= limit)
        .min_by_key(|&(distance, _)| distance)
        .map(|(_, candidate)| candidate)
}

fn distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.to_lowercase().chars().collect();
    let right: Vec<char> = right.to_lowercase().chars().collect();
    let mut table = vec![vec![0; right.len() + 1]; left.len() + 1];
    for (i, row) in table.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in table[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=left.len() {
        for j in 1..=right.len() {
            let substitution = table[i - 1][j - 1] + usize::from(left[i - 1] != right[j - 1]);
            let mut best = substitution
                .min(table[i - 1][j] + 1)
                .min(table[i][j - 1] + 1);
            if i > 1 && j > 1 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1] {
                best = best.min(table[i - 2][j - 2] + 1);
            }
            table[i][j] = best;
        }
    }
    table[left.len()][right.len()]
}
