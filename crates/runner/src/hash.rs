use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const DIRECTORY: u8 = 1;

const FILE: u8 = 2;

pub(crate) fn files(workspace: &Path, arguments: &[String]) -> Result<String, String> {
    let patterns = patterns(workspace, arguments)?;
    let mut digest = Sha256::new();
    let mut found = false;
    for file in walk(&patterns)? {
        if !file.starts_with(workspace) || file == workspace {
            continue;
        }
        let metadata = fs::metadata(&file)
            .map_err(|error| format!("`{}` could not be read: {error}", file.display()))?;
        if metadata.is_dir() {
            continue;
        }
        digest.update(contents(&file)?);
        found = true;
    }
    Ok(if found {
        hex(&digest.finalize())
    } else {
        String::new()
    })
}

fn contents(file: &Path) -> Result<[u8; 32], String> {
    let failed = |error: io::Error| format!("`{}` could not be read: {error}", file.display());
    let mut reader = File::open(file).map_err(failed)?;
    let mut digest = Sha256::new();
    io::copy(&mut reader, &mut digest).map_err(failed)?;
    Ok(digest.finalize().into())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn walk(patterns: &[Pattern]) -> Result<Vec<PathBuf>, String> {
    let mut stack = Vec::new();
    for search in search_paths(patterns) {
        match fs::symlink_metadata(&search) {
            Ok(_) => stack.insert(0, search),
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("`{}` could not be read: {error}", search.display())),
        }
    }
    let mut found = Vec::new();
    while let Some(item) = stack.pop() {
        let text = item.to_string_lossy();
        let matched = matched(patterns, &text);
        let partial = patterns
            .iter()
            .any(|pattern| !pattern.negate && pattern.partial(&text));
        if matched == 0 && !partial {
            continue;
        }
        let metadata = fs::symlink_metadata(&item)
            .map_err(|error| format!("`{}` could not be read: {error}", item.display()))?;
        if metadata.is_dir() {
            if matched & DIRECTORY == 0 && !partial {
                continue;
            }
            let mut children = Vec::new();
            for entry in fs::read_dir(&item)
                .map_err(|error| format!("`{}` could not be listed: {error}", item.display()))?
            {
                let entry = entry.map_err(|error| {
                    format!("`{}` could not be listed: {error}", item.display())
                })?;
                children.push(entry.file_name());
            }
            children.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
            stack.extend(children.into_iter().rev().map(|name| item.join(name)));
        } else if matched & FILE != 0 {
            found.push(item);
        }
    }
    Ok(found)
}

fn matched(patterns: &[Pattern], item: &str) -> u8 {
    let mut kind = 0;
    for pattern in patterns {
        if pattern.negate {
            kind &= !pattern.matches(item);
        } else {
            kind |= pattern.matches(item);
        }
    }
    kind
}

fn search_paths(patterns: &[Pattern]) -> Vec<PathBuf> {
    let candidates: Vec<&PathBuf> = patterns
        .iter()
        .filter(|pattern| !pattern.negate)
        .map(|pattern| &pattern.search)
        .collect();
    let mut included: Vec<PathBuf> = Vec::new();
    for search in &candidates {
        if included.contains(search) {
            continue;
        }
        let nested = search
            .ancestors()
            .skip(1)
            .any(|ancestor| candidates.iter().any(|other| other.as_path() == ancestor));
        if !nested {
            included.push((*search).clone());
        }
    }
    included
}

#[derive(Debug)]
struct Pattern {
    negate: bool,
    segments: Vec<Segment>,
    directories_only: bool,
    implicit: bool,
    search: PathBuf,
}

#[derive(Debug, Clone)]
enum Segment {
    Globstar,
    Literal(String),
    Glob(Vec<Token>),
}

