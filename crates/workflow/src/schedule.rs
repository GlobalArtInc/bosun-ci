use std::collections::BTreeSet;
use std::str::FromStr;

use bosun_types::ir::{Cron, CronDays};
use chrono_tz::{TZ_VARIANTS, Tz};

use crate::surface::suggest;

struct Field {
    name: &'static str,
    low: u32,
    high: u32,
    names: &'static [&'static str],
    first_name: u32,
    period: Option<u32>,
}

const MINUTE: Field = Field {
    name: "minute",
    low: 0,
    high: 59,
    names: &[],
    first_name: 0,
    period: None,
};

const HOUR: Field = Field {
    name: "hour",
    low: 0,
    high: 23,
    names: &[],
    first_name: 0,
    period: None,
};

const DAY_OF_MONTH: Field = Field {
    name: "day of the month",
    low: 1,
    high: 31,
    names: &[],
    first_name: 0,
    period: None,
};

const MONTH: Field = Field {
    name: "month",
    low: 1,
    high: 12,
    names: &[
        "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
    ],
    first_name: 1,
    period: None,
};

const DAY_OF_WEEK: Field = Field {
    name: "day of the week",
    low: 0,
    high: 7,
    names: &["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"],
    first_name: 0,
    period: Some(7),
};

fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some(text.parse::<u32>().unwrap_or(u32::MAX))
}

impl Field {
    fn value(&self, text: &str) -> Result<u32, String> {
        if let Some(value) = number(text) {
            if (self.low..=self.high).contains(&value) {
                return Ok(value);
            }
            return Err(format!(
                "`{text}` is outside the range of the {}, {}-{}",
                self.name, self.low, self.high
            ));
        }
        let upper = text.to_ascii_uppercase();
        match self.names.iter().position(|name| *name == upper) {
            Some(position) => Ok(self.first_name + u32::try_from(position).unwrap_or(0)),
            None if self.names.is_empty() => {
                Err(format!("`{text}` in the {} is not a number", self.name))
            }
            None => Err(format!(
                "`{text}` in the {} is neither a number nor one of {}",
                self.name,
                self.names.join(", ")
            )),
        }
    }

    fn item(&self, item: &str, values: &mut BTreeSet<u32>) -> Result<(), String> {
        if item.is_empty() {
            return Err(format!("the {} holds an empty item", self.name));
        }
        let (range, step) = match item.split_once('/') {
            None => (item, None),
            Some((range, step)) => match number(step) {
                Some(step) if step > 0 => (range, Some(step)),
                _ => {
                    return Err(format!(
                        "the step of `{item}` in the {} is not a positive number",
                        self.name
                    ));
                }
            },
        };
        let (low, high) = if range == "*" {
            (self.low, self.high)
        } else if let Some((low, high)) = range.split_once('-') {
            (self.value(low)?, self.value(high)?)
        } else {
            let value = self.value(range)?;
            (value, if step.is_some() { self.high } else { value })
        };
        if low > high {
            return Err(format!(
                "the range `{range}` in the {} runs backwards",
                self.name
            ));
        }
        let step = usize::try_from(step.unwrap_or(1)).unwrap_or(usize::MAX);
        values.extend((low..=high).step_by(step));
        Ok(())
    }

    fn parse(&self, text: &str) -> Result<Vec<u8>, String> {
        let mut values = BTreeSet::new();
        for item in text.split(',') {
            self.item(item, &mut values)?;
        }
        let values: BTreeSet<u8> = values
            .into_iter()
            .map(|value| self.period.map_or(value, |period| value % period))
            .map(|value| u8::try_from(value).unwrap_or(u8::MAX))
            .collect();
        Ok(values.into_iter().collect())
    }
}

pub(crate) fn cron(expression: &str) -> Result<Cron, String> {
    if expression.trim_start().starts_with('@') {
        return Err(
            "shortcuts such as `@daily` are not supported; write the five fields".to_string(),
        );
    }
    let fields: Vec<&str> = expression.split_whitespace().collect();
    let [minute, hour, day_of_month, month, day_of_week] = fields[..] else {
        return Err(format!(
            "it has {} fields, and a cron has five: minute, hour, day of the month, month and day of the week",
            fields.len()
        ));
    };
    let days = if day_of_month.starts_with('*') || day_of_week.starts_with('*') {
        CronDays::Both
    } else {
        CronDays::Either
    };
    Ok(Cron {
        expression: expression.to_string(),
        minutes: MINUTE.parse(minute)?,
        hours: HOUR.parse(hour)?,
        days_of_month: DAY_OF_MONTH.parse(day_of_month)?,
        months: MONTH.parse(month)?,
        days_of_week: DAY_OF_WEEK.parse(day_of_week)?,
        days,
    })
}

pub(crate) fn zone(name: &str) -> Result<(), Option<&'static str>> {
    match Tz::from_str(name) {
        Ok(_) => Ok(()),
        Err(_) => Err(suggest(name, TZ_VARIANTS.iter().map(|zone| zone.name()))),
    }
}
