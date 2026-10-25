use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::Arc;
use std::time::SystemTime;

use aho_corasick::AhoCorasick;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use bosun_types::status::Conclusion;
use tokio::sync::mpsc::UnboundedSender;

use crate::engine::Event;
use crate::output::LONGEST_LINE;

const MASK: &[u8] = b"***";

const SHORTEST_FORM: usize = 4;

enum Automaton {
    Empty,
    Built(AhoCorasick),
    Unbuilt,
}

pub(crate) struct Masker {
    automaton: Automaton,
    longest: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Cut {
    pub(crate) at: usize,
    pub(crate) masked: usize,
    pub(crate) carried: usize,
}

impl Masker {
    pub(crate) fn new<'a>(secrets: impl IntoIterator<Item = &'a str>) -> Masker {
        let mut patterns = BTreeSet::new();
        for secret in secrets {
            for value in values(secret) {
                patterns.insert(value.as_bytes().to_vec());
                patterns.extend(
                    forms(value)
                        .into_iter()
                        .filter(|form| form.len() >= SHORTEST_FORM),
                );
            }
        }
        let longest = patterns.iter().map(Vec::len).max().unwrap_or(0);
        let automaton = if patterns.is_empty() {
            Automaton::Empty
        } else {
            AhoCorasick::new(&patterns).map_or(Automaton::Unbuilt, Automaton::Built)
        };
        Masker { automaton, longest }
    }

    pub(crate) fn longest(&self) -> usize {
        self.longest
    }

    pub(crate) fn masks(&self, text: &str) -> bool {
        !self.ranges(text.as_bytes(), 0).is_empty()
    }

    pub(crate) fn mask(&self, text: &[u8], masked: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len());
        let mut at = 0;
        for range in self.ranges(text, masked) {
            out.extend_from_slice(&text[at..range.start]);
            out.extend_from_slice(MASK);
            at = range.end;
        }
        out.extend_from_slice(&text[at..]);
        out
    }

    pub(crate) fn cut(&self, text: &[u8], at: usize, masked: usize) -> Cut {
        match self
            .ranges(text, masked)
            .into_iter()
            .find(|range| range.start < at && at < range.end)
        {
            None => Cut {
                at,
                masked,
                carried: 0,
            },
            Some(range) if range.start > 0 => Cut {
                at: range.start,
                masked,
                carried: 0,
            },
            Some(range) => Cut {
                at,
                masked: at,
                carried: range.end - at,
            },
        }
    }

    fn ranges(&self, text: &[u8], masked: usize) -> Vec<Range<usize>> {
        let mut found: Vec<Range<usize>> = match &self.automaton {
            Automaton::Empty => Vec::new(),
            Automaton::Built(automaton) => automaton
                .find_overlapping_iter(text)
                .map(|hit| hit.range())
                .collect(),
            Automaton::Unbuilt => std::iter::once(0..text.len()).collect(),
        };
        found.push(0..masked.min(text.len()));
        found.retain(|range| !range.is_empty());
        found.sort_by_key(|range| range.start);
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(found.len());
        for range in found {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        merged
    }
}

fn values(secret: &str) -> impl Iterator<Item = &str> {
    std::iter::once(secret)
        .chain(secret.split(['\r', '\n']))
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn forms(value: &str) -> Vec<Vec<u8>> {
    let mut forms = Vec::new();
    for leading in 0..3 {
        forms.extend(base64(value.as_bytes(), leading));
    }
    forms.push(json(value));
    forms.push(url(value));
    forms.push(xml(value));
    forms.push(value.replace('\'', "''").into_bytes());
    forms.push(value.replace('"', "\\\"").into_bytes());
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        forms.push(inner.as_bytes().to_vec());
    }
    if let Some((before, after)) = value.split_once('&') {
        forms.push(before.as_bytes().to_vec());
        forms.push(after.as_bytes().to_vec());
    }
    for section in powershell(value).into_iter().flatten() {
        forms.push(section.as_bytes().to_vec());
    }
    forms
}

fn powershell(value: &str) -> Option<[&str; 2]> {
    if let Some(at) = value.find("&+") {
        let (before, rest) = value.split_at(at + 2);
        let coloured = rest.chars().next().map_or(0, char::len_utf8);
        return Some([before, &rest[coloured..]]);
    }
    let at = value.rfind('&')?;
    Some([&value[..=at], &value[at + 1..]])
}

fn base64(bytes: &[u8], leading: usize) -> [Vec<u8>; 2] {
    let mut data = vec![0; leading];
    data.extend_from_slice(bytes);
    let skip = (leading * 8).div_ceil(6);
    let end = data.len() * 8 / 6;
    let open = STANDARD_NO_PAD.encode(&data);
    let closed = STANDARD.encode(&data);
    [open.get(skip..end), closed.get(skip..)]
        .map(|form| form.unwrap_or_default().as_bytes().to_vec())
}

fn json(value: &str) -> Vec<u8> {
    let quoted = bosun_expr::to_json(&bosun_expr::Value::String(value.to_owned()));
    quoted
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or_default()
        .as_bytes()
        .to_vec()
}

fn url(value: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte);
        } else {
            out.extend_from_slice(format!("%{byte:02X}").as_bytes());
        }
    }
    out
}

