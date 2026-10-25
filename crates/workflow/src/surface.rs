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
    now: &[
        "workflow_dispatch",
        "push",
        "pull_request",
        "pull_request_target",
        "schedule",
        "merge_group",
        "repository_dispatch",
        "branch_protection_rule",
        "check_run",
        "check_suite",
        "create",
        "delete",
        "deployment",
        "deployment_status",
        "discussion",
        "discussion_comment",
        "fork",
        "gollum",
        "issue_comment",
        "issues",
        "label",
        "milestone",
        "page_build",
        "project",
        "project_card",
        "project_column",
        "public",
        "pull_request_review",
        "pull_request_review_comment",
        "registry_package",
        "release",
        "status",
        "watch",
    ],
    later: &[
        ("workflow_call", "M2"),
        ("workflow_run", "M2"),
        ("upstream", "M2"),
        ("plugin", "M5"),
    ],
    required: &[],
};

pub(crate) const PUSH: Shape = Shape {
    noun: "key",
    now: &[
        "branches",
        "branches-ignore",
        "tags",
        "tags-ignore",
        "paths",
        "paths-ignore",
    ],
    later: &[],
    required: &[],
};

pub(crate) const PULL_REQUEST: Shape = Shape {
    noun: "key",
    now: &[
        "types",
        "branches",
        "branches-ignore",
        "paths",
        "paths-ignore",
    ],
    later: &[],
    required: &[],
};

pub(crate) const MERGE_GROUP: Shape = Shape {
    noun: "key",
    now: &["types", "branches", "branches-ignore"],
    later: &[],
    required: &[],
};

pub(crate) const ACTIVITY: Shape = Shape {
    noun: "key",
    now: &["types"],
    later: &[],
    required: &[],
};

pub(crate) const SCHEDULE: Shape = Shape {
    noun: "key",
    now: &["cron", "timezone"],
    later: &[],
    required: &["cron"],
};

pub(crate) const WORKFLOW_DISPATCH: Shape = Shape {
    noun: "key",
    now: &["inputs"],
    later: &[],
    required: &[],
};

pub(crate) const INPUT: Shape = Shape {
    noun: "key",
    now: &["description", "required", "default", "type", "options"],
    later: &[],
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
