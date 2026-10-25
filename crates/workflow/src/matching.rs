use bosun_types::ir::{Filter, PullRequest, Trigger};

use crate::pattern::Patterns;

const PULL_REQUEST_TYPES: &[&str] = &["opened", "synchronize", "reopened"];
const MERGE_GROUP_TYPES: &[&str] = &["checks_requested"];

pub struct Event<'e> {
    pub name: &'e str,
    pub action: Option<&'e str>,
    pub r#ref: Option<&'e str>,
    pub base_ref: Option<&'e str>,
    pub changed_files: Option<&'e [String]>,
}

pub fn matches(triggers: &[Trigger], event: &Event<'_>) -> bool {
    triggers.iter().any(|trigger| fires(trigger, event))
}

fn fires(trigger: &Trigger, event: &Event<'_>) -> bool {
    match trigger {
        Trigger::Push(push) => {
            if event.name != "push" {
                return false;
            }
            let reference = event.r#ref.unwrap_or_default();
            if let Some(tag) = reference.strip_prefix("refs/tags/") {
                return match &push.tags {
                    Some(filter) => names(filter, Some(tag)),
                    None => push.branches.is_none(),
                };
            }
            let refs = match &push.branches {
                Some(filter) => names(filter, reference.strip_prefix("refs/heads/")),
                None => push.tags.is_none(),
            };
            refs && files(push.paths.as_ref(), event.changed_files)
        }
        Trigger::PullRequest(pull_request) => {
            event.name == "pull_request" && change(pull_request, event)
        }
        Trigger::PullRequestTarget(pull_request) => {
            event.name == "pull_request_target" && change(pull_request, event)
        }
        Trigger::MergeGroup(merge_group) => {
            event.name == "merge_group"
                && activity(
                    merge_group.types.as_deref(),
                    Some(MERGE_GROUP_TYPES),
                    event.action,
                )
                && base(merge_group.branches.as_ref(), event.base_ref)
        }
        Trigger::Schedule(_) => event.name == "schedule",
        Trigger::WorkflowDispatch(_) => event.name == "workflow_dispatch",
        Trigger::RepositoryDispatch(dispatch) => {
            event.name == "repository_dispatch"
                && activity(dispatch.types.as_deref(), None, event.action)
        }
        Trigger::Generic(generic) => {
            event.name == generic.name && activity(generic.types.as_deref(), None, event.action)
        }
    }
}

fn change(pull_request: &PullRequest, event: &Event<'_>) -> bool {
    activity(
        pull_request.types.as_deref(),
        Some(PULL_REQUEST_TYPES),
        event.action,
    ) && base(pull_request.branches.as_ref(), event.base_ref)
        && files(pull_request.paths.as_ref(), event.changed_files)
}

fn activity(types: Option<&[String]>, defaults: Option<&[&str]>, action: Option<&str>) -> bool {
    match (types, defaults, action) {
        (Some(types), _, Some(action)) => types.iter().any(|kind| kind == action),
        (None, Some(defaults), Some(action)) => defaults.contains(&action),
        (None, None, _) => true,
        (_, _, None) => false,
    }
}

fn base(filter: Option<&Filter>, base_ref: Option<&str>) -> bool {
    filter.is_none_or(|filter| {
        names(
            filter,
            base_ref.and_then(|base| base.strip_prefix("refs/heads/")),
        )
    })
}

fn names(filter: &Filter, name: Option<&str>) -> bool {
    let Some(name) = name else {
        return false;
    };
    match filter {
        Filter::Include(patterns) => Patterns::new(patterns).select(name),
        Filter::Exclude(patterns) => !Patterns::new(patterns).select(name),
    }
}

fn files(filter: Option<&Filter>, changed: Option<&[String]>) -> bool {
    let (Some(filter), Some(changed)) = (filter, changed) else {
        return true;
    };
    match filter {
        Filter::Include(patterns) => {
            let patterns = Patterns::new(patterns);
            changed.iter().any(|file| patterns.select(file))
        }
        Filter::Exclude(patterns) => {
            let patterns = Patterns::new(patterns);
            changed.iter().any(|file| !patterns.select(file))
        }
    }
}
