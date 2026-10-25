use std::collections::BTreeSet;
use std::str::FromStr;

use bosun_types::ir::{Cron, CronDays};
use chrono::{
    DateTime, Datelike, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc,
};
use chrono_tz::Tz;

const HORIZON_DAYS: u32 = 4018;

const LONGEST_GAP_MINUTES: i64 = 48 * 60;

const HOURS: usize = 24;

pub(crate) fn zone(name: &str) -> Option<Tz> {
    Tz::from_str(name).ok()
}

pub(crate) fn next(cron: &Cron, zone: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let mut date = after.with_timezone(&zone).date_naive().pred_opt()?;
    let mut found: Option<DateTime<Utc>> = None;
    for _ in 0..HORIZON_DAYS {
        let first = instants(cron, zone, date)
            .into_iter()
            .find(|instant| *instant > after);
        if let Some(found) = found {
            return Some(first.map_or(found, |first| found.min(first)));
        }
        found = first;
        date = date.succ_opt()?;
    }
    found
}

fn instants(cron: &Cron, zone: Tz, date: NaiveDate) -> BTreeSet<DateTime<Utc>> {
    let mut instants = BTreeSet::new();
    if !runs_on(cron, date) {
        return instants;
    }
    let every_hour = cron.hours.len() == HOURS;
    for hour in &cron.hours {
        for minute in &cron.minutes {
            let Some(time) = NaiveTime::from_hms_opt(u32::from(*hour), u32::from(*minute), 0)
            else {
                continue;
            };
            let local = date.and_time(time);
            match zone.from_local_datetime(&local) {
                LocalResult::Single(instant) => {
                    instants.insert(instant.to_utc());
                }
                LocalResult::Ambiguous(first, second) => {
                    instants.insert(first.to_utc());
                    if every_hour {
                        instants.insert(second.to_utc());
                    }
                }
                LocalResult::None => {
                    if let Some(instant) = after_gap(zone, local) {
                        instants.insert(instant);
                    }
                }
            }
        }
    }
    instants
}

fn after_gap(zone: Tz, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    (1..=LONGEST_GAP_MINUTES).find_map(|minutes| {
        zone.from_local_datetime(&(local + TimeDelta::minutes(minutes)))
            .earliest()
            .map(|instant| instant.to_utc())
    })
}

fn runs_on(cron: &Cron, date: NaiveDate) -> bool {
    let has =
        |values: &[u8], value: u32| u8::try_from(value).is_ok_and(|value| values.contains(&value));
    if !has(&cron.months, date.month()) {
        return false;
    }
    let day_of_month = has(&cron.days_of_month, date.day());
    let day_of_week = has(&cron.days_of_week, date.weekday().num_days_from_sunday());
    match cron.days {
        CronDays::Both => day_of_month && day_of_week,
        CronDays::Either => day_of_month || day_of_week,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use bosun_types::ir::{Schedule, Trigger};
    use chrono::{DateTime, Utc};
    use serde::Deserialize;

    use super::{next, zone};

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Case {
        description: String,
        cron: String,
        timezone: Option<String>,
        after: DateTime<Utc>,
        instants: Vec<DateTime<Utc>>,
    }

    fn cases() -> Vec<Case> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../contracts/fixtures/schedules/cases.json");
        let text =
            fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    fn lowered(case: &Case) -> Schedule {
        let timezone = case
            .timezone
            .as_ref()
            .map(|timezone| format!("\n      timezone: {timezone}"))
            .unwrap_or_default();
        let source = format!(
            "on:\n  schedule:\n    - cron: '{}'{timezone}\njobs:\n  build:\n    runs-on: shell\n    \
             steps:\n      - run: make\n",
            case.cron
        );
        let (triggers, diagnostics) = bosun_workflow::triggers("cases.yml", &source);
        let Some([Trigger::Schedule(schedules)]) = triggers.as_deref() else {
            panic!(
                "{}: the cron does not lower: {diagnostics:?}",
                case.description
            );
        };
        let [schedule] = schedules.as_slice() else {
            panic!("{}: the cron lowers to {schedules:?}", case.description);
        };
        schedule.clone()
    }

    #[test]
    fn each_case_fires_on_its_instants_and_on_no_other() {
        for case in cases() {
            let schedule = lowered(&case);
            let zone = zone(schedule.timezone.as_deref().unwrap_or("UTC"))
                .unwrap_or_else(|| panic!("{}: an unknown time zone", case.description));
            let mut after = case.after;
            let mut fired = Vec::new();
            while fired.len() < case.instants.len() {
                let Some(instant) = next(&schedule.cron, zone, after) else {
                    break;
                };
                fired.push(instant);
                after = instant;
            }
            if case.instants.is_empty() {
                assert_eq!(
                    next(&schedule.cron, zone, after),
                    None,
                    "{}",
                    case.description
                );
            }
            assert_eq!(fired, case.instants, "{}", case.description);
        }
    }
}
