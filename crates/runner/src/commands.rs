use std::fs;
use std::io::{self, ErrorKind};
use std::path::PathBuf;

use serde_json::{Map, Value};

pub(crate) struct Commands {
    env: PathBuf,
    output: PathBuf,
    path: PathBuf,
    state: PathBuf,
    summary: PathBuf,
}

#[derive(Debug, Default)]
pub(crate) struct Written {
    pub(crate) env: Vec<(String, String)>,
    pub(crate) outputs: Vec<(String, String)>,
    pub(crate) paths: Vec<String>,
    pub(crate) faults: Vec<String>,
}

impl Commands {
    pub(crate) fn create(github: &Map<String, Value>) -> io::Result<Commands> {
        let file = |property: &str| {
            github
                .get(property)
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .ok_or_else(|| {
                    io::Error::other(format!("the step's `github` context has no `{property}`"))
                })
        };
        let commands = Commands {
            env: file("env")?,
            output: file("output")?,
            path: file("path")?,
            state: file("state")?,
            summary: file("step_summary")?,
        };
        for path in [
            &commands.env,
            &commands.output,
            &commands.path,
            &commands.state,
            &commands.summary,
        ] {
            if let Some(directory) = path.parent() {
                fs::create_dir_all(directory)?;
            }
            fs::write(path, b"")?;
        }
        Ok(commands)
    }

    pub(crate) fn read(&self) -> Written {
        let mut written = Written::default();
        let mut pairs_of = |variable: &str, path: &PathBuf| match read(path) {
            Ok(text) => match pairs(&text) {
                Ok(pairs) => pairs,
                Err(fault) => {
                    written.faults.push(format!("{variable}: {fault}"));
                    Vec::new()
                }
            },
            Err(error) => {
                written
                    .faults
                    .push(format!("{variable} could not be read: {error}"));
                Vec::new()
            }
        };
        let env = pairs_of("GITHUB_ENV", &self.env);
        let outputs = pairs_of("GITHUB_OUTPUT", &self.output);
        pairs_of("GITHUB_STATE", &self.state);
        written.env = env;
        written.outputs = outputs;
        match read(&self.path) {
            Ok(text) => {
                written.paths = lines(&text)
                    .map(|(line, _)| line.to_owned())
                    .filter(|line| !line.is_empty())
                    .collect();
            }
            Err(error) => written
                .faults
                .push(format!("GITHUB_PATH could not be read: {error}")),
        }
        written
    }
}

fn read(path: &PathBuf) -> io::Result<String> {
    match fs::read(path) {
        Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

fn lines(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (line, newline, after) = match rest.find('\n') {
            Some(end) if end > 0 && rest.as_bytes()[end - 1] == b'\r' => {
                (&rest[..end - 1], &rest[end - 1..=end], &rest[end + 1..])
            }
            Some(end) => (&rest[..end], &rest[end..=end], &rest[end + 1..]),
            None => (rest, "", ""),
        };
        rest = after;
        Some((line, newline))
    })
}

fn pairs(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    let mut lines = lines(text);
    while let Some((line, _)) = lines.next() {
        if line.is_empty() {
            continue;
        }
        let equals = line.find('=');
        let heredoc = line.find("<<");
        match (equals, heredoc) {
            (Some(equals), heredoc) if heredoc.is_none_or(|heredoc| equals < heredoc) => {
                let (name, value) = (&line[..equals], &line[equals + 1..]);
                if name.is_empty() {
                    return Err(format!("the line `{line}` has no name before its `=`"));
                }
                pairs.push((name.to_owned(), value.to_owned()));
            }
            (_, Some(heredoc)) => {
                let (name, delimiter) = (&line[..heredoc], &line[heredoc + 2..]);
                if name.is_empty() || delimiter.is_empty() {
                    return Err(format!(
                        "the line `{line}` needs a name before its `<<` and a delimiter after it"
                    ));
                }
                let mut value = String::new();
                let mut pending = "";
                loop {
                    let Some((next, newline)) = lines.next() else {
                        return Err(format!(
                            "the value of `{name}` never ends with a line `{delimiter}`"
                        ));
                    };
                    if next == delimiter {
                        break;
                    }
                    if newline.is_empty() {
                        return Err(format!(
                            "the value of `{name}` reaches the end of the file without a line \
                             `{delimiter}`"
                        ));
                    }
                    value.push_str(pending);
                    value.push_str(next);
                    pending = newline;
                }
                pairs.push((name.to_owned(), value));
            }
            _ => {
                return Err(format!(
                    "the line `{line}` is neither `NAME=VALUE` nor `NAME<<DELIMITER`"
                ));
            }
        }
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::pairs;

    fn pairs_of(text: &str) -> Vec<(String, String)> {
        pairs(text).unwrap_or_else(|fault| panic!("{text:?}: {fault}"))
    }

    fn pair(name: &str, value: &str) -> (String, String) {
        (name.to_owned(), value.to_owned())
    }

    #[test]
    fn a_line_is_a_name_and_a_value_split_at_its_first_equals_sign() {
        assert_eq!(
            pairs_of("A=1\n\nB=x=y\nC=\r\nD= spaced \n"),
            vec![
                pair("A", "1"),
                pair("B", "x=y"),
                pair("C", ""),
                pair("D", " spaced ")
            ]
        );
    }

    #[test]
    fn a_heredoc_keeps_its_lines_without_the_last_newline() {
        assert_eq!(
            pairs_of("notes<<EOF\nfirst\r\nsecond\nEOF\nempty<<END\nEND\nafter=1"),
            vec![
                pair("notes", "first\r\nsecond"),
                pair("empty", ""),
                pair("after", "1")
            ]
        );
        assert_eq!(pairs_of("a<<x=y\nv\nx=y"), vec![pair("a", "v")]);
        assert_eq!(pairs_of("a=b<<c\n"), vec![pair("a", "b<<c")]);
    }

    #[test]
    fn a_line_without_a_name_or_a_delimiter_and_a_heredoc_without_its_end_are_refused() {
        for (text, fault) in [
            (
                "value",
                "the line `value` is neither `NAME=VALUE` nor `NAME<<DELIMITER`",
            ),
            ("=value", "the line `=value` has no name before its `=`"),
            (
                "<<EOF\nx\nEOF",
                "the line `<<EOF` needs a name before its `<<` and a delimiter after it",
            ),
            (
                "notes<<",
                "the line `notes<<` needs a name before its `<<` and a delimiter after it",
            ),
            (
                "notes<<EOF\nfirst\n",
                "the value of `notes` never ends with a line `EOF`",
            ),
            (
                "notes<<EOF\nfirst",
                "the value of `notes` reaches the end of the file without a line `EOF`",
            ),
        ] {
            assert_eq!(pairs(text), Err(fault.to_owned()), "{text:?}");
        }
    }
}
