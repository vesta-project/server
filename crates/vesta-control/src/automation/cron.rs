//! Bounded five-field cron evaluation using a fixed UTC offset, not host timezone.
use crate::{ControlError, IdentityErrorCode};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cron {
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    day_wildcard: bool,
    weekday_wildcard: bool,
}

fn invalid() -> ControlError {
    ControlError::rejected(IdentityErrorCode::InvalidRequest)
}

impl Cron {
    /// Numeric minute, hour, day-of-month, month and day-of-week fields. Supports
    /// lists, inclusive ranges and positive steps; Sunday is both 0 and 7.
    pub fn parse(expr: &str) -> crate::Result<Self> {
        if expr.len() > 256 || expr.chars().any(|c| c.is_control()) {
            return Err(invalid());
        }
        let fields: Vec<_> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(invalid());
        }
        Ok(Self {
            minutes: field(fields[0], 0, 59, false)?,
            hours: field(fields[1], 0, 23, false)?,
            days: field(fields[2], 1, 31, false)?,
            months: field(fields[3], 1, 12, false)?,
            weekdays: field(fields[4], 0, 7, true)?,
            day_wildcard: fields[2].starts_with('*'),
            weekday_wildcard: fields[4].starts_with('*'),
        })
    }

    fn matches_day(&self, date: i64) -> bool {
        let (_, month, day) = civil_from_days(date);
        if self.months & (1 << month) == 0 {
            return false;
        }
        let dom = self.days & (1 << day) != 0;
        let dow = self.weekdays & (1 << (date + 4).rem_euclid(7)) != 0;
        match (self.day_wildcard, self.weekday_wildcard) {
            (false, false) => dom || dow,
            _ => dom && dow,
        }
    }

    /// Latest matching UTC minute at or before `epoch_millis`, bounded to the
    /// preceding eight calendar years. Useful for latest-only reconciliation.
    pub fn previous_at_or_before(&self, epoch_millis: i64, utc_offset_minutes: i16) -> Option<i64> {
        if !(-840..=840).contains(&utc_offset_minutes) {
            return None;
        }
        let first_local = epoch_millis
            .div_euclid(60_000)
            .checked_add(i64::from(utc_offset_minutes))?;
        let first_day = first_local.div_euclid(1440);
        let first_minute = first_local.rem_euclid(1440);
        let (year, month, day) = civil_from_days(first_day);
        let end_day = days_from_civil(year - 8, month, day.min(month_days(year - 8, month)));
        for date in (end_day..=first_day).rev() {
            if !self.matches_day(date) {
                continue;
            }
            let maximum = if date == first_day {
                first_minute
            } else {
                1439
            };
            for hour in (0..=maximum / 60).rev() {
                if self.hours & (1 << hour) == 0 {
                    continue;
                }
                let minute_max = if hour == maximum / 60 {
                    maximum % 60
                } else {
                    59
                };
                let available = self.minutes & ((1_u64 << (minute_max + 1)) - 1);
                if available == 0 {
                    continue;
                }
                let minute = i64::from(63 - available.leading_zeros());
                let local = date.checked_mul(1440)?.checked_add(hour * 60 + minute)?;
                let utc = local.checked_sub(i64::from(utc_offset_minutes))?;
                return utc.checked_mul(60_000);
            }
        }
        None
    }

    /// First matching UTC minute strictly after `epoch_millis`. Searches at
    /// most eight calendar years, skipping nonmatching dates and selecting set
    /// hour/minute bits instead of scanning every minute. Offsets are fixed and
    /// constrained to the civil timezone range (-14h..=14h).
    pub fn next_after(&self, epoch_millis: i64, utc_offset_minutes: i16) -> Option<i64> {
        if !(-840..=840).contains(&utc_offset_minutes) {
            return None;
        }
        // Work in minutes to avoid overflow when applying an offset near i64's
        // millisecond endpoints. Euclidean division also handles pre-epoch time.
        let first_utc = epoch_millis.div_euclid(60_000).checked_add(1)?;
        let first_local = first_utc.checked_add(i64::from(utc_offset_minutes))?;
        let first_day = first_local.div_euclid(1440);
        let first_minute = first_local.rem_euclid(1440);
        let (year, month, day) = civil_from_days(first_day);
        let end_day = days_from_civil(year + 8, month, day.min(month_days(year + 8, month)));
        for date in first_day..=end_day {
            if !self.matches_day(date) {
                continue;
            }
            let minimum = if date == first_day { first_minute } else { 0 };
            for hour in minimum / 60..24 {
                if self.hours & (1 << hour) == 0 {
                    continue;
                }
                let minute_min = if hour == minimum / 60 {
                    minimum % 60
                } else {
                    0
                };
                let available = self.minutes & (u64::MAX << minute_min);
                if available == 0 {
                    continue;
                }
                let minute = i64::from(available.trailing_zeros());
                let local = date.checked_mul(1440)?.checked_add(hour * 60 + minute)?;
                let utc = local.checked_sub(i64::from(utc_offset_minutes))?;
                let millis = utc.checked_mul(60_000)?;
                if millis > epoch_millis {
                    return Some(millis);
                }
            }
        }
        None
    }
}