#[derive(Debug, Clone)]
enum Token {
    Char(char),
    One,
    Any,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

fn patterns(workspace: &Path, arguments: &[String]) -> Result<Vec<Pattern>, String> {
    let mut patterns = Vec::new();
    for line in arguments.join("\n").split('\n') {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let pattern = Pattern::new(workspace, line)?;
        let descendants = (pattern.directories_only
            || !matches!(pattern.segments.last(), Some(Segment::Globstar)))
        .then(|| {
            let mut segments = pattern.segments.clone();
            segments.push(Segment::Globstar);
            Pattern {
                negate: pattern.negate,
                segments,
                directories_only: false,
                implicit: true,
                search: pattern.search.clone(),
            }
        });
        patterns.push(pattern);
        patterns.extend(descendants);
    }
    Ok(patterns)
}

impl Pattern {
    fn new(workspace: &Path, line: &str) -> Result<Pattern, String> {
        let mut negate = false;
        let mut text = line;
        while let Some(rest) = text.strip_prefix('!') {
            negate = !negate;
            text = rest.trim();
        }
        if text.is_empty() {
            return Err(format!("`{line}` is a pattern without a path"));
        }
        let mut normalized = String::with_capacity(text.len());
        for c in text.chars() {
            if !(c == '/' && normalized.ends_with('/')) {
                normalized.push(c);
            }
        }
        let absolute = normalized.starts_with('/');
        let directories_only = normalized.len() > 1 && normalized.ends_with('/');
        let body = normalized.trim_start_matches('/').trim_end_matches('/');
        let mut written: Vec<&str> = if body.is_empty() {
            Vec::new()
        } else {
            body.split('/').collect()
        };
        if !absolute
            && written
                .first()
                .is_some_and(|first| literal(first).as_deref() == Some("."))
        {
            written.remove(0);
        }
        if written
            .iter()
            .any(|segment| matches!(literal(segment).as_deref(), Some(".") | Some("..")))
        {
            return Err(format!(
                "`{line}` holds `.` or `..` as a part of its path, which a pattern may not"
            ));
        }
        let mut segments = vec![Segment::Literal(String::new())];
        if !absolute {
            for component in workspace.components().skip(1) {
                segments.push(Segment::Literal(
                    component.as_os_str().to_string_lossy().into_owned(),
                ));
            }
        }
        for segment in written {
            segments.push(compile(segment));
        }
        let mut search = PathBuf::from("/");
        for segment in &segments[1..] {
            match segment {
                Segment::Literal(text) => search.push(text),
                Segment::Globstar | Segment::Glob(_) => break,
            }
        }
        Ok(Pattern {
            negate,
            segments,
            directories_only,
            implicit: false,
            search,
        })
    }

    fn matches(&self, item: &str) -> u8 {
        let mut parts: Vec<&str> = item.split('/').collect();
        if matches!(self.segments.last(), Some(Segment::Globstar)) && !self.implicit {
            parts.push("");
        }
        if !one(&parts, &self.segments, false) {
            0
        } else if self.directories_only {
            DIRECTORY
        } else {
            DIRECTORY | FILE
        }
    }

