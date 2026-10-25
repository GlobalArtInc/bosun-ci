use bosun_types::ir::{Comparison, Expr, Number, Span};

use crate::error::{Code, Error};
use crate::lexer::{self, Kind, Token};
use crate::source::Source;

pub(crate) const MAX_DEPTH: u32 = 50;
pub(crate) const MAX_LENGTH: usize = 21_000;

struct Parsed {
    expr: Expr,
    start: usize,
    end: usize,
    depth: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Logic {
    And,
    Or,
}

#[derive(Clone, Copy)]
enum Operator {
    Logic(Logic),
    Compare(Comparison),
}

impl Operator {
    fn power(self) -> u8 {
        match self {
            Operator::Logic(Logic::Or) => 1,
            Operator::Logic(Logic::And) => 2,
            Operator::Compare(Comparison::Equal | Comparison::NotEqual) => 3,
            Operator::Compare(_) => 4,
        }
    }
}

struct Parser<'s> {
    source: &'s Source,
    tokens: Vec<Token>,
    at: usize,
    nesting: u32,
}

pub(crate) fn parse(
    source: &Source,
    start: usize,
    end: usize,
    whole: (usize, usize),
) -> Result<Expr, Error> {
    let (first, last) = source.trimmed(start, end);
    if first == last {
        return Err(Error::new(
            Code::ExpressionSyntax,
            source.span(whole.0, whole.1),
            "the expression is empty".to_string(),
        ));
    }
    if last - first > MAX_LENGTH {
        return Err(Error::new(
            Code::LimitExceeded,
            source.span(first, last),
            format!("the expression is longer than {MAX_LENGTH} characters"),
        ));
    }
    let mut parser = Parser {
        source,
        tokens: lexer::lex(source.chars(), first, last),
        at: 0,
        nesting: 0,
    };
    let parsed = parser.binary(0)?;
    match parser.tokens.get(parser.at) {
        Some(token) => Err(parser.unexpected(token)),
        None => Ok(parsed.expr),
    }
}

