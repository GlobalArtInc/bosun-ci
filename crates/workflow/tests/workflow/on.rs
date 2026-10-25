use bosun_types::diagnostic::Diagnostic;
use bosun_types::ir::{Cron, CronDays, Trigger};

use crate::documents::arrivals;
use crate::{FILE, invalid, valid};

const JOBS: &str = "jobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n";

fn with_on(on: &str) -> String {
    format!("on:{on}\n{JOBS}")
}

fn only(source: &str) -> Diagnostic {
    let mut diagnostics = invalid(source);
    assert_eq!(diagnostics.len(), 1, "{source}\nproduced {diagnostics:#?}");
    diagnostics.remove(0)
}

fn event_of(trigger: &Trigger) -> &str {
    match trigger {
        Trigger::Push(_) => "push",
        Trigger::PullRequest(_) => "pull_request",
        Trigger::PullRequestTarget(_) => "pull_request_target",
        Trigger::MergeGroup(_) => "merge_group",
        Trigger::Schedule(_) => "schedule",
        Trigger::WorkflowDispatch(_) => "workflow_dispatch",
        Trigger::RepositoryDispatch(_) => "repository_dispatch",
        Trigger::Generic(generic) => &generic.name,
    }
}

fn cron(expression: &str) -> Cron {
    let workflow = valid(&with_on(&format!(
        "\n  schedule:\n    - cron: '{expression}'"
    )));
    match workflow.triggers.as_slice() {
        [Trigger::Schedule(schedules)] => schedules[0].cron.clone(),
        other => panic!("{expression} lowered to {other:?}"),
    }
}

#[test]
fn every_event_lowers_alike_in_every_form() {
    let events: Vec<String> = arrivals()
        .into_iter()
        .filter(|(place, _, milestone)| place == "on" && ["M0", "M1"].contains(&milestone.as_str()))
        .map(|(_, key, _)| key)
        .collect();
    assert_eq!(events.len(), 33, "{events:?}");
    for event in events.iter().filter(|event| *event != "schedule") {
        let forms = [
            format!(" {event}"),
            format!(" [{event}]"),
            format!("\n  {event}:"),
            format!("\n  {event}: {{}}"),
        ];
        let lowered: Vec<Vec<Trigger>> = forms
            .iter()
            .map(|form| valid(&with_on(form)).triggers)
            .collect();
        assert_eq!(lowered[0].len(), 1, "{event}");
        assert_eq!(event_of(&lowered[0][0]), event);
        for (form, triggers) in forms.iter().zip(&lowered) {
            assert_eq!(triggers, &lowered[0], "`on:{form}`");
        }
    }
}

#[test]
fn schedule_needs_the_mapping_form() {
    for form in [" schedule", " [push, schedule]"] {
        let diagnostic = only(&with_on(form));
        assert_eq!(diagnostic.code, "invalid_value", "{form}");
        assert_eq!(
            diagnostic.message,
            "`schedule` needs a list of `cron` entries, which only the mapping form of `on` can give it"
        );
    }
    let cases = [
        (
            "\n  schedule:",
            "invalid_type",
            "`schedule` must be a list of `cron` entries",
        ),
        (
            "\n  schedule: []",
            "invalid_value",
            "`schedule` lists no `cron` entries",
        ),
        (
            "\n  schedule:\n    - timezone: UTC",
            "missing_key",
            "missing key `cron` in entry 1 of `schedule`",
        ),
    ];
    for (form, code, message) in cases {
        let diagnostic = only(&with_on(form));
        assert_eq!(
            (diagnostic.code.as_str(), diagnostic.message.as_str()),
            (code, message),
            "{form}"
        );
    }
}

#[test]
fn triggers_come_back_from_a_file_whose_jobs_are_broken() {
    let source = "on:\n  push:\n    branches: [main]\njobs:\n  build:\n    runs-on: shell\n    colour: red\n    steps:\n      - run: make\n";
    let errors = invalid(source);
    let (triggers, diagnostics) = bosun_workflow::triggers(FILE, source);
    assert_eq!(diagnostics, errors);
    assert_eq!(
        triggers,
        Some(valid(&source.replace("    colour: red\n", "")).triggers)
    );

    let missing_jobs = bosun_workflow::triggers(FILE, "on: [push, pull_request]\n");
    assert_eq!(missing_jobs.0.map(|triggers| triggers.len()), Some(2));
    assert_eq!(missing_jobs.1.len(), 1);

    let fine = format!("on: push\n{JOBS}");
    assert_eq!(
        bosun_workflow::triggers(FILE, &fine),
        (Some(valid(&fine).triggers), Vec::new())
    );
}