    fn partial(&self, item: &str) -> bool {
        if item == "/" {
            return true;
        }
        let parts: Vec<&str> = item.split('/').collect();
        one(&parts, &self.segments, true)
    }
}

fn one(parts: &[&str], segments: &[Segment], partial: bool) -> bool {
    let mut index = 0;
    while index < parts.len() && index < segments.len() {
        match &segments[index] {
            Segment::Globstar => {
                if index + 1 == segments.len() {
                    return true;
                }
                for start in index..parts.len() {
                    if one(&parts[start..], &segments[index + 1..], partial) {
                        return true;
                    }
                }
                return partial;
            }
            Segment::Literal(text) => {
                if parts[index] != text {
                    return false;
                }
            }
            Segment::Glob(tokens) => {
                let chars: Vec<char> = parts[index].chars().collect();
                if !glob(&chars, tokens) {
                    return false;
                }
            }
        }
        index += 1;
    }
    if index == parts.len() && index == segments.len() {
        true
    } else if index == parts.len() {
        partial
    } else {
        index == parts.len() - 1 && parts[index].is_empty()
    }
}

fn glob(chars: &[char], tokens: &[Token]) -> bool {
    match tokens.split_first() {
        None => chars.is_empty(),
        Some((Token::Any, rest)) => (0..=chars.len()).any(|skip| glob(&chars[skip..], rest)),
        Some((token, rest)) => chars.split_first().is_some_and(|(c, after)| {
            let fits = match token {
                Token::Char(expected) => c == expected,
                Token::One => true,
                Token::Class { negated, ranges } => {
                    ranges.iter().any(|(low, high)| low <= c && c <= high) != *negated
                }
                Token::Any => unreachable!(),
            };
            fits && glob(after, rest)
        }),
    }
}

fn compile(segment: &str) -> Segment {
    if segment == "**" {
        return Segment::Globstar;
    }
    let chars: Vec<char> = segment.chars().collect();
    let mut tokens = Vec::new();
    let mut magic = false;
    let mut at = 0;
    while at < chars.len() {
        match chars[at] {
            '\\' if at + 1 < chars.len() => {
                tokens.push(Token::Char(chars[at + 1]));
                at += 2;
            }
            '*' => {
                if !matches!(tokens.last(), Some(Token::Any)) {
                    tokens.push(Token::Any);
                }
                magic = true;
                at += 1;
            }
            '?' => {
                tokens.push(Token::One);
                magic = true;
                at += 1;
            }
            '[' => match class(&chars, at) {
                Some((
                    Token::Class {
                        negated: false,
                        ranges,
                    },
                    next,
                    1,
                )) => {
                    tokens.push(Token::Char(ranges[0].0));
                    at = next;
                }
                Some((token, next, _)) => {
                    tokens.push(token);
                    magic = true;
                    at = next;
                }
                None => {
                    tokens.push(Token::Char('['));
                    at += 1;
                }
            },
            c => {
                tokens.push(Token::Char(c));
                at += 1;
            }
        }
    }
    if magic {
        Segment::Glob(tokens)
    } else {
        Segment::Literal(
            tokens
                .into_iter()
                .map(|token| match token {
                    Token::Char(c) => c,
                    Token::One | Token::Any | Token::Class { .. } => unreachable!(),
                })
                .collect(),
        )
    }
}

fn class(chars: &[char], open: usize) -> Option<(Token, usize, usize)> {
    let mut at = open + 1;
    let negated = matches!(chars.get(at), Some('!' | '^'));
    if negated {
        at += 1;
    }
    let first = at;
    let mut members = Vec::new();
    loop {
        let c = *chars.get(at)?;
        if c == ']' && at > first {
            break;
        }
        if c == '\\' {
            members.push(*chars.get(at + 1)?);
            at += 2;
        } else {
            members.push(c);
            at += 1;
        }
    }
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < members.len() {
        if index + 2 < members.len() && members[index + 1] == '-' {
            let (low, high) = (members[index], members[index + 2]);
            if low > high {
                return None;
            }
            ranges.push((low, high));
            index += 3;
        } else {
            ranges.push((members[index], members[index]));
            index += 1;
        }
    }
    let count = members.len();
    Some((Token::Class { negated, ranges }, at + 1, count))
}

fn literal(segment: &str) -> Option<String> {
    match compile(segment) {
        Segment::Literal(text) => Some(text),
        Segment::Globstar | Segment::Glob(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{Pattern, compile, glob, one, patterns, search_paths};

    fn matches(pattern: &str, name: &str) -> bool {
        let chars: Vec<char> = name.chars().collect();
        match compile(pattern) {
            super::Segment::Glob(tokens) => glob(&chars, &tokens),
            super::Segment::Literal(text) => text == name,
            super::Segment::Globstar => true,
        }
    }

    #[test]
    fn wildcards_classes_and_escapes_match_one_part_of_a_path() {
        for (pattern, name, expected) in [
            ("*.txt", "a.txt", true),
            ("*.txt", ".hidden.txt", true),
            ("*.txt", "a.txt.bak", false),
            ("x?.log", "x1.log", true),
            ("x?.log", "x.log", false),
            ("x[0-9].log", "x7.log", true),
            ("x[0-9].log", "xa.log", false),
            ("x[!0-9].log", "xa.log", true),
            ("x[^0-9].log", "x1.log", false),
            ("[]]", "]", true),
            ("[a-]", "-", true),
            ("[z-a]", "[z-a]", true),
            ("[unclosed", "[unclosed", true),
            ("\\*", "*", true),
            ("\\*", "a", false),
            ("a**b", "axyzb", true),
        ] {
            assert_eq!(matches(pattern, name), expected, "`{pattern}` and `{name}`");
        }
    }

    #[test]
    fn a_relative_pattern_is_rooted_at_the_workspace_and_searches_below_its_literal_part() {
        let workspace = Path::new("/w/s");
        let found =
            patterns(workspace, &["src/*/x".to_owned(), "!src/a/x".to_owned()]).expect("patterns");
        assert_eq!(found.len(), 4);
        assert_eq!(found[0].search, Path::new("/w/s/src"));
        assert_eq!(
            search_paths(&found),
            vec![Path::new("/w/s/src").to_path_buf()]
        );
        let absolute = Pattern::new(workspace, "/etc/*.conf").expect("a pattern");
        assert_eq!(absolute.search, Path::new("/etc"));
        let dotted = Pattern::new(workspace, "./a/b").expect("a pattern");
        assert_eq!(dotted.search, Path::new("/w/s/a/b"));
        for refused in ["../x", "a/./b", "a/..", "!", "!!  "] {
            assert!(
                Pattern::new(workspace, refused).is_err(),
                "`{refused}` was taken"
            );
        }
    }

    #[test]
    fn a_trailing_globstar_written_in_the_pattern_matches_the_directory_before_it() {
        let workspace = Path::new("/w");
        let written = Pattern::new(workspace, "a/**").expect("a pattern");
        assert_eq!(written.matches("/w/a"), 3);
        assert_eq!(written.matches("/w/a/b/c"), 3);
        assert_eq!(written.matches("/w/b"), 0);
        let implied = &patterns(workspace, &["a".to_owned()]).expect("patterns")[1];
        assert_eq!(implied.matches("/w/a"), 0);
        assert_eq!(implied.matches("/w/a/b"), 3);
        let parts: Vec<&str> = "/w".split('/').collect();
        assert!(one(&parts, &written.segments, true));
    }
}
