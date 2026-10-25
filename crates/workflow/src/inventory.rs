use bosun_types::diagnostic::{Diagnostic, PathSegment, Severity};

use crate::lower::{Lowering, Seen};
use crate::surface::{Arrives, Milestone};
use crate::yaml::{self, Node, Value};

const DOCKER: &str = "docker://";

#[derive(Debug)]
pub struct Inventory {
    pub uses: Vec<Uses>,
    pub keys: Vec<KeyUse>,
}

#[derive(Debug)]
pub struct Uses {
    pub name: String,
    pub version: Option<String>,
}

#[derive(Debug)]
pub struct KeyUse {
    pub key: String,
    pub from: Option<Milestone>,
    pub extension: bool,
}

pub fn inventory(file: &str, source: &str) -> Result<Inventory, Diagnostic> {
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let root = yaml::load(source).map_err(|fault| Diagnostic {
        severity: Severity::Error,
        code: fault.code.as_str().to_string(),
        message: fault.message,
        file: file.to_string(),
        span: fault.span,
        path: Vec::new(),
    })?;
    let mut lowering = Lowering::new(file, source);
    lowering.workflow(&root);
    Ok(Inventory {
        uses: uses(&root),
        keys: lowering
            .seen
            .iter()
            .map(|entry| {
                let (from, extension) = arrival(&lowering.seen, entry);
                KeyUse {
                    key: generalized(&entry.path),
                    from,
                    extension,
                }
            })
            .collect(),
    })
}

fn arrival(seen: &[Seen], entry: &Seen) -> (Option<Milestone>, bool) {
    match entry.arrives {
        Arrives::Listed { from, extension } => (Some(from), extension),
        Arrives::Unknown => (None, false),
        Arrives::Inherited => {
            let parent = &entry.path[..entry.path.len().saturating_sub(1)];
            seen.iter()
                .find(|candidate| candidate.path == parent)
                .map_or((None, false), |found| arrival(seen, found))
        }
    }
}

fn generalized(path: &[PathSegment]) -> String {
    let names: Vec<&str> = path
        .iter()
        .filter_map(|segment| match segment {
            PathSegment::Key(name) => Some(name.as_str()),
            PathSegment::Index(_) => None,
        })
        .collect();
    names
        .iter()
        .enumerate()
        .map(|(position, name)| match names[..position] {
            ["jobs"] => "<job_id>",
            ["on", "workflow_dispatch", "inputs"] => "<input_id>",
            _ => name,
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn field<'n>(node: &'n Node, name: &str) -> Option<&'n Node> {
    let Value::Mapping(pairs) = &node.value else {
        return None;
    };
    pairs
        .iter()
        .find(|(key, _)| matches!(&key.value, Value::String(key) if key == name))
        .map(|(_, value)| value)
}

fn reference(node: &Node, found: &mut Vec<Uses>) {
    if let Some(Node {
        value: Value::String(text),
        ..
    }) = field(node, "uses")
    {
        found.push(split(text));
    }
}

fn uses(root: &Node) -> Vec<Uses> {
    let mut found = Vec::new();
    let Some(Node {
        value: Value::Mapping(jobs),
        ..
    }) = field(root, "jobs")
    else {
        return found;
    };
    for (_, job) in jobs {
        reference(job, &mut found);
        if let Some(Node {
            value: Value::Sequence(steps),
            ..
        }) = field(job, "steps")
        {
            for step in steps {
                reference(step, &mut found);
            }
        }
    }
    found
}

fn split(text: &str) -> Uses {
    let at = if let Some(image) = text.strip_prefix(DOCKER) {
        image
            .rfind('@')
            .or_else(|| {
                image
                    .rfind(':')
                    .filter(|&colon| !image[colon..].contains('/'))
            })
            .map(|at| at + DOCKER.len())
    } else if text.starts_with("./") {
        None
    } else {
        text.find('@')
    };
    match at {
        Some(at) => Uses {
            name: text[..at].to_string(),
            version: Some(text[at + 1..].to_string()),
        },
        None => Uses {
            name: text.to_string(),
            version: None,
        },
    }
}