impl Parser<'_> {
    fn is(&self, kind: &Kind) -> bool {
        self.tokens.get(self.at).map(|token| &token.kind) == Some(kind)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).cloned();
        if token.is_some() {
            self.at += 1;
        }
        token
    }

    fn span(&self, start: usize, end: usize) -> Span {
        self.source.span(start, end)
    }

    fn syntax(&self, start: usize, end: usize, message: String) -> Error {
        Error::new(Code::ExpressionSyntax, self.span(start, end), message)
    }

    fn unexpected(&self, token: &Token) -> Error {
        let message = match token.kind {
            Kind::UnclosedString => "the string is not closed".to_string(),
            _ => format!("unexpected `{}`", self.source.text(token.start, token.end)),
        };
        self.syntax(token.start, token.end, message)
    }

    fn ended(&self) -> Error {
        let (start, end) = self
            .tokens
            .last()
            .map_or((0, 0), |token| (token.start, token.end));
        let message = format!(
            "the expression ends after `{}`",
            self.source.text(start, end)
        );
        self.syntax(start, end, message)
    }

    fn unclosed(&self, start: usize, end: usize) -> Error {
        let message = format!("`{}` is not closed", self.source.text(start, end));
        self.syntax(start, end, message)
    }

    fn advance(&mut self) -> Result<Token, Error> {
        self.next().ok_or_else(|| self.ended())
    }

    fn closing(&mut self, open: &Token, kind: Kind) -> Result<Token, Error> {
        match self.next() {
            Some(token) if token.kind == kind => Ok(token),
            Some(token) => Err(self.unexpected(&token)),
            None => Err(self.unclosed(open.start, open.end)),
        }
    }

    fn too_deep(&self, start: usize, end: usize) -> Error {
        Error::new(
            Code::LimitExceeded,
            self.span(start, end),
            format!("the expression nests deeper than {MAX_DEPTH} levels"),
        )
    }

    fn deeper(&self, depth: u32, start: usize, end: usize) -> Result<u32, Error> {
        if depth > MAX_DEPTH {
            return Err(self.too_deep(start, end));
        }
        Ok(depth)
    }

    fn operator(&self) -> Option<Operator> {
        match &self.tokens.get(self.at)?.kind {
            Kind::Compare(comparison) => Some(Operator::Compare(*comparison)),
            Kind::And => Some(Operator::Logic(Logic::And)),
            Kind::Or => Some(Operator::Logic(Logic::Or)),
            _ => None,
        }
    }

    fn binary(&mut self, min: u8) -> Result<Parsed, Error> {
        let mut left = self.unary()?;
        let mut chain = None;
        while let Some(operator) = self.operator() {
            let power = operator.power();
            if power < min {
                break;
            }
            self.at += 1;
            let right = self.binary(power + 1)?;
            left = match operator {
                Operator::Compare(comparison) => {
                    chain = None;
                    self.compare(comparison, left, right)?
                }
                Operator::Logic(logic) => {
                    let extend = chain == Some(logic);
                    chain = Some(logic);
                    self.logic(logic, left, right, extend)?
                }
            };
        }
        Ok(left)
    }

    fn compare(&self, operator: Comparison, left: Parsed, right: Parsed) -> Result<Parsed, Error> {
        let (start, end) = (left.start, right.end);
        let depth = self.deeper(1 + left.depth.max(right.depth), start, end)?;
        Ok(Parsed {
            expr: Expr::Compare {
                operator,
                left: Box::new(left.expr),
                right: Box::new(right.expr),
                span: self.span(start, end),
            },
            start,
            end,
            depth,
        })
    }

    fn logic(
        &self,
        logic: Logic,
        left: Parsed,
        right: Parsed,
        extend: bool,
    ) -> Result<Parsed, Error> {
        let (start, end) = (left.start, right.end);
        let span = self.span(start, end);
        if extend {
            let depth = self.deeper(left.depth.max(1 + right.depth), start, end)?;
            let mut expr = left.expr;
            if let Expr::And {
                operands,
                span: whole,
            }
            | Expr::Or {
                operands,
                span: whole,
            } = &mut expr
            {
                operands.push(right.expr);
                *whole = span;
            }
            return Ok(Parsed {
                expr,
                start,
                end,
                depth,
            });
        }
        let depth = self.deeper(1 + left.depth.max(right.depth), start, end)?;
        let operands = vec![left.expr, right.expr];
        let expr = match logic {
            Logic::And => Expr::And { operands, span },
            Logic::Or => Expr::Or { operands, span },
        };
        Ok(Parsed {
            expr,
            start,
            end,
            depth,
        })
    }

    fn unary(&mut self) -> Result<Parsed, Error> {
        self.nesting += 1;
        if self.nesting > MAX_DEPTH {
            let (start, end) = self
                .tokens
                .get(self.at)
                .or(self.tokens.last())
                .map_or((0, 0), |token| (token.start, token.end));
            return Err(self.too_deep(start, end));
        }
        let parsed = if self.is(&Kind::Not) {
            let token = self.advance()?;
            let operand = self.unary()?;
            let (start, end) = (token.start, operand.end);
            let depth = self.deeper(operand.depth + 1, start, end)?;
            Parsed {
                expr: Expr::Not {
                    operand: Box::new(operand.expr),
                    span: self.span(start, end),
                },
                start,
                end,
                depth,
            }
        } else {
            self.postfix()?
        };
        self.nesting -= 1;
        Ok(parsed)
    }

    fn postfix(&mut self) -> Result<Parsed, Error> {
        let (mut current, accessible) = self.primary()?;
        if !accessible {
            return Ok(current);
        }
        loop {
            let dot = self.is(&Kind::Dot);
            if !dot && !self.is(&Kind::LeftBracket) {
                return Ok(current);
            }
            let start = current.start;
            let target = Box::new(current.expr);
            let mut depth = current.depth;
            let (expr, end) = if dot {
                self.at += 1;
                let token = self.advance()?;
                match token.kind {
                    Kind::Name(name) => (
                        Expr::Property {
                            target,
                            name,
                            span: self.span(start, token.end),
                        },
                        token.end,
                    ),
                    Kind::Star => (
                        Expr::Filter {
                            target,
                            span: self.span(start, token.end),
                        },
                        token.end,
                    ),
                    _ => return Err(self.unexpected(&token)),
                }
            } else {
                let open = self.advance()?;
                let star = self.is(&Kind::Star)
                    && self.tokens.get(self.at + 1).map(|token| &token.kind)
                        == Some(&Kind::RightBracket);
                if star {
                    self.at += 1;
                    let close = self.advance()?;
                    (
                        Expr::Filter {
                            target,
                            span: self.span(start, close.end),
                        },
                        close.end,
                    )
                } else {
                    let index = self.binary(0)?;
                    let close = self.closing(&open, Kind::RightBracket)?;
                    depth = depth.max(index.depth);
                    (
                        Expr::Index {
                            target,
                            index: Box::new(index.expr),
                            span: self.span(start, close.end),
                        },
                        close.end,
                    )
                }
            };
            let depth = self.deeper(depth + 1, start, end)?;
            current = Parsed {
                expr,
                start,
                end,
                depth,
            };
        }
    }

    fn primary(&mut self) -> Result<(Parsed, bool), Error> {
        let Token { kind, start, end } = self.advance()?;
        let span = self.span(start, end);
        let parsed = |expr| Parsed {
            expr,
            start,
            end,
            depth: 1,
        };
        match kind {
            Kind::Null => Ok((parsed(Expr::Null { span }), false)),
            Kind::Boolean(value) => Ok((parsed(Expr::Boolean { value, span }), false)),
            Kind::Number(value) => Ok((
                parsed(Expr::Number {
                    value: Number(value),
                    span,
                }),
                false,
            )),
            Kind::String(value) => Ok((parsed(Expr::String { value, span }), false)),
            Kind::Name(function) if self.is(&Kind::LeftParen) => {
                Ok((self.call(function, start)?, true))
            }
            Kind::Name(name) => Ok((parsed(Expr::Context { name, span }), true)),
            Kind::LeftParen => {
                let open = Token {
                    kind: Kind::LeftParen,
                    start,
                    end,
                };
                let inner = self.binary(0)?;
                let close = self.closing(&open, Kind::RightParen)?;
                let depth = self.deeper(inner.depth + 1, start, close.end)?;
                Ok((
                    Parsed {
                        expr: inner.expr,
                        start,
                        end: close.end,
                        depth,
                    },
                    true,
                ))
            }
            kind => Err(self.unexpected(&Token { kind, start, end })),
        }
    }

    fn call(&mut self, function: String, start: usize) -> Result<Parsed, Error> {
        let open = self.advance()?;
        let mut arguments = Vec::new();
        let mut depth = 0;
        let close = if self.is(&Kind::RightParen) {
            self.advance()?
        } else {
            loop {
                let argument = self.binary(0)?;
                depth = depth.max(argument.depth);
                arguments.push(argument.expr);
                match self.next() {
                    Some(token) if token.kind == Kind::Comma => {}
                    Some(token) if token.kind == Kind::RightParen => break token,
                    Some(token) => return Err(self.unexpected(&token)),
                    None => return Err(self.unclosed(open.start, open.end)),
                }
            }
        };
        let end = close.end;
        let depth = self.deeper(depth + 1, start, end)?;
        Ok(Parsed {
            expr: Expr::Call {
                function,
                arguments,
                span: self.span(start, end),
            },
            start,
            end,
            depth,
        })
    }
}