#[test]
fn no_triggers_come_back_when_on_itself_is_broken() {
    for source in [
        format!("on:\n  push:\n    branch: main\n{JOBS}"),
        format!("on: [push, workflow_call]\n{JOBS}"),
        format!("on: push\non: pull_request\n{JOBS}"),
        format!("on: [push\n{JOBS}"),
        JOBS.to_string(),
    ] {
        let (triggers, diagnostics) = bosun_workflow::triggers(FILE, &source);
        assert_eq!(triggers, None, "{source}");
        assert!(!diagnostics.is_empty(), "{source}");
    }
}

#[test]
fn filters_refuse_what_cannot_be_matched() {
    let cases = [
        (
            "\n  push:\n    tags: [v1]\n    tags-ignore: [v2]",
            "invalid_value",
            "`tags-ignore` of `push` cannot be used together with `tags`; exclude with `!` patterns in `tags` instead",
        ),
        (
            "\n  pull_request:\n    paths-ignore: [docs]\n    paths: [src]",
            "invalid_value",
            "`paths-ignore` of `pull_request` cannot be used together with `paths`; exclude with `!` patterns in `paths` instead",
        ),
        (
            "\n  push:\n    branches: []",
            "invalid_value",
            "`branches` of `push` lists nothing",
        ),
        (
            "\n  issues:\n    types: []",
            "invalid_value",
            "`types` of `issues` lists nothing",
        ),
        (
            "\n  issues:\n    types: ['']",
            "invalid_value",
            "`types` of `issues` names an empty activity type",
        ),
        (
            "\n  push:\n    branches: ['']",
            "invalid_value",
            "`` in `branches` of `push` is not a valid pattern: the pattern is empty",
        ),
        (
            "\n  push:\n    branches: ['!']",
            "invalid_value",
            "`!` in `branches` of `push` is not a valid pattern: `!` is followed by no pattern",
        ),
        (
            "\n  push:\n    branches: ['v[]']",
            "invalid_value",
            "`v[]` in `branches` of `push` is not a valid pattern: `[]` lists no characters",
        ),
        (
            "\n  push:\n    branches: ['v[0-9']",
            "invalid_value",
            "`v[0-9` in `branches` of `push` is not a valid pattern: `[` is not closed by `]`",
        ),
        (
            "\n  push:\n    branches: ['v[9-0]']",
            "invalid_value",
            "`v[9-0]` in `branches` of `push` is not a valid pattern: the range starting at `9` inside `[ ]` must end at a later character of a-z, A-Z or 0-9",
        ),
        (
            "\n  push:\n    branches: ['v[a-Z]']",
            "invalid_value",
            "`v[a-Z]` in `branches` of `push` is not a valid pattern: the range starting at `a` inside `[ ]` must end at a later character of a-z, A-Z or 0-9",
        ),
        (
            "\n  push:\n    paths: ['[_]x']",
            "invalid_value",
            "`[_]x` in `paths` of `push` is not a valid pattern: `_` inside `[ ]` is neither a letter nor a digit",
        ),
        (
            "\n  push:\n    paths: ['docs\\']",
            "invalid_value",
            "`docs\\` in `paths` of `push` is not a valid pattern: `\\` at the end escapes nothing",
        ),
        (
            "\n  push:\n    paths: ['**+']",
            "invalid_value",
            "`**+` in `paths` of `push` is not a valid pattern: `+` must follow a character or a `[ ]` that repeats nothing yet",
        ),
        (
            "\n  push:\n    branch: main",
            "unknown_key",
            "unknown key `branch` in `push`; did you mean `branches`?",
        ),
        (
            "\n  pull_request:\n    tags: [v1]",
            "unknown_key",
            "unknown key `tags` in `pull_request`",
        ),
        (
            "\n  push: main",
            "invalid_type",
            "`push` must be empty or a mapping",
        ),
        (
            "\n  push:\n    branches: { main: true }",
            "invalid_type",
            "`branches` of `push` must be a string or a list of strings",
        ),
    ];
    for (form, code, message) in cases {
        let diagnostic = only(&with_on(form));
        assert_eq!(
            (diagnostic.code.as_str(), diagnostic.message.as_str()),
            (code, message),
            "{form}"
        );
    }
}

