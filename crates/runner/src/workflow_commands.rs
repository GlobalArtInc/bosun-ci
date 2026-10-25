use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::annotation::{Annotation, AnnotationKind, Kept};
use crate::mask::Sink;
use crate::matchers::{self, Matching, Place, Registry, Scanned};

const KEY: &str = "::";

const DATA: [(&str, &str); 3] = [("%0D", "\r"), ("%0A", "\n"), ("%25", "%")];

const PROPERTY: [(&str, &str); 5] = [
    ("%0D", "\r"),
    ("%0A", "\n"),
    ("%3A", ":"),
    ("%2C", ","),
    ("%25", "%"),
];

#[derive(Default)]
pub(crate) struct Effects {
    pub(crate) annotations: Kept,
    pub(crate) outputs: Vec<(String, String)>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) paths: Vec<String>,
    pub(crate) failed: bool,
}

#[derive(Clone)]
pub(crate) struct Reports {
    index: u32,
    sink: Sink,
    effects: Arc<Mutex<Effects>>,
}

impl Reports {
    pub(crate) fn new(index: u32, sink: Sink) -> Reports {
        Reports {
            index,
            sink,
            effects: Arc::default(),
        }
    }

    fn effects(&self) -> MutexGuard<'_, Effects> {
        self.effects.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn take(&self) -> Effects {
        std::mem::take(&mut *self.effects())
    }

    fn line(&self, text: &str) {
        for line in text.split('\n') {
            self.sink.line(self.index, line);
        }
    }

    fn marked(&self, marker: &str, text: &str) {
        self.line(&format!("##[{marker}]{text}"));
    }

    pub(crate) fn note(&self, kind: AnnotationKind, message: String) {
        self.annotate(Annotation::new(kind, message), None);
    }

    fn annotate(&self, annotation: Annotation, logged: Option<&str>) {
        if annotation.message.is_empty() {
            return;
        }
        self.marked(
            annotation.kind.name(),
            logged.unwrap_or(&annotation.message),
        );
        self.effects().annotations.add(annotation);
    }

    fn fail(&self, line: &str, reason: &str) {
        self.note(
            AnnotationKind::Error,
            format!("the command `{line}` could not be processed: {reason}"),
        );
        self.effects().failed = true;
    }
}

#[derive(Default)]
pub(crate) struct Settings {
    pub(crate) debug: bool,
    pub(crate) unsecure_commands: bool,
    pub(crate) unsecure_stop_tokens: bool,
    pub(crate) place: Place,
}

#[derive(Clone, Copy)]
enum Name {
    AddMask,
    AddMatcher,
    AddPath,
    Debug,
    Echo,
    EndGroup,
    Error,
    Group,
    Notice,
    RemoveMatcher,
    SaveState,
    SetEnv,
    SetOutput,
    StopCommands,
    Warning,
}

impl Name {
    fn of(name: &str) -> Option<Name> {
        Some(match name {
            "add-mask" => Name::AddMask,
            "add-matcher" => Name::AddMatcher,
            "add-path" => Name::AddPath,
            "debug" => Name::Debug,
            "echo" => Name::Echo,
            "endgroup" => Name::EndGroup,
            "error" => Name::Error,
            "group" => Name::Group,
            "notice" => Name::Notice,
            "remove-matcher" => Name::RemoveMatcher,
            "save-state" => Name::SaveState,
            "set-env" => Name::SetEnv,
            "set-output" => Name::SetOutput,
            "stop-commands" => Name::StopCommands,
            "warning" => Name::Warning,
            _ => return None,
        })
    }

