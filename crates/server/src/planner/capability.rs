use bosun_db::pools::Placement;
use bosun_types::api::Capability;
use bosun_types::ir::JobTemplate;

pub(crate) fn needed_capability(job: &JobTemplate, aliased: &[Capability]) -> Capability {
    if job.container.is_some() || !job.services.is_empty() || aliased.contains(&Capability::Podman)
    {
        Capability::Podman
    } else {
        Capability::Shell
    }
}

pub(crate) fn require(placement: &mut Placement, needed: Capability) {
    let label = match needed {
        Capability::Shell => "shell",
        Capability::Podman => "podman",
    };
    if !placement.labels.iter().any(|held| held == label) {
        placement.labels.push(label.to_owned());
    }
    if !placement.capabilities.contains(&needed) {
        placement.capabilities.push(needed);
    }
}
