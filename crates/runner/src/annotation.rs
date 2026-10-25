use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const PER_STEP: usize = 10;

const PER_JOB: usize = 50;

const LONGEST_TEXT: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnnotationKind {
    Notice,
    Warning,
    Error,
}

impl AnnotationKind {
    pub(crate) fn name(self) -> &'static str {
        match self {
            AnnotationKind::Notice => "notice",
            AnnotationKind::Warning => "warning",
            AnnotationKind::Error => "error",
        }
    }

    fn position(self) -> usize {
        match self {
            AnnotationKind::Notice => 0,
            AnnotationKind::Warning => 1,
            AnnotationKind::Error => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub kind: AnnotationKind,
    pub message: String,
    pub title: Option<String>,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub end_line: Option<u32>,
    pub column: Option<u32>,
    pub end_column: Option<u32>,
}

impl Annotation {
    pub(crate) fn new(kind: AnnotationKind, message: String) -> Annotation {
        Annotation {
            kind,
            message,
            title: None,
            file: None,
            line: None,
            end_line: None,
            column: None,
            end_column: None,
        }
    }

    pub(crate) fn texts(self, mut mask: impl FnMut(&str) -> String) -> Annotation {
        let mut text = |text: &str| shortened(mask(text));
        Annotation {
            message: text(&self.message),
            title: self.title.as_deref().map(&mut text),
            file: self.file.as_deref().map(&mut text),
            ..self
        }
    }
}

fn shortened(mut text: String) -> String {
    if let Some((end, _)) = text.char_indices().nth(LONGEST_TEXT) {
        text.truncate(end);
    }
    text
}

#[derive(Default)]
pub(crate) struct Kept {
    annotations: Vec<Annotation>,
    counts: [usize; 3],
}

impl Kept {
    pub(crate) fn add(&mut self, annotation: Annotation) {
        let count = &mut self.counts[annotation.kind.position()];
        if *count < PER_STEP {
            self.annotations.push(annotation);
        }
        *count += 1;
    }

    pub(crate) fn into_vec(self) -> Vec<Annotation> {
        self.annotations
    }
}

#[derive(Clone, Default)]
pub(crate) struct Allowance(Arc<AtomicUsize>);

impl Allowance {
    pub(crate) fn admits(&self) -> bool {
        self.0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |taken| {
                (taken < PER_JOB).then_some(taken + 1)
            })
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{Allowance, Annotation, AnnotationKind, Kept};

    #[test]
    fn a_step_keeps_ten_annotations_of_each_kind_and_a_job_admits_fifty() {
        let mut kept = Kept::default();
        for kind in [
            AnnotationKind::Warning,
            AnnotationKind::Error,
            AnnotationKind::Notice,
        ] {
            for number in 0..11 {
                kept.add(Annotation::new(kind, number.to_string()));
            }
        }
        let annotations = kept.into_vec();
        assert_eq!(annotations.len(), 30);
        assert!(
            annotations
                .iter()
                .all(|annotation| annotation.message != "10")
        );
        let allowance = Allowance::default();
        assert_eq!((0..60).filter(|_| allowance.admits()).count(), 50);
    }

    #[test]
    fn every_text_of_an_annotation_is_masked_and_cut_at_4096_characters() {
        let mut annotation = Annotation::new(AnnotationKind::Error, "\u{e9}".repeat(5000));
        annotation.title = Some("a title".to_owned());
        annotation.file = Some("src/app.ts".to_owned());
        let masked = annotation.texts(|text| text.replace('a', "*"));
        assert_eq!(masked.message, "\u{e9}".repeat(4096));
        assert_eq!(masked.title.as_deref(), Some("* title"));
        assert_eq!(masked.file.as_deref(), Some("src/*pp.ts"));
    }
}
