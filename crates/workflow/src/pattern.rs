#[derive(Clone, Copy, PartialEq, Eq)]
enum Repeat {
    One,
    Optional,
    OneOrMore,
}

enum Unit {
    Char(char),
    Class(Vec<(char, char)>),
}

enum Token {
    Unit(Unit, Repeat),
    Segment,
    Anything,
    Directories,
}

pub(crate) struct Glob {
    tokens: Vec<Token>,
}

pub(crate) struct Patterns {
    globs: Vec<(bool, Option<Glob>)>,
}

pub(crate) fn check(pattern: &str) -> Result<(), String> {
    match pattern.strip_prefix('!') {
        Some("") => Err("`!` is followed by no pattern".to_string()),
        Some(rest) => Glob::new(rest).map(drop),
        None if pattern.is_empty() => Err("the pattern is empty".to_string()),
        None => Glob::new(pattern).map(drop),
    }
}

impl Patterns {
    pub(crate) fn new(patterns: &[String]) -> Patterns {
        Patterns {
            globs: patterns
                .iter()
                .map(|pattern| match pattern.strip_prefix('!') {
                    Some(rest) => (true, Glob::new(rest).ok()),
                    None => (false, Glob::new(pattern).ok()),
                })
                .collect(),
        }
    }

    pub(crate) fn select(&self, name: &str) -> bool {
        let chars: Vec<char> = name.chars().collect();
        let mut selected = false;
        for (negated, glob) in &self.globs {
            if glob.as_ref().is_some_and(|glob| glob.matches(&chars)) {
                selected = !negated;
            }
        }
        selected
    }
}

impl Unit {
    fn accepts(&self, c: char) -> bool {
        match self {
            Unit::Char(expected) => *expected == c,
            Unit::Class(ranges) => ranges.iter().any(|&(low, high)| low <= c && c <= high),
        }
    }
}

fn kind(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(0),
        'A'..='Z' => Some(1),
        '0'..='9' => Some(2),
        _ => None,
    }
}

fn class(chars: &[char], start: usize) -> Result<(Unit, usize), String> {
    let mut ranges = Vec::new();
    let mut at = start;
    loop {
        match chars.get(at) {
            None => return Err("`[` is not closed by `]`".to_string()),
            Some(']') if at == start => return Err("`[]` lists no characters".to_string()),
            Some(']') => return Ok((Unit::Class(ranges), at + 1)),
            Some(&low) => {
                if kind(low).is_none() {
                    return Err(format!(
                        "`{low}` inside `[ ]` is neither a letter nor a digit"
                    ));
                }
                if chars.get(at + 1) == Some(&'-') {
                    let high = chars.get(at + 2).copied().filter(|&high| high != ']');
                    match high {
                        Some(high) if kind(high) == kind(low) && low <= high => {
                            ranges.push((low, high));
                            at += 3;
                        }
                        _ => {
                            return Err(format!(
                                "the range starting at `{low}` inside `[ ]` must end at a later character of a-z, A-Z or 0-9"
                            ));
                        }
                    }
                } else {
                    ranges.push((low, low));
                    at += 1;
                }
            }
        }
    }
}

impl Glob {
    pub(crate) fn new(pattern: &str) -> Result<Glob, String> {
        let chars: Vec<char> = pattern.chars().collect();
        let mut tokens: Vec<Token> = Vec::new();
        let mut at = 0;
        while let Some(&c) = chars.get(at) {
            match c {
                '*' if chars.get(at + 1) == Some(&'*') => {
                    if chars.get(at + 2) == Some(&'/') {
                        tokens.push(Token::Directories);
                        at += 3;
                    } else {
                        tokens.push(Token::Anything);
                        at += 2;
                    }
                }
                '*' => {
                    tokens.push(Token::Segment);
                    at += 1;
                }
                '?' | '+' if !tokens.is_empty() => {
                    match tokens.last_mut() {
                        Some(Token::Unit(_, repeat)) if *repeat == Repeat::One => {
                            *repeat = if c == '?' {
                                Repeat::Optional
                            } else {
                                Repeat::OneOrMore
                            };
                        }
                        _ if c == '?' => {}
                        _ => {
                            return Err(
                                "`+` must follow a character or a `[ ]` that repeats nothing yet"
                                    .to_string(),
                            );
                        }
                    }
                    at += 1;
                }
                '[' => {
                    let (unit, next) = class(&chars, at + 1)?;
                    tokens.push(Token::Unit(unit, Repeat::One));
                    at = next;
                }
                '\\' => {
                    let Some(&escaped) = chars.get(at + 1) else {
                        return Err("`\\` at the end escapes nothing".to_string());
                    };
                    tokens.push(Token::Unit(Unit::Char(escaped), Repeat::One));
                    at += 2;
                }
                _ => {
                    tokens.push(Token::Unit(Unit::Char(c), Repeat::One));
                    at += 1;
                }
            }
        }
        Ok(Glob { tokens })
    }

    fn matches(&self, name: &[char]) -> bool {
        let width = name.len() + 1;
        let mut memo = vec![None; (self.tokens.len() + 1) * width];
        self.from(0, 0, name, &mut memo)
    }

    fn from(&self, token: usize, at: usize, name: &[char], memo: &mut [Option<bool>]) -> bool {
        let slot = token * (name.len() + 1) + at;
        if let Some(known) = memo[slot] {
            return known;
        }
        let result = match self.tokens.get(token) {
            None => at == name.len(),
            Some(Token::Unit(unit, repeat)) => {
                let first = name.get(at).is_some_and(|&c| unit.accepts(c));
                match repeat {
                    Repeat::One => first && self.from(token + 1, at + 1, name, memo),
                    Repeat::Optional => {
                        self.from(token + 1, at, name, memo)
                            || (first && self.from(token + 1, at + 1, name, memo))
                    }
                    Repeat::OneOrMore => {
                        let mut end = at;
                        let mut found = false;
                        while !found && name.get(end).is_some_and(|&c| unit.accepts(c)) {
                            end += 1;
                            found = self.from(token + 1, end, name, memo);
                        }
                        found
                    }
                }
            }
            Some(Token::Segment) => {
                let mut end = at;
                loop {
                    if self.from(token + 1, end, name, memo) {
                        break true;
                    }
                    match name.get(end) {
                        Some(&c) if c != '/' => end += 1,
                        _ => break false,
                    }
                }
            }
            Some(Token::Anything) => {
                (at..=name.len()).any(|end| self.from(token + 1, end, name, memo))
            }
            Some(Token::Directories) => {
                self.from(token + 1, at, name, memo)
                    || (at + 1..name.len()).any(|slash| {
                        name[slash] == '/' && self.from(token + 1, slash + 1, name, memo)
                    })
            }
        };
        memo[slot] = Some(result);
        result
    }
}
