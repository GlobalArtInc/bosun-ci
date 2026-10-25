use bosun_types::ir::{Condition, Segment, Template};

use crate::error::{Code, Error};
use crate::parser;
use crate::source::Source;

enum Piece {
    Text(usize, usize),
    Expression {
        start: usize,
        inner: (usize, usize),
        end: usize,
    },
}

fn find_open(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len().saturating_sub(2))
        .find(|&at| chars[at] == '$' && chars[at + 1] == '{' && chars[at + 2] == '{')
}

fn split(source: &Source) -> Result<Vec<Piece>, Error> {
    let chars = source.chars();
    let mut pieces = Vec::new();
    let mut at = 0;
    while let Some(open) = find_open(chars, at) {
        if open > at {
            pieces.push(Piece::Text(at, open));
        }
        let inner = open + 3;
        let mut quoted = false;
        let mut close = None;
        for index in inner..chars.len() {
            if chars[index] == '\'' {
                quoted = !quoted;
            } else if !quoted && chars[index] == '}' && chars[index - 1] == '}' {
                close = Some(index);
                break;
            }
        }
        let Some(close) = close else {
            return Err(Error::new(
                Code::ExpressionSyntax,
                source.span(open, chars.len()),
                "`${{` is not closed by `}}`".to_string(),
            ));
        };
        pieces.push(Piece::Expression {
            start: open,
            inner: (inner, close - 1),
            end: close + 1,
        });
        at = close + 1;
    }
    if at < chars.len() {
        pieces.push(Piece::Text(at, chars.len()));
    }
    Ok(pieces)
}

fn segments(source: &Source, pieces: &[Piece]) -> Result<Template, Error> {
    let mut segments = Vec::new();
    for piece in pieces {
        segments.push(match *piece {
            Piece::Text(start, end) => Segment::Text(source.text(start, end)),
            Piece::Expression { start, inner, end } => {
                Segment::Expression(parser::parse(source, inner.0, inner.1, (start, end))?)
            }
        });
    }
    Ok(Template::new(segments))
}

pub(crate) fn template(text: &str) -> Result<Template, Error> {
    let source = Source::new(text);
    let pieces = split(&source)?;
    if pieces.iter().all(|piece| matches!(piece, Piece::Text(..))) {
        return Ok(Template::text(text));
    }
    segments(&source, &pieces)
}

pub(crate) fn condition(text: &str) -> Result<Option<Condition>, Error> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let source = Source::new(text);
    let pieces = split(&source)?;
    let blank = |piece: &Piece| match *piece {
        Piece::Text(start, end) => source.trimmed(start, end).0 == end,
        Piece::Expression { .. } => false,
    };
    let expressions: Vec<&Piece> = pieces
        .iter()
        .filter(|piece| matches!(piece, Piece::Expression { .. }))
        .collect();
    let condition = match expressions.as_slice() {
        [] => Condition::Expression(parser::parse(&source, 0, source.len(), (0, source.len()))?),
        [Piece::Expression { start, inner, end }]
            if pieces
                .iter()
                .all(|piece| matches!(piece, Piece::Expression { .. }) || blank(piece)) =>
        {
            Condition::Expression(parser::parse(&source, inner.0, inner.1, (*start, *end))?)
        }
        _ => Condition::Template(segments(&source, &pieces)?),
    };
    Ok(Some(condition))
}
