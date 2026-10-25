use std::borrow::Cow;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use fancy_regex::{Captures, Regex};
use serde::Deserialize;

use crate::annotation::{Annotation, AnnotationKind};

const GITHUB: &str = "https://github.com";

#[derive(Deserialize)]
struct Document {
    #[serde(rename = "problemMatcher", default)]
    problem_matcher: Vec<WrittenMatcher>,
}

#[derive(Deserialize)]
struct WrittenMatcher {
    #[serde(default)]
    owner: String,
    #[serde(default)]
    severity: Option<String>,
    #[serde(default)]
    pattern: Vec<WrittenPattern>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WrittenPattern {
    #[serde(default)]
    regexp: String,
    file: Option<i64>,
    from_path: Option<i64>,
    line: Option<i64>,
    column: Option<i64>,
    severity: Option<i64>,
    code: Option<i64>,
    message: Option<i64>,
    #[serde(default, rename = "loop")]
    repeats: bool,
}

impl WrittenPattern {
    fn groups(&self) -> [(Property, Option<i64>); PROPERTIES] {
        [
            (Property::File, self.file),
            (Property::FromPath, self.from_path),
            (Property::Line, self.line),
            (Property::Column, self.column),
            (Property::Severity, self.severity),
            (Property::Code, self.code),
            (Property::Message, self.message),
        ]
    }
}

const PROPERTIES: usize = 7;

#[derive(Clone, Copy)]
enum Property {
    File,
    FromPath,
    Line,
    Column,
    Severity,
    Code,
    Message,
}

impl Property {
    fn name(self) -> &'static str {
        match self {
            Property::File => "file",
            Property::FromPath => "fromPath",
            Property::Line => "line",
            Property::Column => "column",
            Property::Severity => "severity",
            Property::Code => "code",
            Property::Message => "message",
        }
    }
}

struct Pattern {
    regex: Regex,
    groups: [Option<usize>; PROPERTIES],
    repeats: bool,
}

pub(crate) struct Matcher {
    owner: String,
    severity: Option<String>,
    patterns: Vec<Pattern>,
}

pub(crate) fn load(file: &str, place: &Place) -> Result<Vec<Matcher>, String> {
    let text = fs::read_to_string(place.rooted(file))
        .map_err(|error| format!("`{file}` could not be read: {error}"))?;
    let document: Document = serde_json::from_str(&text)
        .map_err(|error| format!("`{file}` is not a document of problem matchers: {error}"))?;
    let mut matchers: Vec<Matcher> = Vec::new();
    for written in document.problem_matcher {
        let matcher = validated(written)?;
        if matchers
            .iter()
            .any(|known| same(&known.owner, &matcher.owner))
        {
            return Err(format!(
                "the owner `{}` names more than one problem matcher",
                matcher.owner
            ));
        }
        matchers.push(matcher);
    }
    Ok(matchers)
}

fn validated(written: WrittenMatcher) -> Result<Matcher, String> {
    let WrittenMatcher {
        owner,
        severity,
        pattern,
    } = written;
    if owner.is_empty() {
        return Err("a problem matcher has no `owner`".to_owned());
    }
    if let Some(severity) = severity.as_deref().filter(|severity| !severity.is_empty())
        && kind(severity).is_none()
    {
        return Err(format!(
            "the problem matcher `{owner}` has the severity `{severity}`, which is none of \
             `error`, `warning` and `notice`"
        ));
    }
    if pattern.is_empty() {
        return Err(format!("the problem matcher `{owner}` has no pattern"));
    }
    let count = pattern.len();
    let mut set = [false; PROPERTIES];
    let mut patterns = Vec::with_capacity(count);
    for (position, written) in pattern.into_iter().enumerate() {
        if written.repeats && (position == 0 || position + 1 < count) {
            return Err(format!(
                "only the last of several patterns of the problem matcher `{owner}` may set `loop`"
            ));
        }
        if written.repeats && written.message.is_none() {
            return Err(format!(
                "the pattern of the problem matcher `{owner}` that sets `loop` must set `message`"
            ));
        }
        let regex = Regex::new(&written.regexp).map_err(|error| {
            format!(
                "the pattern `{}` of the problem matcher `{owner}` is not a regular expression: \
                 {error}",
                written.regexp
            )
        })?;
        let mut groups = [None; PROPERTIES];
        for (property, group) in written.groups() {
            let Some(group) = group else {
                continue;
            };
            let (position, name) = (property as usize, property.name());
            if set[position] {
                return Err(format!(
                    "the problem matcher `{owner}` sets `{name}` in more than one pattern"
                ));
            }
            set[position] = true;
            groups[position] = Some(
                usize::try_from(group)
                    .ok()
                    .filter(|group| *group < regex.captures_len())
                    .ok_or_else(|| {
                        format!(
                            "the problem matcher `{owner}` sets `{name}` to the group {group}, \
                             which its pattern `{}` does not have",
                            written.regexp
                        )
                    })?,
            );
        }
        patterns.push(Pattern {
            regex,
            groups,
            repeats: written.repeats,
        });
    }
    if !set[Property::Message as usize] {
        return Err(format!(
            "no pattern of the problem matcher `{owner}` sets `message`"
        ));
    }
    Ok(Matcher {
        owner,
        severity,
        patterns,
    })
}