fn xml(value: &str) -> Vec<u8> {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '&' => out.push_str("&amp;"),
            c => out.push(c),
        }
    }
    out.into_bytes()
}

#[derive(Clone)]
pub(crate) struct Sink {
    events: UnboundedSender<Event>,
    masker: Arc<Masker>,
}

impl Sink {
    pub(crate) fn new(events: UnboundedSender<Event>, masker: Masker) -> Sink {
        Sink {
            events,
            masker: Arc::new(masker),
        }
    }

    pub(crate) fn masker(&self) -> &Masker {
        &self.masker
    }

    pub(crate) fn started(&self, index: u32) {
        let _ = self.events.send(Event::StepStarted {
            index,
            at: SystemTime::now(),
        });
    }

    pub(crate) fn line(&self, index: u32, text: &str) {
        self.output(index, text.as_bytes(), 0);
    }

    pub(crate) fn output(&self, index: u32, text: &[u8], masked: usize) {
        let text = self.masker.mask(text, masked);
        if text.is_empty() {
            self.send_line(index, Vec::new());
        }
        for piece in text.chunks(LONGEST_LINE) {
            self.send_line(index, piece.to_vec());
        }
    }

    pub(crate) fn finished(
        &self,
        index: u32,
        outcome: Conclusion,
        conclusion: Conclusion,
        outputs: &BTreeMap<String, String>,
    ) {
        let _ = self.events.send(Event::StepFinished {
            index,
            at: SystemTime::now(),
            outcome,
            conclusion,
            outputs: outputs
                .iter()
                .map(|(name, value)| (self.text(name), self.text(value)))
                .collect(),
        });
    }

    fn text(&self, text: &str) -> String {
        String::from_utf8_lossy(&self.masker.mask(text.as_bytes(), 0)).into_owned()
    }

    fn send_line(&self, index: u32, text: Vec<u8>) {
        let _ = self.events.send(Event::Line {
            index,
            at: SystemTime::now(),
            text,
        });
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    use super::{Cut, Masker};

    fn masked(secrets: &[&str], text: &str) -> String {
        let masker = Masker::new(secrets.iter().copied());
        String::from_utf8(masker.mask(text.as_bytes(), 0)).expect("UTF-8")
    }

    #[test]
    fn overlapping_and_adjacent_occurrences_become_one_mask() {
        assert_eq!(masked(&["aba"], "x ababa y"), "x *** y");
        assert_eq!(masked(&["ab"], "abab ab"), "*** ***");
        assert_eq!(masked(&["abc", "cde"], "abcde"), "***");
    }

    #[test]
    fn a_value_is_masked_whatever_its_length_and_a_short_derived_form_is_not() {
        assert_eq!(masked(&["a"], "banana"), "b***n***n***");
        assert_eq!(
            masked(&["'"], "it's 'quoted' ''"),
            "it***s ***quoted*** ***"
        );
        assert_eq!(masked(&["\"ab\""], "ab \"ab\""), "ab ***");
    }

    #[test]
    fn the_powershell_sections_skip_the_character_coloured_after_an_ampersand_plus() {
        assert_eq!(masked(&["abcd&+"], "abcd&+ abcd"), "*** ***");
        assert_eq!(
            masked(&["abcd&+\u{e9}fghi"], "abcd&+ fghi \u{e9}fghi"),
            "*** *** \u{e9}***"
        );
    }

    #[test]
    fn white_space_around_a_value_and_its_lines_is_not_part_of_the_mask() {
        assert_eq!(
            masked(
                &["  first \r\n\n second  \n"],
                "[first] [second] [first \r\n\n second]"
            ),
            "[***] [***] [***]"
        );
        assert_eq!(masked(&[" \n\t "], " \n\t "), " \n\t ");
    }

    #[test]
    fn base64_is_masked_at_every_alignment_followed_by_more_data_or_at_its_end() {
        let secret = "s3cr3t-value";
        for (prefix, suffix) in [
            ("", ""),
            ("x", ""),
            ("xy", ""),
            ("", "\n"),
            ("x", "!\n"),
            ("xy", "?"),
        ] {
            let encoded = STANDARD.encode(format!("{prefix}{secret}{suffix}"));
            let masked = masked(&[secret], &encoded);
            assert!(
                masked.contains("***"),
                "{prefix:?} {suffix:?}: {encoded} gives {masked}"
            );
            let visible = masked.replace("***", "");
            assert!(
                visible.len() <= 2 + 4,
                "{prefix:?} {suffix:?}: {encoded} gives {masked}, which keeps too much of it"
            );
        }
    }

    #[test]
    fn a_cut_never_falls_inside_a_mask() {
        let masker = Masker::new(["secret"]);
        assert_eq!(
            masker.cut(b"0123secret4567", 6, 0),
            Cut {
                at: 4,
                masked: 0,
                carried: 0
            }
        );
        assert_eq!(
            masker.cut(b"secret4567", 3, 0),
            Cut {
                at: 3,
                masked: 3,
                carried: 3
            }
        );
        assert_eq!(
            masker.cut(b"0123secret4567", 2, 0),
            Cut {
                at: 2,
                masked: 0,
                carried: 0
            }
        );
        assert_eq!(
            masker.cut(b"et4567", 4, 2),
            Cut {
                at: 4,
                masked: 2,
                carried: 0
            }
        );
    }
}
