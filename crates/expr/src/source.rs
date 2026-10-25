use bosun_types::ir::Span;

pub(crate) struct Source {
    chars: Vec<char>,
    lines: Vec<usize>,
}

fn number(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

impl Source {
    pub(crate) fn new(text: &str) -> Source {
        let chars: Vec<char> = text.chars().collect();
        let mut lines = vec![0];
        for (index, &c) in chars.iter().enumerate() {
            if c == '\n' {
                lines.push(index + 1);
            }
        }
        Source { chars, lines }
    }

    pub(crate) fn chars(&self) -> &[char] {
        &self.chars
    }

    pub(crate) fn len(&self) -> usize {
        self.chars.len()
    }

    pub(crate) fn text(&self, start: usize, end: usize) -> String {
        self.chars[start..end].iter().collect()
    }

    fn position(&self, index: usize) -> (u32, u32) {
        let line = self.lines.partition_point(|&start| start <= index);
        let column = index - self.lines[line - 1] + 1;
        (number(line), number(column))
    }

    pub(crate) fn span(&self, start: usize, end: usize) -> Span {
        let (line, column) = self.position(start);
        let (end_line, end_column) = self.position(end);
        Span {
            line,
            column,
            end_line,
            end_column,
        }
    }

    pub(crate) fn trimmed(&self, start: usize, end: usize) -> (usize, usize) {
        let mut first = start;
        while first < end && self.chars[first].is_whitespace() {
            first += 1;
        }
        let mut last = end;
        while last > first && self.chars[last - 1].is_whitespace() {
            last -= 1;
        }
        (first, last)
    }
}
