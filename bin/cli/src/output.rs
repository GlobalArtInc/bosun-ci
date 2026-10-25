use serde::Serialize;
use serde_json::Value;

use crate::client::Failure;

pub(crate) fn name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        Ok(other) => other.to_string(),
        Err(error) => format!("<{error}>"),
    }
}

pub(crate) fn json<T: Serialize>(value: &T) -> Result<(), Failure> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| Failure::new(format!("the answer cannot be printed as JSON: {error}")))?;
    println!("{text}");
    Ok(())
}

pub(crate) fn table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| header.chars().count())
        .collect();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let headers: Vec<String> = headers.iter().map(|header| (*header).to_owned()).collect();
    for row in std::iter::once(&headers).chain(rows) {
        let mut line = String::new();
        for (index, (cell, width)) in row.iter().zip(&widths).enumerate() {
            if index + 1 == row.len() {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  "));
            }
        }
        println!("{}", line.trim_end());
    }
}

pub(crate) fn fields(rows: &[(&str, String)]) {
    let width = rows
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    for (name, value) in rows {
        let line = format!("{name:<width$}  {value}");
        println!("{}", line.trim_end());
    }
}

pub(crate) fn text(line: &str) -> &str {
    line.split_once(' ').map_or(line, |(_, text)| text)
}

pub(crate) fn failure(failure: &Failure) {
    eprintln!("error: {}", failure.message);
    for detail in &failure.details {
        eprintln!("  {detail}");
    }
}

#[cfg(test)]
mod tests {
    use bosun_types::status::{JobStatus, RunConclusion};

    use super::{name, text};

    #[test]
    fn names_are_the_words_of_the_api() {
        assert_eq!(name(&JobStatus::InProgress), "in_progress");
        assert_eq!(name(&RunConclusion::StartupFailure), "startup_failure");
    }

    #[test]
    fn a_log_line_reads_without_its_time() {
        assert_eq!(text("2026-10-24T18:34:51.5Z line 1 of 2"), "line 1 of 2");
        assert_eq!(text("2026-10-24T18:34:51.5Z "), "");
        assert_eq!(text("bare"), "bare");
    }
}
