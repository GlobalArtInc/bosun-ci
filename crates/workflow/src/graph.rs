use std::collections::BTreeMap;

use bosun_types::diagnostic::PathSegment;
use bosun_types::ir::Span;

use crate::Code;
use crate::lower::Lowering;
use crate::surface::suggest;

#[derive(Clone)]
pub(crate) struct Need {
    pub(crate) name: String,
    pub(crate) span: Span,
    pub(crate) path: Vec<PathSegment>,
}

pub(crate) struct GraphJob<'n> {
    pub(crate) name: &'n str,
    pub(crate) needs: Vec<Need>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    New,
    Open,
    Done,
}

impl Lowering<'_> {
    pub(crate) fn graph(&mut self, jobs: &[GraphJob<'_>]) {
        let positions: BTreeMap<&str, usize> = jobs
            .iter()
            .enumerate()
            .map(|(position, job)| (job.name, position))
            .collect();
        let mut edges = Vec::with_capacity(jobs.len());
        for job in jobs {
            let mut targets = Vec::new();
            for need in &job.needs {
                match positions.get(need.name.as_str()) {
                    Some(&target) => targets.push((target, need)),
                    None => {
                        let hint = suggest(&need.name, jobs.iter().map(|job| job.name))
                            .map(|name| format!("; did you mean `{name}`?"))
                            .unwrap_or_default();
                        self.error(
                            Code::UnknownJob,
                            need.span,
                            &need.path,
                            format!(
                                "job `{}` needs `{}`, which does not exist{hint}",
                                job.name, need.name
                            ),
                        );
                    }
                }
            }
            edges.push(targets);
        }

        let mut visits = vec![Visit::New; jobs.len()];
        for start in 0..jobs.len() {
            if visits[start] != Visit::New {
                continue;
            }
            visits[start] = Visit::Open;
            let mut stack = vec![(start, 0)];
            while let Some(&(job, next)) = stack.last() {
                let Some(&(target, need)) = edges[job].get(next) else {
                    visits[job] = Visit::Done;
                    stack.pop();
                    continue;
                };
                if let Some(top) = stack.last_mut() {
                    top.1 = next + 1;
                }
                match visits[target] {
                    Visit::New => {
                        visits[target] = Visit::Open;
                        stack.push((target, 0));
                    }
                    Visit::Open => {
                        let from = stack
                            .iter()
                            .position(|&(job, _)| job == target)
                            .unwrap_or(0);
                        let cycle: Vec<String> = stack[from..]
                            .iter()
                            .map(|&(job, _)| jobs[job].name)
                            .chain([jobs[target].name])
                            .map(|name| format!("`{name}`"))
                            .collect();
                        self.error(
                            Code::NeedsCycle,
                            need.span,
                            &need.path,
                            format!("`needs` forms a cycle: {}", cycle.join(" -> ")),
                        );
                    }
                    Visit::Done => {}
                }
            }
        }
    }
}