#[test]
fn a_cron_lowers_to_the_values_it_selects() {
    let every_hour: Vec<u8> = (0..24).collect();
    let every_day: Vec<u8> = (1..32).collect();
    let every_month: Vec<u8> = (1..13).collect();
    let every_weekday: Vec<u8> = (0..7).collect();
    assert_eq!(
        cron("0 4 * * 1-5"),
        Cron {
            expression: "0 4 * * 1-5".to_string(),
            minutes: vec![0],
            hours: vec![4],
            days_of_month: every_day.clone(),
            months: every_month.clone(),
            days_of_week: vec![1, 2, 3, 4, 5],
            days: CronDays::Both,
        }
    );
    let quarter = cron("*/15 */4 * * *");
    assert_eq!(quarter.minutes, [0, 15, 30, 45]);
    assert_eq!(quarter.hours, [0, 4, 8, 12, 16, 20]);
    assert_eq!(quarter.days_of_week, every_weekday);
    assert_eq!(cron("* * * * *").hours, every_hour);
    assert_eq!(cron("59 23 28-31 * *").days_of_month, [28, 29, 30, 31]);
    assert_eq!(cron("5/20 * * * *").minutes, [5, 25, 45]);
    assert_eq!(cron("0 9-17/4,20 * * *").hours, [9, 13, 17, 20]);
    assert_eq!(cron("0 0 * * 7").days_of_week, [0]);
    assert_eq!(cron("0 0 * * SUN").days_of_week, [0]);
    assert_eq!(cron("0 0 * * fri-7").days_of_week, [0, 5, 6]);
    assert_eq!(cron("0 0 * jan,Dec *").months, [1, 12]);
    assert_eq!(cron("0 0 1 * MON").days, CronDays::Either);
    assert_eq!(cron("0 0 */2 * MON").days, CronDays::Both);
    assert_eq!(cron("0 0 1 * *").days, CronDays::Both);
    assert_eq!(cron("  0   0 1  *\t* ").expression, "  0   0 1  *\t* ");
}

#[test]
fn an_invalid_cron_names_its_fault() {
    let cases = [
        (
            "* * * *",
            "it has 4 fields, and a cron has five: minute, hour, day of the month, month and day of the week",
        ),
        (
            "@daily",
            "shortcuts such as `@daily` are not supported; write the five fields",
        ),
        (
            "*/0 * * * *",
            "the step of `*/0` in the minute is not a positive number",
        ),
        (
            "0 0 10-5 * *",
            "the range `10-5` in the day of the month runs backwards",
        ),
        ("0 24 * * *", "`24` is outside the range of the hour, 0-23"),
        (
            "0 0 0 * *",
            "`0` is outside the range of the day of the month, 1-31",
        ),
        ("0 0 ? * *", "`?` in the day of the month is not a number"),
        (
            "0 0 * * MON-",
            "`` in the day of the week is neither a number nor one of SUN, MON, TUE, WED, THU, FRI, SAT",
        ),
        ("1,,2 * * * *", "the minute holds an empty item"),
        ("0 0 L * *", "`L` in the day of the month is not a number"),
    ];
    for (expression, reason) in cases {
        let diagnostic = only(&with_on(&format!(
            "\n  schedule:\n    - cron: '{expression}'"
        )));
        assert_eq!(diagnostic.code, "invalid_value", "{expression}");
        assert_eq!(
            diagnostic.message,
            format!(
                "`cron` of entry 1 of `schedule` is `{expression}`, which is not a valid cron: {reason}"
            )
        );
    }
}