fn number(value: &str) -> crate::Result<u8> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    value.parse().map_err(|_| invalid())
}

fn field(value: &str, min: u8, max: u8, sunday: bool) -> crate::Result<u64> {
    let mut bits = 0;
    for item in value.split(',') {
        let mut pieces = item.split('/');
        let range = pieces.next().ok_or_else(invalid)?;
        let step = pieces.next().map(number).transpose()?.unwrap_or(1);
        if pieces.next().is_some() || step == 0 {
            return Err(invalid());
        }
        let (start, end) = if range == "*" {
            (min, max)
        } else if let Some((start, end)) = range.split_once('-') {
            (number(start)?, number(end)?)
        } else {
            let start = number(range)?;
            (start, if item.contains('/') { max } else { start })
        };
        if start < min || end > max || start > end {
            return Err(invalid());
        }
        for n in (start..=end).step_by(usize::from(step)) {
            bits |= 1_u64 << if sunday && n == 7 { 0 } else { n };
        }
    }
    if bits == 0 { Err(invalid()) } else { Ok(bits) }
}

fn month_days(year: i64, month: i64) -> i64 {
    match month {
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

// Proleptic Gregorian calendar, with day zero at 1970-01-01.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719468;
    let era = days.div_euclid(146097);
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(y: i64, m: i64, d: i64, h: i64, minute: i64) -> i64 {
        (days_from_civil(y, m, d) * 1440 + h * 60 + minute) * 60_000
    }
    #[test]
    fn exclusive_and_pre_epoch() {
        let c = Cron::parse("* * * * *").unwrap();
        assert_eq!(c.next_after(0, 0), Some(60_000));
        assert_eq!(c.next_after(-1, 0), Some(0));
        assert_eq!(c.next_after(-60_000, 0), Some(0));
        assert_eq!(c.next_after(i64::MAX, 0), None);
        assert!(c.next_after(i64::MIN, -840).is_some());
    }
    #[test]
    fn leap_centuries_and_impossible_days() {
        let c = Cron::parse("0 0 29 2 *").unwrap();
        assert_eq!(
            c.next_after(at(2096, 2, 29, 0, 0), 0),
            Some(at(2104, 2, 29, 0, 0))
        );
        assert_eq!(
            c.next_after(at(1999, 1, 1, 0, 0), 0),
            Some(at(2000, 2, 29, 0, 0))
        );
        assert_eq!(Cron::parse("0 0 31 2 *").unwrap().next_after(0, 0), None);
    }
    #[test]
    fn lists_ranges_steps_and_sunday_alias() {
        let c = Cron::parse("5,10-20/5 2-4/2 * * 7").unwrap();
        assert_eq!(
            c.next_after(at(2026, 10, 11, 2, 5), 0),
            Some(at(2026, 10, 11, 2, 10))
        );
        assert_eq!(c, Cron::parse("5,10-20/5 2-4/2 * * 0").unwrap());
        assert_eq!(
            Cron::parse("0/20 * * * *").unwrap().next_after(0, 0),
            Some(20 * 60_000)
        );
    }
    #[test]
    fn day_fields_or_and_offset() {
        let c = Cron::parse("0 0 15 * 0").unwrap();
        assert_eq!(
            c.next_after(at(2026, 10, 10, 0, 0), 0),
            Some(at(2026, 10, 11, 0, 0))
        );
        assert_eq!(
            c.next_after(at(2026, 10, 12, 0, 0), 0),
            Some(at(2026, 10, 15, 0, 0))
        );
        let c = Cron::parse("0 0 * * *").unwrap();
        assert_eq!(
            c.next_after(at(2026, 1, 1, 0, 0), 60),
            Some(at(2026, 1, 1, 23, 0))
        );
        assert_eq!(c.next_after(0, 841), None);
        assert!(c.next_after(0, -840).is_some());
    }
    #[test]
    fn previous_occurrence_is_inclusive_and_bounded() {
        let c = Cron::parse("* * * * *").unwrap();
        assert_eq!(c.previous_at_or_before(0, 0), Some(0));
        assert_eq!(c.previous_at_or_before(59_999, 0), Some(0));
        assert_eq!(c.previous_at_or_before(-1, 0), Some(-60_000));
        assert_eq!(c.previous_at_or_before(i64::MIN, 0), None);
        assert!(c.previous_at_or_before(i64::MAX, 840).is_some());
        assert_eq!(c.previous_at_or_before(0, -841), None);
        let c = Cron::parse("0 0 29 2 *").unwrap();
        assert_eq!(
            c.previous_at_or_before(at(2104, 2, 28, 0, 0), 0),
            Some(at(2096, 2, 29, 0, 0))
        );
        assert_eq!(
            Cron::parse("0 0 31 2 *")
                .unwrap()
                .previous_at_or_before(0, 0),
            None
        );
    }
    #[test]
    fn malformed_rejected() {
        for expr in [
            "",
            "* * * *",
            "* * * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "*/0 * * * *",
            "1,,2 * * * *",
            "9-2 * * * *",
            "+1 * * * *",
            "*/1/2 * * * *",
            "@daily",
        ] {
            assert!(Cron::parse(expr).is_err(), "{expr}");
        }
    }
}