fn same(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

fn kind(severity: &str) -> Option<AnnotationKind> {
    match severity.to_lowercase().as_str() {
        "error" => Some(AnnotationKind::Error),
        "warning" => Some(AnnotationKind::Warning),
        "notice" => Some(AnnotationKind::Notice),
        _ => None,
    }
}

pub(crate) fn owners(matchers: &[Matcher]) -> Vec<String> {
    matchers
        .iter()
        .map(|matcher| matcher.owner.clone())
        .collect()
}

#[derive(Clone, Default)]
struct Captured([Option<String>; PROPERTIES]);

impl Captured {
    fn get(&self, property: Property) -> Option<&str> {
        self.0[property as usize].as_deref()
    }
}

fn captured(running: Option<&Captured>, pattern: &Pattern, captures: &Captures<str>) -> Captured {
    let mut found = Captured::default();
    for (property, group) in pattern.groups.iter().enumerate() {
        found.0[property] = running
            .and_then(|running| running.0[property].clone())
            .or_else(|| {
                group.map(|group| {
                    captures
                        .get(group)
                        .map_or("", |text| text.as_str())
                        .to_owned()
                })
            });
    }
    found
}

#[derive(Clone, Default)]
pub(crate) struct Registry(Arc<Mutex<Registered>>);

#[derive(Default)]
struct Registered {
    version: u64,
    matchers: Vec<Arc<Matcher>>,
}

impl Registry {
    fn lock(&self) -> MutexGuard<'_, Registered> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn add(&self, added: Vec<Matcher>) {
        let mut registered = self.lock();
        let mut matchers: Vec<Arc<Matcher>> = added.into_iter().map(Arc::new).collect();
        let kept: Vec<Arc<Matcher>> = registered
            .matchers
            .iter()
            .filter(|known| !matchers.iter().any(|new| same(&new.owner, &known.owner)))
            .cloned()
            .collect();
        matchers.extend(kept);
        registered.matchers = matchers;
        registered.version += 1;
    }

    pub(crate) fn remove(&self, owners: &[String]) -> Vec<String> {
        let mut registered = self.lock();
        let (removed, kept): (Vec<Arc<Matcher>>, Vec<Arc<Matcher>>) = registered
            .matchers
            .drain(..)
            .partition(|known| owners.iter().any(|owner| same(owner, &known.owner)));
        registered.matchers = kept;
        registered.version += 1;
        removed
            .iter()
            .map(|matcher| matcher.owner.clone())
            .collect()
    }

    fn discard(&self, matcher: &Arc<Matcher>) {
        let mut registered = self.lock();
        registered
            .matchers
            .retain(|known| !Arc::ptr_eq(known, matcher));
        registered.version += 1;
    }

    fn changed(&self, known: Option<u64>) -> Option<(u64, Vec<Arc<Matcher>>)> {
        let registered = self.lock();
        (known != Some(registered.version))
            .then(|| (registered.version, registered.matchers.clone()))
    }
}

struct Running {
    matcher: Arc<Matcher>,
    state: Vec<Option<Captured>>,
}

impl Running {
    fn new(matcher: Arc<Matcher>) -> Running {
        let state = vec![None; matcher.patterns.len().saturating_sub(1)];
        Running { matcher, state }
    }

    fn reset(&mut self) {
        self.state.fill(None);
    }

