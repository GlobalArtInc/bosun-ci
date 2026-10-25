use bosun_types::ir::{Expr, Segment, Span, Template};

pub(crate) struct Text {
    chars: Vec<char>,
    lines: Vec<usize>,
}

pub(crate) struct Placement {
    lines: Vec<usize>,
    positions: Vec<(u32, u32)>,
    start: (u32, u32),
}

fn narrow(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn escape(chars: &[char]) -> usize {
    match chars.get(1) {
        Some('x') => 4,
        Some('u') => 6,
        Some('U') => 10,
        Some('\n' | '\r') | None => 0,
        Some(_) => 2,
    }
}

impl Text {
    pub(crate) fn new(source: &str) -> Text {
        let chars: Vec<char> = source.chars().collect();
        let mut lines = vec![0];
        for (index, &c) in chars.iter().enumerate() {
            if c == '\n' || (c == '\r' && chars.get(index + 1) != Some(&'\n')) {
                lines.push(index + 1);
            }
        }
        Text { chars, lines }
    }

    fn offset(&self, line: u32, column: u32) -> usize {
        let line = usize::try_from(line)
            .unwrap_or(usize::MAX)
            .saturating_sub(1);
        let column = usize::try_from(column)
            .unwrap_or(usize::MAX)
            .saturating_sub(1);
        self.lines
            .get(line)
            .map_or(self.chars.len(), |start| start.saturating_add(column))
            .min(self.chars.len())
    }

    fn position(&self, offset: usize) -> (u32, u32) {
        let line = self.lines.partition_point(|&start| start <= offset);
        let column = offset - self.lines[line - 1] + 1;
        (narrow(line), narrow(column))
    }

    pub(crate) fn place(&self, node: Span, value: &str) -> Placement {
        let mut at = self.offset(node.line, node.column);
        let end = self.offset(node.end_line, node.end_column).max(at);
        match self.chars.get(at) {
            Some('\'' | '"') => at += 1,
            Some('|' | '>') => {
                while at < end && self.chars[at] != '\n' {
                    at += 1;
                }
                at += 1;
            }
            _ => {}
        }
        let mut at = at.min(end);
        let start = self.position(at);
        let mut lines = vec![0];
        let mut positions = Vec::new();
        for (index, c) in value.chars().enumerate() {
            if c == '\n' {
                lines.push(index + 1);
            }
            loop {
                let Some(&found) = self.chars.get(at).filter(|_| at < end) else {
                    positions.push(self.position(end));
                    break;
                };
                if found == c {
                    positions.push(self.position(at));
                    at += 1;
                    break;
                }
                if found == '\\' {
                    let length = escape(&self.chars[at..end]);
                    if length > 0 {
                        positions.push(self.position(at));
                        at = (at + length).min(end);
                        break;
                    }
                    at += 1;
                } else if found.is_whitespace() || found == '\'' || found == '"' {
                    at += 1;
                } else {
                    positions.push(self.position(at));
                    break;
                }
            }
        }
        Placement {
            lines,
            positions,
            start,
        }
    }
}

impl Placement {
    fn index(&self, line: u32, column: u32) -> usize {
        let line = usize::try_from(line)
            .unwrap_or(usize::MAX)
            .saturating_sub(1);
        let column = usize::try_from(column)
            .unwrap_or(usize::MAX)
            .saturating_sub(1);
        self.lines
            .get(line)
            .map_or(self.positions.len(), |start| start.saturating_add(column))
            .min(self.positions.len())
    }

    fn after(&self, index: usize) -> (u32, u32) {
        match index
            .checked_sub(1)
            .and_then(|last| self.positions.get(last))
        {
            Some(&(line, column)) => (line, column.saturating_add(1)),
            None => self.start,
        }
    }

    pub(crate) fn span(&self, span: Span) -> Span {
        let start = self.index(span.line, span.column);
        let end = self.index(span.end_line, span.end_column).max(start);
        let (line, column) = self
            .positions
            .get(start)
            .copied()
            .unwrap_or_else(|| self.after(start));
        let (end_line, end_column) = self.after(end).max((line, column));
        Span {
            line,
            column,
            end_line,
            end_column,
        }
    }

    pub(crate) fn expression(&self, expr: &mut Expr) {
        match expr {
            Expr::Null { span }
            | Expr::Boolean { span, .. }
            | Expr::Number { span, .. }
            | Expr::String { span, .. }
            | Expr::Context { span, .. } => *span = self.span(*span),
            Expr::Property { target, span, .. } | Expr::Filter { target, span } => {
                *span = self.span(*span);
                self.expression(target);
            }
            Expr::Index {
                target,
                index,
                span,
            } => {
                *span = self.span(*span);
                self.expression(target);
                self.expression(index);
            }
            Expr::Not { operand, span } => {
                *span = self.span(*span);
                self.expression(operand);
            }
            Expr::Compare {
                left, right, span, ..
            } => {
                *span = self.span(*span);
                self.expression(left);
                self.expression(right);
            }
            Expr::And { operands, span } | Expr::Or { operands, span } => {
                *span = self.span(*span);
                for operand in operands {
                    self.expression(operand);
                }
            }
            Expr::Call {
                arguments, span, ..
            } => {
                *span = self.span(*span);
                for argument in arguments {
                    self.expression(argument);
                }
            }
        }
    }

    pub(crate) fn template(&self, template: &Template) -> Template {
        Template::new(
            template
                .segments()
                .iter()
                .cloned()
                .map(|segment| match segment {
                    Segment::Expression(mut expr) => {
                        self.expression(&mut expr);
                        Segment::Expression(expr)
                    }
                    text => text,
                })
                .collect(),
        )
    }
}