#[test]
fn a_time_zone_is_an_exact_name_of_the_database() {
    for zone in [
        "UTC",
        "Etc/UTC",
        "Europe/Belgrade",
        "America/Argentina/Buenos_Aires",
    ] {
        let workflow = valid(&with_on(&format!(
            "\n  schedule:\n    - cron: '0 3 * * *'\n      timezone: {zone}"
        )));
        let [Trigger::Schedule(schedules)] = workflow.triggers.as_slice() else {
            panic!("{zone}: {:?}", workflow.triggers);
        };
        assert_eq!(schedules[0].timezone.as_deref(), Some(zone));
    }
    for (zone, message) in [
        (
            "europe/belgrade",
            "`timezone` of entry 1 of `schedule` is `europe/belgrade`, which is not a time zone of the IANA database; did you mean `Europe/Belgrade`?",
        ),
        (
            "Mars/Olympus_Mons",
            "`timezone` of entry 1 of `schedule` is `Mars/Olympus_Mons`, which is not a time zone of the IANA database",
        ),
    ] {
        let diagnostic = only(&with_on(&format!(
            "\n  schedule:\n    - cron: '0 3 * * *'\n      timezone: {zone}"
        )));
        assert_eq!(
            (diagnostic.code.as_str(), diagnostic.message.as_str()),
            ("invalid_value", message)
        );
    }
}

#[test]
fn dispatch_inputs_are_checked_against_their_type() {
    let input = |body: &str| with_on(&format!("\n  workflow_dispatch:\n    inputs:\n{body}"));
    let cases = [
        (
            "      level:\n        options: [a, b]",
            "invalid_value",
            "`options` of input `level` of `workflow_dispatch` apply only to inputs of type `choice`",
        ),
        (
            "      level:\n        type: choice",
            "missing_key",
            "missing key `options` in input `level` of `workflow_dispatch`, whose type is `choice`",
        ),
        (
            "      level:\n        type: choice\n        options: []",
            "invalid_value",
            "`options` of input `level` of `workflow_dispatch` lists no options",
        ),
        (
            "      level:\n        type: choice\n        options: a",
            "invalid_type",
            "`options` of input `level` of `workflow_dispatch` must be a list of strings",
        ),
        (
            "      flag:\n        required: yes",
            "invalid_type",
            "`required` of input `flag` of `workflow_dispatch` must be `true` or `false`",
        ),
        (
            "      flag:\n        type: boolean\n        default: 'true'",
            "invalid_type",
            "`default` of input `flag` of `workflow_dispatch` must be `true` or `false`",
        ),
        (
            "      count:\n        type: number\n        default: .nan",
            "invalid_value",
            "`default` of input `count` of `workflow_dispatch` is not a finite number",
        ),
        (
            "      count:\n        type: number\n        default: '5'",
            "invalid_type",
            "`default` of input `count` of `workflow_dispatch` must be a number",
        ),
        (
            "      flag:\n        type: bool",
            "invalid_value",
            "`type` of input `flag` of `workflow_dispatch` is `bool`, which is not `string`, `boolean`, `choice`, `number` or `environment`",
        ),
        (
            "      flag:\n        type: strng",
            "invalid_value",
            "`type` of input `flag` of `workflow_dispatch` is `strng`, which is not `string`, `boolean`, `choice`, `number` or `environment`; did you mean `string`?",
        ),
        (
            "      flag:\n        descripton: x",
            "unknown_key",
            "unknown key `descripton` in input `flag` of `workflow_dispatch`; did you mean `description`?",
        ),
        (
            "      1st:\n        description: x",
            "invalid_value",
            "the name of input `1st` of `workflow_dispatch` must start with a letter or `_` and hold only letters, digits, `-` and `_`",
        ),
        (
            "      flag: x",
            "invalid_type",
            "input `flag` of `workflow_dispatch` must be a mapping",
        ),
    ];
    for (body, code, message) in cases {
        let diagnostic = only(&input(body));
        assert_eq!(
            (diagnostic.code.as_str(), diagnostic.message.as_str()),
            (code, message),
            "{body}"
        );
    }
    let inputs = only(&with_on("\n  workflow_dispatch:\n    inputs: x"));
    assert_eq!(
        (inputs.code.as_str(), inputs.message.as_str()),
        (
            "invalid_type",
            "`inputs` of `workflow_dispatch` must be a mapping"
        )
    );
}