    fn step(&mut self, line: &str) -> Result<Option<Captured>, fancy_regex::Error> {
        let matcher = Arc::clone(&self.matcher);
        let patterns = &matcher.patterns;
        if let [pattern] = patterns.as_slice() {
            return Ok(pattern
                .regex
                .captures(line)?
                .map(|captures| matcher.finished(captured(None, pattern, &captures))));
        }
        for position in (0..patterns.len()).rev() {
            let running = position
                .checked_sub(1)
                .and_then(|before| self.state[before].clone());
            if position > 0 && running.is_none() {
                continue;
            }
            let pattern = &patterns[position];
            let last = position + 1 == patterns.len();
            match pattern.regex.captures(line)? {
                Some(captures) if last => {
                    let found = captured(running.as_ref(), pattern, &captures);
                    self.reset();
                    if pattern.repeats {
                        self.state[position - 1] = running;
                    }
                    return Ok(Some(matcher.finished(found)));
                }
                Some(captures) => {
                    self.state[position] = Some(captured(running.as_ref(), pattern, &captures));
                }
                None if last => self.state[position - 1] = None,
                None => self.state[position] = None,
            }
        }
        Ok(None)
    }
}

impl Matcher {
    fn finished(&self, mut found: Captured) -> Captured {
        let severity = &mut found.0[Property::Severity as usize];
        if severity.as_deref().is_none_or(str::is_empty)
            && let Some(default) = self.severity.as_ref().filter(|default| !default.is_empty())
        {
            *severity = Some(default.clone());
        }
        found
    }
}

#[derive(Default)]
pub(crate) struct Place {
    pub(crate) workspace: PathBuf,
    pub(crate) repository: String,
    pub(crate) server_url: String,
}

impl Place {
    pub(crate) fn relative(&self, file: &str) -> String {
        match Path::new(file).strip_prefix(&self.workspace) {
            Ok(rest) if !self.workspace.as_os_str().is_empty() && !rest.as_os_str().is_empty() => {
                rest.to_string_lossy().into_owned()
            }
            _ => file.to_owned(),
        }
    }

    fn rooted(&self, file: &str) -> PathBuf {
        let path = Path::new(file);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.workspace.join(path)
        }
    }

    fn file(&self, file: &str, from: Option<&str>) -> Option<String> {
        let mut path = PathBuf::from(file);
        if !path.is_absolute()
            && let Some(directory) = from
                .filter(|from| !from.trim().is_empty())
                .and_then(|from| Path::new(from).parent())
                .filter(|directory| !directory.as_os_str().is_empty())
        {
            path = directory.join(path);
        }
        if !path.is_absolute() {
            path = self.workspace.join(path);
        }
        if !path.is_absolute() {
            return None;
        }
        let path = normal(&path);
        if !path.is_file() {
            return None;
        }
        let root = self.repository_of(&path)?;
        let relative = path.strip_prefix(&root).ok()?;
        Some(relative.to_string_lossy().into_owned())
    }

    fn repository_of(&self, file: &Path) -> Option<PathBuf> {
        let mut directory = file.parent()?;
        loop {
            let config = directory.join(".git").join("config");
            if config.is_file() {
                return self
                    .names_the_repository(&config)
                    .then(|| directory.to_path_buf());
            }
            directory = directory.parent()?;
        }
    }

    fn names_the_repository(&self, config: &Path) -> bool {
        let Ok(text) = fs::read_to_string(config) else {
            return false;
        };
        let server = if self.server_url.is_empty() {
            GITHUB
        } else {
            &self.server_url
        };
        let remotes = [
            format!("url = {server}/{}", self.repository),
            format!("url = git@{}:{}.git", host(server), self.repository),
        ]
        .map(|remote| remote.to_lowercase());
        text.split('\n')
            .map(|line| line.trim().to_lowercase())
            .any(|line| remotes.contains(&line))
    }
}

fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    authority.split(':').next().unwrap_or(authority)
}

fn normal(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            other => normal.push(other),
        }
    }
    normal
}

fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<i32>()
        .ok()
        .and_then(|number| u32::try_from(number).ok())
}

fn annotation(found: &Captured, place: &Place) -> Option<Annotation> {
    let message = found
        .get(Property::Message)
        .filter(|message| !message.trim().is_empty())?;
    let kind = match found.get(Property::Severity).unwrap_or_default() {
        "" => AnnotationKind::Error,
        severity => kind(severity)?,
    };
    let mut annotation = Annotation::new(kind, message.to_owned());
    annotation.line = found.get(Property::Line).and_then(number);
    annotation.column = found.get(Property::Column).and_then(number);
    annotation.file = found
        .get(Property::File)
        .filter(|file| !file.trim().is_empty())
        .and_then(|file| place.file(file, found.get(Property::FromPath)));
    Some(annotation)
}

