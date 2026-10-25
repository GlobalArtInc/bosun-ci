use bosun_types::ir::Comparison;

use crate::number;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kind {
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    Comma,
    Dot,
    Star,
    Not,
    Compare(Comparison),
    And,
    Or,
    Null,
    Boolean(bool),
    Number(f64),
    String(String),
    Name(String),
    UnclosedString,
    Unexpected,
}

#[derive(Debug, Clone)]
pub(crate) struct Token {
    pub(crate) kind: Kind,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

fn boundary(c: char) -> bool {
    matches!(
        c,
        '(' | '[' | ')' | ']' | ',' | '.' | '!' | '>' | '<' | '=' | '&' | '|'
    ) || c.is_whitespace()
}

fn legal(word: &[char]) -> bool {
    match word.split_first() {
        Some((first, rest)) => {
            (first.is_ascii_alphabetic() || *first == '_')
                && rest
                    .iter()
                    .all(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        }
        None => false,
    }
}

fn starts_number(previous: Option<&Kind>) -> bool {
    matches!(
        previous,
        None | Some(
            Kind::Comma
                | Kind::LeftParen
                | Kind::LeftBracket
                | Kind::Not
                | Kind::Compare(_)
                | Kind::And
                | Kind::Or
        )
    )
}

struct Lexer<'c> {
    chars: &'c [char],
    at: usize,
    end: usize,
}

impl Lexer<'_> {
    fn peek(&self, offset: usize) -> Option<char> {
        let index = self.at + offset;
        (index < self.end).then(|| self.chars[index])
    }

    fn skip_word(&mut self) {
        while self.peek(0).is_some_and(|c| !boundary(c)) {
            self.at += 1;
        }
    }

    fn string(&mut self) -> Kind {
        self.at += 1;
        let mut value = String::new();
        while let Some(c) = self.peek(0) {
            self.at += 1;
            if c == '\'' {
                if self.peek(0) == Some('\'') {
                    self.at += 1;
                } else {
                    return Kind::String(value);
                }
            }
            value.push(c);
        }
        Kind::UnclosedString
    }

    fn operator(&mut self, first: char) -> Kind {
        let pair = match (first, self.peek(1)) {
            ('!', Some('=')) => Some(Kind::Compare(Comparison::NotEqual)),
            ('>', Some('=')) => Some(Kind::Compare(Comparison::GreaterOrEqual)),
            ('<', Some('=')) => Some(Kind::Compare(Comparison::LessOrEqual)),
            ('=', Some('=')) => Some(Kind::Compare(Comparison::Equal)),
            ('&', Some('&')) => Some(Kind::And),
            ('|', Some('|')) => Some(Kind::Or),
            _ => None,
        };
        if let Some(kind) = pair {
            self.at += 2;
            return kind;
        }
        self.at += 1;
        match first {
            '!' => Kind::Not,
            '>' => Kind::Compare(Comparison::Greater),
            '<' => Kind::Compare(Comparison::Less),
            _ => {
                self.skip_word();
                Kind::Unexpected
            }
        }
    }

    fn number(&mut self, start: usize) -> Kind {
        self.at += 1;
        while self.peek(0).is_some_and(|c| !boundary(c) || c == '.') {
            self.at += 1;
        }
        let text: String = self.chars[start..self.at].iter().collect();
        match number::parse(&text) {
            Some(value) if !value.is_nan() => Kind::Number(value),
            _ => Kind::Unexpected,
        }
    }

    fn word(&mut self, start: usize, previous: Option<&Kind>) -> Kind {
        self.at += 1;
        self.skip_word();
        let word = &self.chars[start..self.at];
        if !legal(word) {
            return Kind::Unexpected;
        }
        let text: String = word.iter().collect();
        if previous == Some(&Kind::Dot) {
            return Kind::Name(text);
        }
        match text.to_ascii_lowercase().as_str() {
            "true" => Kind::Boolean(true),
            "false" => Kind::Boolean(false),
            "null" => Kind::Null,
            "nan" => Kind::Number(f64::NAN),
            "infinity" => Kind::Number(f64::INFINITY),
            _ => Kind::Name(text),
        }
    }
}

pub(crate) fn lex(chars: &[char], start: usize, end: usize) -> Vec<Token> {
    let mut lexer = Lexer {
        chars,
        at: start,
        end,
    };
    let mut tokens: Vec<Token> = Vec::new();
    while let Some(c) = lexer.peek(0) {
        if c.is_whitespace() {
            lexer.at += 1;
            continue;
        }
        let start = lexer.at;
        let previous = tokens.last().map(|token| &token.kind);
        let single = match c {
            '(' => Some(Kind::LeftParen),
            ')' => Some(Kind::RightParen),
            '[' => Some(Kind::LeftBracket),
            ']' => Some(Kind::RightBracket),
            ',' => Some(Kind::Comma),
            '*' => Some(Kind::Star),
            '.' if !starts_number(previous) => Some(Kind::Dot),
            _ => None,
        };
        let kind = match single {
            Some(kind) => {
                lexer.at += 1;
                kind
            }
            None => match c {
                '\'' => lexer.string(),
                '!' | '>' | '<' | '=' | '&' | '|' => lexer.operator(c),
                '.' | '-' | '+' | '0'..='9' => lexer.number(start),
                _ => lexer.word(start, previous),
            },
        };
        tokens.push(Token {
            kind,
            start,
            end: lexer.at,
        });
    }
    tokens
}
