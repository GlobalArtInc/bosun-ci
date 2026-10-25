use Milestone::{M0, M1, M2, M3, M5};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Milestone {
    M0,
    M1,
    M2,
    M3,
    M4,
    M5,
}

impl Milestone {
    pub const ALL: [Milestone; 6] = [
        Milestone::M0,
        Milestone::M1,
        Milestone::M2,
        Milestone::M3,
        Milestone::M4,
        Milestone::M5,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Milestone::M0 => "M0",
            Milestone::M1 => "M1",
            Milestone::M2 => "M2",
            Milestone::M3 => "M3",
            Milestone::M4 => "M4",
            Milestone::M5 => "M5",
        }
    }
}

pub const MILESTONE: Milestone = Milestone::M1;

pub const PENDING: &[(&str, &str)] = &[];

pub(crate) struct Arrival {
    name: &'static str,
    from: Milestone,
    extension: bool,
}

const fn key(name: &'static str, from: Milestone) -> Arrival {
    Arrival {
        name,
        from,
        extension: false,
    }
}

const fn extension(name: &'static str, from: Milestone) -> Arrival {
    Arrival {
        name,
        from,
        extension: true,
    }
}

pub(crate) struct Shape {
    pub(crate) noun: &'static str,
    place: &'static str,
    keys: &'static [Arrival],
    pub(crate) required: &'static [&'static str],
    pub(crate) loose: bool,
}

pub(crate) enum Known {
    Now,
    Later(Milestone),
    Unknown(Option<&'static str>),
}

pub(crate) enum Arrives {
    Listed { from: Milestone, extension: bool },
    Inherited,
    Unknown,
}

impl Shape {
    pub(crate) fn arrives(&self, name: &str) -> Arrives {
        match self.keys.iter().find(|arrival| arrival.name == name) {
            Some(arrival) => Arrives::Listed {
                from: arrival.from,
                extension: arrival.extension,
            },
            None => Arrives::Unknown,
        }
    }

    pub(crate) fn find(&self, name: &str) -> Known {
        match self.keys.iter().find(|arrival| arrival.name == name) {
            Some(arrival)
                if arrival.from <= MILESTONE && !PENDING.contains(&(self.place, arrival.name)) =>
            {
                Known::Now
            }
            Some(arrival) => Known::Later(arrival.from),
            None => Known::Unknown(suggest(name, self.keys.iter().map(|arrival| arrival.name))),
        }
    }