pub(crate) fn without_colours(line: &str) -> Cow<'_, str> {
    const PREFIX: &str = "\x1b[";
    if !line.contains(PREFIX) {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(PREFIX) {
        out.push_str(&rest[..at]);
        let after = &rest[at + PREFIX.len()..];
        let end = after
            .find(|c: char| !(c.is_ascii_digit() || c == ';'))
            .unwrap_or(after.len());
        if after[end..].starts_with('m') {
            rest = &after[end + 1..];
        } else {
            out.push_str(PREFIX);
            rest = after;
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

pub(crate) struct Scanned {
    pub(crate) annotation: Option<Annotation>,
    pub(crate) failures: Vec<String>,
}

pub(crate) struct Matching {
    registry: Registry,
    version: Option<u64>,
    running: Vec<Running>,
}

impl Matching {
    pub(crate) fn new(registry: Registry) -> Matching {
        Matching {
            registry,
            version: None,
            running: Vec::new(),
        }
    }

    pub(crate) fn registry(&self) -> &Registry {
        &self.registry
    }

    fn refresh(&mut self) {
        let Some((version, matchers)) = self.registry.changed(self.version) else {
            return;
        };
        let mut previous = std::mem::take(&mut self.running);
        self.running = matchers
            .into_iter()
            .map(|matcher| {
                match previous
                    .iter()
                    .position(|running| Arc::ptr_eq(&running.matcher, &matcher))
                {
                    Some(position) => previous.swap_remove(position),
                    None => Running::new(matcher),
                }
            })
            .collect();
        self.version = Some(version);
    }

    pub(crate) fn scan(&mut self, line: &str, place: &Place) -> Scanned {
        self.refresh();
        let mut failures = Vec::new();
        let mut position = 0;
        while position < self.running.len() {
            match self.running[position].step(line) {
                Ok(Some(found)) => {
                    for (other, running) in self.running.iter_mut().enumerate() {
                        if other != position {
                            running.reset();
                        }
                    }
                    if let Some(annotation) = annotation(&found, place) {
                        return Scanned {
                            annotation: Some(annotation),
                            failures,
                        };
                    }
                    position += 1;
                }
                Ok(None) => position += 1,
                Err(error) => {
                    let failed = self.running.remove(position);
                    self.registry.discard(&failed.matcher);
                    failures.push(format!(
                        "the problem matcher `{}` is removed, since matching a line failed: \
                         {error}",
                        failed.matcher.owner
                    ));
                }
            }
        }
        Scanned {
            annotation: None,
            failures,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{Matching, Place, Registry, Scanned, host, load, without_colours};
    use crate::annotation::AnnotationKind;

    fn place(directory: &tempfile::TempDir) -> Place {
        Place {
            workspace: directory.path().to_path_buf(),
            ..Place::default()
        }
    }

    fn matching(document: &str) -> Matching {
        let directory = tempfile::tempdir().expect("a directory");
        fs::write(directory.path().join("matcher.json"), document).expect("written");
        let registry = Registry::default();
        registry.add(load("matcher.json", &place(&directory)).expect("matchers"));
        Matching::new(registry)
    }

    fn messages(matching: &mut Matching, lines: &[&str]) -> Vec<Option<(AnnotationKind, String)>> {
        lines
            .iter()
            .map(|line| {
                let Scanned {
                    annotation,
                    failures,
                } = matching.scan(line, &Place::default());
                assert_eq!(failures, Vec::<String>::new());
                annotation.map(|annotation| (annotation.kind, annotation.message))
            })
            .collect()
    }

    #[test]
    fn a_loop_pattern_keeps_its_running_match_until_a_line_breaks_it() {
        let mut matching = matching(
            r#"{"problemMatcher": [{"owner": "o", "severity": "warning", "pattern": [
                {"regexp": "^file (.*)$", "file": 1},
                {"regexp": "^  (\\d+) (.*)$", "line": 1, "message": 2, "loop": true}
            ]}]}"#,
        );
        assert_eq!(
            messages(
                &mut matching,
                &[
                    "  1 orphan",
                    "file a",
                    "  1 one",
                    "  2 two",
                    "",
                    "  3 three"
                ]
            ),
            [
                None,
                None,
                Some((AnnotationKind::Warning, "one".to_owned())),
                Some((AnnotationKind::Warning, "two".to_owned())),
                None,
                None
            ]
        );
    }

    #[test]
    fn a_pattern_that_is_not_the_last_must_match_on_the_line_before() {
        let mut matching = matching(
            r#"{"problemMatcher": [{"owner": "o", "pattern": [
                {"regexp": "^first$"},
                {"regexp": "^second$"},
                {"regexp": "^(third)$", "message": 1}
            ]}]}"#,
        );
        assert_eq!(
            messages(
                &mut matching,
                &[
                    "first", "second", "third", "third", "first", "second", "other", "third"
                ]
            ),
            [
                None,
                None,
                Some((AnnotationKind::Error, "third".to_owned())),
                None,
                None,
                None,
                None,
                None
            ]
        );
    }

    #[test]
    fn a_document_that_breaks_a_rule_of_problem_matchers_is_refused() {
        let directory = tempfile::tempdir().expect("a directory");
        let path = directory.path().join("matcher.json");
        let place = place(&directory);
        for (document, refusal) in [
            (
                r#"{"problemMatcher": [{"pattern": [{"regexp": "(.)", "message": 1}]}]}"#,
                "a problem matcher has no `owner`".to_owned(),
            ),
            (
                r#"{"problemMatcher": [{"owner": "o", "severity": "info", "pattern": [{"regexp": "(.)", "message": 1}]}]}"#,
                "the problem matcher `o` has the severity `info`, which is none of `error`, \
                 `warning` and `notice`"
                    .to_owned(),
            ),
            (
                r#"{"problemMatcher": [{"owner": "o", "pattern": []}]}"#,
                "the problem matcher `o` has no pattern".to_owned(),
            ),
            (
                r#"{"problemMatcher": [{"owner": "o", "pattern": [{"regexp": "(.)", "message": 1, "loop": true}]}]}"#,
                "only the last of several patterns of the problem matcher `o` may set `loop`"
                    .to_owned(),
            ),
            (
                r#"{"problemMatcher": [{"owner": "o", "pattern": [{"regexp": "(.)", "file": 1}, {"regexp": "(.)", "loop": true}]}]}"#,
                "the pattern of the problem matcher `o` that sets `loop` must set `message`"
                    .to_owned(),
            ),
            (
                r#"{"problemMatcher": [{"owner": "o", "pattern": [{"regexp": "(.)", "message": 2}]}]}"#,
                "the problem matcher `o` sets `message` to the group 2, which its pattern `(.)` \
                 does not have"
                    .to_owned(),
            ),
            (
                r#"{"problemMatcher": [{"owner": "o", "pattern": [{"regexp": "(.)", "file": 1}, {"regexp": "(.)", "file": 1, "message": 1}]}]}"#,
                "the problem matcher `o` sets `file` in more than one pattern".to_owned(),
            ),
            (
                r#"{"problemMatcher": [{"owner": "o", "pattern": [{"regexp": "(.)", "file": 1}]}]}"#,
                "no pattern of the problem matcher `o` sets `message`".to_owned(),
            ),
            (
                r#"{"problemMatcher": [{"owner": "o", "pattern": [{"regexp": "(.)", "message": 1}]}, {"owner": "O", "pattern": [{"regexp": "(.)", "message": 1}]}]}"#,
                "the owner `O` names more than one problem matcher".to_owned(),
            ),
        ] {
            fs::write(&path, document).expect("written");
            assert_eq!(
                load("matcher.json", &place).map(|_| ()),
                Err(refusal),
                "{document}"
            );
        }
    }

    #[test]
    fn a_matcher_whose_matching_exceeds_its_backtracking_limit_leaves_the_job() {
        let mut matching = matching(
            r#"{"problemMatcher": [{"owner": "slow", "pattern": [
                {"regexp": "^(?:(?=a)(a|aa))*c(.*)$", "message": 2}
            ]}]}"#,
        );
        let Scanned {
            annotation,
            failures,
        } = matching.scan(&"a".repeat(64), &Place::default());
        assert!(annotation.is_none());
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(
            failures[0].starts_with(
                "the problem matcher `slow` is removed, since matching a line failed: "
            ),
            "{failures:?}"
        );
        assert!(
            matching
                .registry()
                .changed(None)
                .is_some_and(|(_, matchers)| matchers.is_empty())
        );
        assert!(matching.scan("c", &Place::default()).annotation.is_none());
    }

    #[test]
    fn colour_codes_are_stripped_and_a_host_is_read_from_its_address() {
        assert_eq!(
            without_colours("\x1b[1m\x1b[31merror\x1b[0m: \x1b[xbad"),
            "error: \x1b[xbad"
        );
        assert_eq!(host("https://github.com"), "github.com");
        assert_eq!(
            host("https://user@git.example.org:8443/path"),
            "git.example.org"
        );
    }
}