    fn echoes(self) -> bool {
        matches!(
            self,
            Name::AddMatcher
                | Name::AddPath
                | Name::Echo
                | Name::RemoveMatcher
                | Name::SaveState
                | Name::SetEnv
                | Name::SetOutput
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Parsed {
    name: String,
    properties: BTreeMap<String, String>,
    data: String,
}

fn unescape(text: &str, mappings: &[(&str, &str)]) -> String {
    mappings
        .iter()
        .fold(text.to_owned(), |text, (escaped, plain)| {
            text.replace(escaped, plain)
        })
}

fn parse(line: &str) -> Option<Parsed> {
    let rest = line.trim_start().strip_prefix(KEY)?;
    let end = rest.find(KEY)?;
    let (info, data) = (&rest[..end], &rest[end + KEY.len()..]);
    let (name, properties) = info.split_once(' ').unwrap_or((info, ""));
    let properties = properties
        .trim()
        .split(',')
        .filter_map(|property| {
            let (key, value) = property.split_once('=')?;
            (!key.is_empty() && !value.is_empty())
                .then(|| (key.to_lowercase(), unescape(value, &PROPERTY)))
        })
        .collect();
    Some(Parsed {
        name: name.to_lowercase(),
        properties,
        data: unescape(data, &DATA),
    })
}

fn listed(owners: &[String]) -> String {
    owners
        .iter()
        .map(|owner| format!("`{owner}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn required(properties: &BTreeMap<String, String>, command: &str) -> Result<String, String> {
    properties
        .get("name")
        .cloned()
        .ok_or_else(|| format!("`::{command}` needs the property `name`"))
}

pub(crate) struct Processor {
    reports: Reports,
    settings: Settings,
    matching: Matching,
    stopped: Option<String>,
    echo: bool,
}

impl Processor {
    pub(crate) fn new(reports: Reports, settings: Settings, registry: Registry) -> Processor {
        Processor {
            reports,
            settings,
            matching: Matching::new(registry),
            stopped: None,
            echo: false,
        }
    }

    pub(crate) fn line(&mut self, raw: &[u8]) -> bool {
        let text = String::from_utf8_lossy(raw);
        let line = text.strip_suffix('\r').unwrap_or(&text);
        self.command(line) || self.matched(line)
    }

    fn matched(&mut self, line: &str) -> bool {
        let stripped = matchers::without_colours(line);
        let Scanned {
            annotation,
            failures,
        } = self.matching.scan(&stripped, &self.settings.place);
        for failure in failures {
            self.reports.note(AnnotationKind::Warning, failure);
        }
        match annotation {
            Some(annotation) => {
                self.reports.annotate(annotation, Some(&stripped));
                true
            }
            None => false,
        }
    }

    fn command(&mut self, line: &str) -> bool {
        let Some(parsed) = parse(line) else {
            return false;
        };
        if let Some(token) = &self.stopped {
            if parsed.name != token.to_lowercase() {
                return false;
            }
            self.reports.line(line);
            self.stopped = None;
            return true;
        }
        let Some(name) = Name::of(&parsed.name) else {
            return false;
        };
        if self.echo && name.echoes() {
            self.reports.line(line);
        }
        if let Err(reason) = self.run(name, parsed, line) {
            self.reports.fail(line, &reason);
        }
        true
    }

    fn run(&mut self, name: Name, parsed: Parsed, line: &str) -> Result<(), String> {
        let Parsed {
            properties, data, ..
        } = parsed;
        match name {
            Name::Debug => {
                if self.settings.debug {
                    for line in data.split('\n') {
                        self.reports.marked("debug", line);
                    }
                }
            }
            Name::Group => self.reports.marked("group", &data),
            Name::EndGroup => self.reports.marked("endgroup", ""),
            Name::Notice => self.annotation(AnnotationKind::Notice, &properties, data),
            Name::Warning => self.annotation(AnnotationKind::Warning, &properties, data),
            Name::Error => self.annotation(AnnotationKind::Error, &properties, data),
            Name::AddMask => {
                if data.trim().is_empty() {
                    self.reports.note(
                        AnnotationKind::Warning,
                        "`::add-mask::` was given an empty value, which masks nothing".to_owned(),
                    );
                } else {
                    self.reports.sink.add_mask(&data);
                }
            }
            Name::StopCommands => self.stop(data, line)?,
            Name::Echo => match data.trim().to_uppercase().as_str() {
                "ON" => self.echo = true,
                "OFF" => self.echo = false,
                _ => return Err(format!("`::echo::` takes `on` or `off`, not `{data}`")),
            },
            Name::AddMatcher => self.add_matcher(&data)?,
            Name::RemoveMatcher => self.remove_matcher(&properties, &data)?,
            Name::SetOutput => {
                self.deprecated("set-output", "GITHUB_OUTPUT");
                let name = required(&properties, "set-output")?;
                self.reports.effects().outputs.push((name, data));
            }
            Name::SaveState => {
                self.deprecated("save-state", "GITHUB_STATE");
                required(&properties, "save-state")?;
            }
            Name::SetEnv => {
                self.unsecure("set-env", "GITHUB_ENV")?;
                let name = required(&properties, "set-env")?;
                self.reports.effects().env.push((name, data));
            }
            Name::AddPath => {
                self.unsecure("add-path", "GITHUB_PATH")?;
                if !data.is_empty() {
                    self.reports.effects().paths.push(data);
                }
            }
        }
        Ok(())
    }

    fn annotation(
        &self,
        kind: AnnotationKind,
        properties: &BTreeMap<String, String>,
        message: String,
    ) {
        let number = |key: &str| {
            properties
                .get(key)
                .and_then(|value| value.trim().parse::<u32>().ok())
        };
        let (mut line, mut end_line) = (number("line"), number("endline"));
        let (mut column, mut end_column) = (number("col"), number("endcolumn"));
        let columns = column.is_some() || end_column.is_some();
        if line.is_none() {
            line = end_line;
        }
        if column.is_none() {
            column = end_column;
        }
        if line.is_none() && columns {
            (column, end_column) = (None, None);
        }
        if let (Some(start), Some(end)) = (line, end_line) {
            if start != end && columns {
                (column, end_column) = (None, None);
            }
            if end < start {
                (line, end_line) = (None, None);
            }
        }
        if let (Some(start), Some(end)) = (column, end_column)
            && end < start
        {
            (column, end_column) = (None, None);
        }
        let mut annotation = Annotation::new(kind, message);
        annotation.title = properties.get("title").cloned();
        annotation.file = properties
            .get("file")
            .map(|file| self.settings.place.relative(file));
        annotation.line = line;
        annotation.end_line = end_line;
        annotation.column = column;
        annotation.end_column = end_column;
        self.reports.annotate(annotation, None);
    }

    fn stop(&mut self, token: String, line: &str) -> Result<(), String> {
        let reserved = token.is_empty()
            || token.eq_ignore_ascii_case("pause-logging")
            || Name::of(&token.to_lowercase()).is_some();
        if reserved && !self.settings.unsecure_stop_tokens {
            return Err(
                "the token of `::stop-commands::` must not be empty, `pause-logging` or the name \
                 of a command: choose another, or set `ACTIONS_ALLOW_UNSECURE_STOPCOMMAND_TOKENS` \
                 to `true` in the step's environment to allow it"
                    .to_owned(),
            );
        }
        self.reports.line(line);
        self.stopped = Some(token);
        Ok(())
    }

    fn add_matcher(&mut self, file: &str) -> Result<(), String> {
        if file.is_empty() {
            self.reports.note(
                AnnotationKind::Warning,
                "`::add-matcher::` names no file of problem matchers".to_owned(),
            );
            return Ok(());
        }
        let matchers = matchers::load(file, &self.settings.place)?;
        if matchers.is_empty() {
            return Ok(());
        }
        let owners = matchers::owners(&matchers);
        self.matching.registry().add(matchers);
        self.reports
            .line(&format!("added the problem matchers {}", listed(&owners)));
        Ok(())
    }

    fn remove_matcher(
        &mut self,
        properties: &BTreeMap<String, String>,
        file: &str,
    ) -> Result<(), String> {
        let owners = match (properties.get("owner"), file.is_empty()) {
            (Some(_), false) => {
                self.reports.note(
                    AnnotationKind::Warning,
                    "`::remove-matcher` takes an `owner` or a file, not both".to_owned(),
                );
                return Ok(());
            }
            (None, true) => {
                self.reports.note(
                    AnnotationKind::Warning,
                    "`::remove-matcher` takes an `owner` or a file of problem matchers".to_owned(),
                );
                return Ok(());
            }
            (Some(owner), true) => vec![owner.clone()],
            (None, false) => {
                let matchers = matchers::load(file, &self.settings.place)?;
                if matchers.is_empty() {
                    return Ok(());
                }
                matchers::owners(&matchers)
            }
        };
        let removed = self.matching.registry().remove(&owners);
        self.reports.line(&if removed.is_empty() {
            "removed no problem matcher".to_owned()
        } else {
            format!("removed the problem matchers {}", listed(&removed))
        });
        Ok(())
    }

    fn deprecated(&self, command: &str, file: &str) {
        self.reports.note(
            AnnotationKind::Warning,
            format!("`::{command}` is deprecated: write to the file in `{file}` instead"),
        );
    }

    fn unsecure(&self, command: &str, file: &str) -> Result<(), String> {
        if self.settings.unsecure_commands {
            return Ok(());
        }
        Err(format!(
            "`::{command}` is disabled: write to the file in `{file}` instead, or set \
             `ACTIONS_ALLOW_UNSECURE_COMMANDS` to `true` in the step's environment to allow it"
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Parsed, parse};

    fn parsed(name: &str, properties: &[(&str, &str)], data: &str) -> Option<Parsed> {
        Some(Parsed {
            name: name.to_owned(),
            properties: properties
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect::<BTreeMap<_, _>>(),
            data: data.to_owned(),
        })
    }

    #[test]
    fn a_command_is_a_name_its_properties_and_its_data_unescaped_as_the_toolkit_escapes_them() {
        assert_eq!(
            parse("::warning file=a%2Cb.ts,line=3,title=x%3Ay%250A::one%0Atwo%0D%25 :: three"),
            parsed(
                "warning",
                &[("file", "a,b.ts"), ("line", "3"), ("title", "x:y%0A")],
                "one\ntwo\r% :: three"
            )
        );
        assert_eq!(
            parse("  ::Set-Output Name=out,=x,empty=,bare::value"),
            parsed("set-output", &[("name", "out")], "value")
        );
        assert_eq!(parse("::endgroup::"), parsed("endgroup", &[], ""));
        assert_eq!(parse("text ::warning::x"), None);
        assert_eq!(parse("::warning only"), None);
    }
}