    pub(crate) fn extension(&self, name: &str) -> bool {
        self.keys
            .iter()
            .any(|arrival| arrival.name == name && arrival.extension)
    }
}

const fn shape(place: &'static str, keys: &'static [Arrival]) -> Shape {
    Shape {
        noun: "key",
        place,
        keys,
        required: &[],
        loose: false,
    }
}

pub(crate) const WORKFLOW: Shape = Shape {
    required: &["on", "jobs"],
    ..shape(
        "workflow",
        &[
            key("name", M0),
            key("run-name", M1),
            key("on", M0),
            key("permissions", M1),
            key("env", M0),
            key("defaults", M1),
            key("concurrency", M1),
            key("jobs", M0),
            key("cache-mode", M2),
            extension("priority", M2),
            extension("notify", M5),
        ],
    )
};

pub(crate) const EVENTS: Shape = Shape {
    noun: "event",
    ..shape(
        "on",
        &[
            key("workflow_dispatch", M0),
            key("push", M1),
            key("pull_request", M1),
            key("pull_request_target", M1),
            key("schedule", M1),
            key("merge_group", M1),
            key("repository_dispatch", M1),
            key("branch_protection_rule", M1),
            key("check_run", M1),
            key("check_suite", M1),
            key("create", M1),
            key("delete", M1),
            key("deployment", M1),
            key("deployment_status", M1),
            key("discussion", M1),
            key("discussion_comment", M1),
            key("fork", M1),
            key("gollum", M1),
            key("issue_comment", M1),
            key("issues", M1),
            key("label", M1),
            key("milestone", M1),
            key("page_build", M1),
            key("project", M1),
            key("project_card", M1),
            key("project_column", M1),
            key("public", M1),
            key("pull_request_review", M1),
            key("pull_request_review_comment", M1),
            key("registry_package", M1),
            key("release", M1),
            key("status", M1),
            key("watch", M1),
            key("workflow_call", M2),
            key("workflow_run", M2),
            extension("upstream", M2),
            extension("plugin", M5),
        ],
    )
};

pub(crate) const PUSH: Shape = shape(
    "push",
    &[
        key("branches", M1),
        key("branches-ignore", M1),
        key("tags", M1),
        key("tags-ignore", M1),
        key("paths", M1),
        key("paths-ignore", M1),
    ],
);

pub(crate) const PULL_REQUEST: Shape = shape(
    "pull_request",
    &[
        key("types", M1),
        key("branches", M1),
        key("branches-ignore", M1),
        key("paths", M1),
        key("paths-ignore", M1),
    ],
);

pub(crate) const MERGE_GROUP: Shape = shape(
    "merge_group",
    &[
        key("types", M1),
        key("branches", M1),
        key("branches-ignore", M1),
    ],
);

pub(crate) const ACTIVITY: Shape = shape("activity", &[key("types", M1)]);

pub(crate) const SCHEDULE: Shape = Shape {
    required: &["cron"],
    ..shape("schedule", &[key("cron", M1), key("timezone", M1)])
};

pub(crate) const WORKFLOW_DISPATCH: Shape = shape("workflow_dispatch", &[key("inputs", M1)]);

pub(crate) const INPUT: Shape = shape(
    "workflow_dispatch.inputs.<input_id>",
    &[
        key("description", M1),
        key("required", M1),
        key("default", M1),
        key("type", M1),
        key("options", M1),
    ],
);

pub(crate) const JOB: Shape = Shape {
    required: &["runs-on", "steps"],
    ..shape(
        "job",
        &[
            key("name", M0),
            key("needs", M0),
            key("if", M1),
            key("runs-on", M0),
            key("permissions", M1),
            key("concurrency", M1),
            key("strategy", M1),
            key("env", M0),
            key("defaults", M1),
            key("timeout-minutes", M1),
            key("continue-on-error", M1),
            key("outputs", M1),
            key("steps", M0),
            key("container", M2),
            key("services", M2),
            key("cache-mode", M2),
            key("uses", M2),
            key("with", M2),
            key("secrets", M2),
            extension("cache-volumes", M2),
            extension("priority", M2),
            key("environment", M3),
            extension("retry", M3),
            extension("debug", M3),
            extension("gate", M3),
        ],
    )
};

pub(crate) const RUNS_ON: Shape = shape("runs-on", &[key("group", M0), key("labels", M0)]);

pub(crate) const STEP: Shape = shape(
    "step",
    &[
        key("id", M0),
        key("name", M0),
        key("if", M1),
        key("uses", M1),
        key("run", M0),
        key("shell", M0),
        key("working-directory", M0),
        key("with", M1),
        key("env", M0),
        key("continue-on-error", M1),
        key("timeout-minutes", M1),
        extension("retry", M3),
        extension("pause", M3),
    ],
);

pub(crate) const DEFAULTS: Shape = shape("defaults", &[key("run", M1)]);

pub(crate) const DEFAULTS_RUN: Shape = shape(
    "defaults.run",
    &[key("shell", M1), key("working-directory", M1)],
);

pub(crate) const CONCURRENCY: Shape = Shape {
    required: &["group"],
    ..shape(
        "concurrency",
        &[
            key("group", M1),
            key("cancel-in-progress", M1),
            key("queue", M1),
        ],
    )
};

pub(crate) const STRATEGY: Shape = shape(
    "strategy",
    &[
        key("matrix", M1),
        key("fail-fast", M1),
        key("max-parallel", M1),
    ],
);

pub(crate) const ACTION: Shape = Shape {
    required: &["runs"],
    loose: true,
    ..shape(
        "root",
        &[
            key("name", M1),
            key("description", M1),
            key("author", M1),
            key("branding", M1),
            key("inputs", M1),
            key("outputs", M1),
            key("runs", M1),
        ],
    )
};

pub(crate) const BRANDING: Shape = Shape {
    loose: true,
    ..shape("branding", &[key("icon", M1), key("color", M1)])
};

pub(crate) const ACTION_INPUT: Shape = Shape {
    loose: true,
    ..shape(
        "inputs.<input_id>",
        &[
            key("description", M1),
            key("required", M1),
            key("default", M1),
            key("deprecationMessage", M1),
        ],
    )
};

pub(crate) const ACTION_OUTPUT: Shape = shape(
    "outputs.<output_id>",
    &[key("description", M1), key("value", M2)],
);

pub(crate) const NODE_RUNS: Shape = Shape {
    required: &["using", "main"],
    ..shape(
        "runs",
        &[
            key("using", M1),
            key("main", M1),
            key("pre", M1),
            key("pre-if", M1),
            key("post", M1),
            key("post-if", M1),
        ],
    )
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
