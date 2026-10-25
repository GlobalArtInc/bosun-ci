use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bosun_types::diagnostic::{Diagnostic, Severity};
use bosun_types::ir::{self, Trigger};

const ENTRIES: usize = 256;

pub(crate) struct Lowered {
    pub(crate) triggers: Option<Vec<Trigger>>,
    pub(crate) workflow: Option<ir::Workflow>,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl Lowered {
    pub(crate) fn new(file: &str, source: &str) -> Lowered {
        let (triggers, _) = bosun_workflow::triggers(file, source);
        Lowered {
            triggers,
            workflow: bosun_workflow::parse(file, source)
                .ok()
                .map(|parsed| parsed.workflow),
            diagnostics: bosun_workflow::lint(file, source),
        }
    }

    pub(crate) fn warnings(&self) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Counted {
    pub(crate) parsed: usize,
    pub(crate) reused: usize,
}

#[derive(Default)]
pub(crate) struct Parses {
    entries: Mutex<Entries>,
}

#[derive(Default)]
struct Entries {
    lowered: HashMap<(String, String), Arc<Lowered>>,
    order: VecDeque<(String, String)>,
}

impl Parses {
    pub(crate) fn lowered(
        &self,
        path: &str,
        blob: &str,
        source: &str,
        counted: &mut Counted,
    ) -> Arc<Lowered> {
        let key = (path.to_owned(), blob.to_owned());
        if let Some(lowered) = self.lock().lowered.get(&key) {
            counted.reused += 1;
            return Arc::clone(lowered);
        }
        let lowered = Arc::new(Lowered::new(path, source));
        counted.parsed += 1;
        let mut entries = self.lock();
        if entries
            .lowered
            .insert(key.clone(), Arc::clone(&lowered))
            .is_none()
        {
            entries.order.push_back(key);
        }
        while entries.order.len() > ENTRIES {
            if let Some(oldest) = entries.order.pop_front() {
                entries.lowered.remove(&oldest);
            }
        }
        lowered
    }

    fn lock(&self) -> MutexGuard<'_, Entries> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::{Counted, Parses};

    const SOURCE: &str =
        "on: push\njobs:\n  build:\n    runs-on: shell\n    steps:\n      - run: make\n";

    #[test]
    fn a_file_is_parsed_once_per_path_and_blob() {
        let parses = Parses::default();
        let mut counted = Counted::default();
        let first = parses.lowered(".bosun/workflows/ci.yml", "a1", SOURCE, &mut counted);
        let again = parses.lowered(".bosun/workflows/ci.yml", "a1", SOURCE, &mut counted);
        assert!(std::sync::Arc::ptr_eq(&first, &again));
        parses.lowered(".github/workflows/ci.yml", "a1", SOURCE, &mut counted);
        parses.lowered(".bosun/workflows/ci.yml", "b2", SOURCE, &mut counted);
        assert_eq!(
            counted,
            Counted {
                parsed: 3,
                reused: 1
            }
        );
        assert!(first.workflow.is_some());
        assert!(first.triggers.is_some());
        assert!(first.diagnostics.is_empty());
    }

    #[test]
    fn the_oldest_files_leave_a_full_cache_first() {
        let parses = Parses::default();
        let mut counted = Counted::default();
        for blob in 0..=super::ENTRIES {
            parses.lowered("ci.yml", &blob.to_string(), SOURCE, &mut counted);
        }
        parses.lowered("ci.yml", &super::ENTRIES.to_string(), SOURCE, &mut counted);
        parses.lowered("ci.yml", "0", SOURCE, &mut counted);
        assert_eq!(
            counted,
            Counted {
                parsed: super::ENTRIES + 2,
                reused: 1
            }
        );
    }
}
