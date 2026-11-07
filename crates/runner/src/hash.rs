use std::cell::Cell;
use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::safe;

const DIRECTORY: u8 = 1;

const FILE: u8 = 2;

const CHUNK: usize = 64 * 1024;

const TEXT_LIMIT: usize = 4096;

const STRIDE: u32 = 16 * 1024;

pub(crate) struct Limits {
    pub(crate) files: usize,
    pub(crate) bytes: u64,
    pub(crate) entries: usize,
}

pub(crate) const LIMITS: Limits = Limits {
    files: 100_000,
    bytes: 1024 * 1024 * 1024,
    entries: 1_000_000,
};

pub(crate) type Halt<'a> = &'a dyn Fn() -> Option<&'static str>;

fn interrupted(halt: Halt<'_>) -> Result<(), String> {
    match halt() {
        Some(reason) => Err(format!("{reason}, so the files were not hashed")),
        None => Ok(()),
    }
}

struct Meter<'a> {
    halt: Halt<'a>,
    steps: Cell<u32>,
}

impl<'a> Meter<'a> {
    fn new(halt: Halt<'a>) -> Meter<'a> {
        Meter {
            halt,
            steps: Cell::new(0),
        }
    }

    fn spend(&self, steps: usize) -> Result<(), String> {
        let total = self
            .steps
            .get()
            .saturating_add(u32::try_from(steps).unwrap_or(u32::MAX));
        if total >= STRIDE {
            self.steps.set(0);
            interrupted(self.halt)
        } else {
            self.steps.set(total);
            Ok(())
        }
    }
}

pub(crate) fn files(
    workspace: &Path,
    arguments: &[String],
    limits: &Limits,
    halt: Halt<'_>,
) -> Result<String, String> {
    let patterns = patterns(workspace, arguments)?;
    let meter = Meter::new(halt);
    let mut digest = Sha256::new();
    let mut found = false;
    let mut read = 0;
    for file in walk(workspace, &patterns, limits, &meter)? {
        interrupted(halt)?;
        let Ok(relative) = file.strip_prefix(workspace) else {
            continue;
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        match safe::open_below(workspace, relative) {
            Ok(opened) => {
                digest.update(contents(opened, &file, &mut read, limits, halt)?);
                found = true;
            }
            Err(error) if error.kind() == ErrorKind::InvalidInput => {}
            Err(error) => {
                return Err(format!("`{}` could not be read: {error}", file.display()));
            }
        }
    }
    Ok(if found {
        hex(&digest.finalize())
    } else {
        String::new()
    })
}

fn contents(
    mut reader: File,
    file: &Path,
    read: &mut u64,
    limits: &Limits,
    halt: Halt<'_>,
) -> Result<[u8; 32], String> {
    let mut digest = Sha256::new();
    let mut chunk = vec![0; CHUNK];
    loop {
        interrupted(halt)?;
        let count = reader
            .read(&mut chunk)
            .map_err(|error| format!("`{}` could not be read: {error}", file.display()))?;
        if count == 0 {
            return Ok(digest.finalize().into());
        }
        *read += count as u64;
        if *read > limits.bytes {
            return Err(format!(
                "the files the patterns match hold more than {} bytes, which `hashFiles` does \
                 not read",
                limits.bytes
            ));
        }
        digest.update(&chunk[..count]);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

struct Pending {
    path: PathBuf,
    matched: u8,
    partial: bool,
}

fn judge(
    patterns: &[Pattern],
    path: PathBuf,
    meter: &Meter<'_>,
) -> Result<Option<Pending>, String> {
    let text = path.to_string_lossy();
    let matched = matched(patterns, &text, meter)?;
    let mut partial = false;
    for pattern in patterns {
        if !pattern.negate && pattern.partial(&text, meter)? {
            partial = true;
            break;
        }
    }
    drop(text);
    Ok((matched != 0 || partial).then_some(Pending {
        path,
        matched,
        partial,
    }))
}

fn walk(
    workspace: &Path,
    patterns: &[Pattern],
    limits: &Limits,
    meter: &Meter<'_>,
) -> Result<Vec<PathBuf>, String> {
    let mut stack = Vec::new();
    let mut visited = 0_usize;
    for search in search_paths(workspace, patterns) {
        match fs::symlink_metadata(&search) {
            Ok(_) => {
                if let Some(pending) = judge(patterns, search, meter)? {
                    stack.insert(0, pending);
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("`{}` could not be read: {error}", search.display())),
        }
    }
    let mut found = Vec::new();
    while let Some(Pending {
        path: item,
        matched,
        partial,
    }) = stack.pop()
    {
        interrupted(meter.halt)?;
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
                visited += 1;
                if visited > limits.entries {
                    return Err(format!(
                        "the workspace holds more than {} entries below the paths the patterns \
                         search, which `hashFiles` does not walk",
                        limits.entries
                    ));
                }
                meter.spend(1)?;
                if let Some(child) = judge(patterns, item.join(entry.file_name()), meter)? {
                    children.push(child);
                }
            }
            children.sort_by(|left, right| {
                left.path
                    .as_os_str()
                    .as_bytes()
                    .cmp(right.path.as_os_str().as_bytes())
            });
            stack.extend(children.into_iter().rev());
        } else if matched & FILE != 0 {
            if found.len() >= limits.files {
                return Err(format!(
                    "the patterns match more than {} files, which `hashFiles` does not hash",
                    limits.files
                ));
            }
            found.push(item);
        }
    }
    Ok(found)
}

fn matched(patterns: &[Pattern], item: &str, meter: &Meter<'_>) -> Result<u8, String> {
    let mut kind = 0;
    for pattern in patterns {
        if pattern.negate {
            kind &= !pattern.matches(item, meter)?;
        } else {
            kind |= pattern.matches(item, meter)?;
        }
    }
    Ok(kind)
}

fn search_paths(workspace: &Path, patterns: &[Pattern]) -> Vec<PathBuf> {
    let candidates: Vec<PathBuf> = patterns
        .iter()
        .filter(|pattern| !pattern.negate)
        .filter_map(|pattern| {
            if pattern.search.starts_with(workspace) {
                Some(pattern.search.clone())
            } else if workspace.starts_with(&pattern.search) {
                Some(workspace.to_path_buf())
            } else {
                None
            }
        })
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
            included.push(search.clone());
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
        if line.len() > TEXT_LIMIT {
            return Err(too_long("a pattern"));
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

    fn matches(&self, item: &str, meter: &Meter<'_>) -> Result<u8, String> {
        if item.len() > TEXT_LIMIT {
            return Err(too_long("a path"));
        }
        let mut parts: Vec<&str> = item.split('/').collect();
        if matches!(self.segments.last(), Some(Segment::Globstar)) && !self.implicit {
            parts.push("");
        }
        Ok(if !one(&parts, &self.segments, false, meter)? {
            0
        } else if self.directories_only {
            DIRECTORY
        } else {
            DIRECTORY | FILE
        })
    }

    fn partial(&self, item: &str, meter: &Meter<'_>) -> Result<bool, String> {
        if item.len() > TEXT_LIMIT {
            return Err(too_long("a path"));
        }
        if item == "/" {
            return Ok(true);
        }
        let parts: Vec<&str> = item.split('/').collect();
        one(&parts, &self.segments, true, meter)
    }
}

fn too_long(what: &str) -> String {
    format!("{what} is longer than {TEXT_LIMIT} bytes, which `hashFiles` does not match")
}

fn one(
    parts: &[&str],
    segments: &[Segment],
    partial: bool,
    meter: &Meter<'_>,
) -> Result<bool, String> {
    let last = parts.len();
    let mut now = vec![false; last + 1];
    let mut next = vec![false; last + 1];
    now[0] = true;
    for (at, segment) in segments.iter().enumerate() {
        next.fill(false);
        let mut star = None;
        for (position, reached) in now.iter().enumerate() {
            if !reached {
                continue;
            }
            meter.spend(1)?;
            if position == last {
                if partial {
                    return Ok(true);
                }
                continue;
            }
            match segment {
                Segment::Globstar => {
                    if partial || at + 1 == segments.len() {
                        return Ok(true);
                    }
                    star.get_or_insert(position);
                }
                Segment::Literal(text) => {
                    if parts[position] == text {
                        next[position + 1] = true;
                    }
                }
                Segment::Glob(tokens) => {
                    if glob(parts[position], tokens, meter)? {
                        next[position + 1] = true;
                    }
                }
            }
        }
        if let Some(from) = star {
            next[from..last].fill(true);
        }
        std::mem::swap(&mut now, &mut next);
        if !now.contains(&true) {
            return Ok(false);
        }
    }
    Ok(now[last] || (last > 0 && now[last - 1] && parts[last - 1].is_empty()))
}

fn glob(name: &str, tokens: &[Token], meter: &Meter<'_>) -> Result<bool, String> {
    let width = tokens.len() + 1;
    let mut now = vec![false; width];
    let mut next = vec![false; width];
    now[0] = true;
    close(&mut now, tokens);
    for c in name.chars() {
        meter.spend(width)?;
        next.fill(false);
        for (index, token) in tokens.iter().enumerate() {
            if !now[index] {
                continue;
            }
            let fits = match token {
                Token::Any => {
                    next[index] = true;
                    false
                }
                Token::Char(expected) => c == *expected,
                Token::One => true,
                Token::Class { negated, ranges } => {
                    ranges.iter().any(|(low, high)| *low <= c && c <= *high) != *negated
                }
            };
            if fits {
                next[index + 1] = true;
            }
        }
        close(&mut next, tokens);
        std::mem::swap(&mut now, &mut next);
        if !now.contains(&true) {
            return Ok(false);
        }
    }
    Ok(now[tokens.len()])
}

fn close(states: &mut [bool], tokens: &[Token]) {
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token, Token::Any) && states[index] {
            states[index + 1] = true;
        }
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

    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::{
        LIMITS, Limits, Meter, Pattern, compile, files, glob, one, patterns, search_paths,
    };
    use crate::safe;

    fn matches(pattern: &str, name: &str) -> bool {
        match compile(pattern) {
            super::Segment::Glob(tokens) => {
                glob(name, &tokens, &Meter::new(&|| None)).expect("a match")
            }
            super::Segment::Literal(text) => text == name,
            super::Segment::Globstar => true,
        }
    }

    fn degree(pattern: &Pattern, item: &str) -> u8 {
        pattern
            .matches(item, &Meter::new(&|| None))
            .expect("a match")
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
            search_paths(workspace, &found),
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
        assert_eq!(degree(&written, "/w/a"), 3);
        assert_eq!(degree(&written, "/w/a/b/c"), 3);
        assert_eq!(degree(&written, "/w/b"), 0);
        let implied = &patterns(workspace, &["a".to_owned()]).expect("patterns")[1];
        assert_eq!(degree(implied, "/w/a"), 0);
        assert_eq!(degree(implied, "/w/a/b"), 3);
        let parts: Vec<&str> = "/w".split('/').collect();
        assert_eq!(
            one(&parts, &written.segments, true, &Meter::new(&|| None)),
            Ok(true)
        );
    }

    #[test]
    fn a_pipe_a_device_a_link_and_what_lies_behind_a_link_are_skipped_instead_of_read() {
        let directory = tempfile::tempdir().expect("a directory");
        let base = fs::canonicalize(directory.path()).expect("a path");
        let outside = base.join("outside");
        fs::create_dir_all(&outside).expect("a directory");
        fs::write(outside.join("secret"), "the credential of the agent").expect("a file");
        let plain = base.join("plain");
        let planted = base.join("planted");
        for workspace in [&plain, &planted] {
            fs::create_dir_all(workspace.join("src")).expect("directories");
            fs::write(workspace.join("src/lib.rs"), "fn main() {}").expect("a file");
            fs::write(workspace.join("Cargo.lock"), "lock").expect("a file");
        }
        assert!(
            std::process::Command::new("mkfifo")
                .arg(planted.join("src/pipe"))
                .status()
                .expect("mkfifo")
                .success()
        );
        symlink("/dev/zero", planted.join("src/zero")).expect("a link");
        symlink(outside.join("secret"), planted.join("src/secret")).expect("a link");
        symlink(&outside, planted.join("linked")).expect("a link");
        let hash = |workspace: std::path::PathBuf, pattern: &'static str| {
            safe::promptly(move || files(&workspace, &[pattern.to_owned()], &LIMITS, &|| None))
        };
        for pattern in ["**", "src/**\nCargo.lock", "linked/secret", "linked/**"] {
            let expected = hash(plain.clone(), pattern);
            let found = hash(planted.clone(), pattern);
            assert_eq!(found, expected, "{pattern}");
            assert!(
                pattern.starts_with("linked")
                    || expected.as_ref().is_ok_and(|hash| !hash.is_empty()),
                "{pattern}: {expected:?}"
            );
        }
        assert_eq!(hash(planted.clone(), "linked/secret"), Ok(String::new()));
    }

    fn tree(files: usize) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().expect("a directory");
        let workspace = fs::canonicalize(directory.path())
            .expect("a path")
            .join("workspace");
        fs::create_dir_all(&workspace).expect("a workspace");
        for index in 0..files {
            fs::write(workspace.join(format!("file-{index:03}")), "0123456789").expect("a file");
        }
        (directory, workspace)
    }

    #[test]
    fn a_pattern_that_cannot_match_below_the_workspace_is_not_walked_and_one_above_it_walks_only_it()
     {
        let (directory, workspace) = tree(3);
        let base = workspace.parent().expect("a parent").to_path_buf();
        let closed = base.join("closed");
        fs::create_dir_all(closed.join("inside")).expect("directories");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(closed.join("pipe"))
                .status()
                .expect("mkfifo")
                .success()
        );
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).expect("permissions");
        let hash = |pattern: String| {
            let workspace = workspace.clone();
            safe::promptly(move || files(&workspace, &[pattern], &LIMITS, &|| None))
        };
        let inside = hash("**".to_owned());
        assert!(inside.as_ref().is_ok_and(|hash| !hash.is_empty()));
        assert_eq!(hash(format!("{}/**", closed.display())), Ok(String::new()));
        assert_eq!(hash("/etc/**".to_owned()), Ok(String::new()));
        assert_eq!(hash(format!("{}/**", base.display())), inside);
        assert_eq!(hash("/**".to_owned()), inside);
        fs::set_permissions(&closed, fs::Permissions::from_mode(0o755)).expect("permissions");
        drop(directory);
    }

    #[test]
    fn more_files_or_bytes_than_the_limits_allow_fail_the_expression_instead_of_being_read() {
        let (_directory, workspace) = tree(5);
        let patterns = ["**".to_owned()];
        let few = files(
            &workspace,
            &patterns,
            &Limits {
                files: 4,
                bytes: 1000,
                entries: 100,
            },
            &|| None,
        );
        assert_eq!(
            few,
            Err("the patterns match more than 4 files, which `hashFiles` does not hash".to_owned())
        );
        let heavy = files(
            &workspace,
            &patterns,
            &Limits {
                files: 5,
                bytes: 49,
                entries: 100,
            },
            &|| None,
        );
        assert_eq!(
            heavy,
            Err(
                "the files the patterns match hold more than 49 bytes, which `hashFiles` does \
                 not read"
                    .to_owned()
            )
        );
        let exact = files(
            &workspace,
            &patterns,
            &Limits {
                files: 5,
                bytes: 50,
                entries: 100,
            },
            &|| None,
        );
        assert!(exact.is_ok_and(|hash| !hash.is_empty()));
    }

    #[test]
    fn a_cancel_or_a_deadline_stops_a_long_hash_between_two_files() {
        let (_directory, workspace) = tree(200);
        let checks = std::cell::Cell::new(0_u32);
        let halt = || {
            checks.set(checks.get() + 1);
            (checks.get() > 10).then_some("the job was cancelled")
        };
        let stopped = files(&workspace, &["**".to_owned()], &LIMITS, &halt);
        assert_eq!(
            stopped,
            Err("the job was cancelled, so the files were not hashed".to_owned())
        );
        assert!(checks.get() < 100, "{}", checks.get());
    }

    #[test]
    fn more_entries_than_the_limit_visits_fail_the_expression_instead_of_being_listed() {
        let (_directory, workspace) = tree(6);
        let limits = |entries| Limits {
            files: 100,
            bytes: 1000,
            entries,
        };
        let refused = files(&workspace, &["**".to_owned()], &limits(5), &|| None);
        assert_eq!(
            refused,
            Err(
                "the workspace holds more than 5 entries below the paths the patterns search, \
                 which `hashFiles` does not walk"
                    .to_owned()
            )
        );
        let accepted = files(&workspace, &["**".to_owned()], &limits(6), &|| None);
        assert!(accepted.is_ok_and(|hash| !hash.is_empty()));
    }

    #[test]
    fn a_pattern_built_to_backtrack_ends_at_once_on_a_long_name() {
        let (_directory, workspace) = tree(0);
        fs::write(workspace.join("a".repeat(255)), "x").expect("a file");
        let nested = workspace.join("a/".repeat(100));
        fs::create_dir_all(&nested).expect("directories");
        fs::write(nested.join("a"), "x").expect("a file");
        let hash = |pattern: &'static str| {
            let workspace = workspace.clone();
            safe::promptly(move || files(&workspace, &[pattern.to_owned()], &LIMITS, &|| None))
        };
        assert_eq!(hash("*a*a*a*a*a*a*a*a*a*a*a*a*b"), Ok(String::new()));
        assert_eq!(
            hash("**/a/**/a/**/a/**/a/**/a/**/a/**/a/**/b"),
            Ok(String::new())
        );
        assert!(hash("*a*a*a*a*a*a*a*a*a*a*a*a*").is_ok_and(|hash| !hash.is_empty()));
        assert!(hash("**/a/**/a/**/a/**/a/**/a/**/a/**/a/**/a").is_ok_and(|hash| !hash.is_empty()));
    }

    #[test]
    fn a_pattern_or_a_path_longer_than_the_limit_is_an_expression_error() {
        let (_directory, workspace) = tree(1);
        let pattern = format!("{}*", "a".repeat(4096));
        assert_eq!(
            files(&workspace, &[pattern], &LIMITS, &|| None),
            Err("a pattern is longer than 4096 bytes, which `hashFiles` does not match".to_owned())
        );
        let pattern = Pattern::new(&workspace, "**").expect("a pattern");
        let long = format!("/{}", "a/".repeat(2100));
        assert_eq!(
            pattern.matches(&long, &Meter::new(&|| None)),
            Err("a path is longer than 4096 bytes, which `hashFiles` does not match".to_owned())
        );
    }

    #[test]
    fn a_cancel_is_seen_in_the_middle_of_one_long_match() {
        let tokens: Vec<super::Token> = (0..2000)
            .flat_map(|_| [super::Token::Any, super::Token::Char('a')])
            .collect();
        let name = "a".repeat(4000);
        let checks = std::cell::Cell::new(0_u32);
        let halt = || {
            checks.set(checks.get() + 1);
            (checks.get() > 3).then_some("the job was cancelled")
        };
        let stopped = glob(&name, &tokens, &Meter::new(&halt));
        assert_eq!(
            stopped,
            Err("the job was cancelled, so the files were not hashed".to_owned())
        );
        assert_eq!(checks.get(), 4);
    }
}
