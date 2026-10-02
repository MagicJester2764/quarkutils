//! The date a count of seconds is, and the count a date is.
//!
//! The kernel's clock counts nanoseconds from the start of 1970, in UTC, and
//! knows nothing of years. Whoever prints a date or reads one goes through
//! here. There is no time zone: what is printed is UTC and says so.

/// A moment, as a calendar and a clock have it. UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Date {
    pub year: i64,
    /// 1 to 12.
    pub month: u32,
    /// 1 to 31.
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Days from 1970-01-01 to a date in the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The year, month and day that is `days` days after 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + (month <= 2) as i64, month, day)
}

/// How many days `month` of `year` has.
pub fn days_in(year: i64, month: u32) -> u32 {
    match month {
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 31,
    }
}

impl Date {
    /// The date `seconds` seconds after the start of 1970.
    pub fn from_unix(seconds: u64) -> Date {
        let (year, month, day) = civil_from_days((seconds / 86_400) as i64);
        let rest = (seconds % 86_400) as u32;
        Date {
            year,
            month: month as u32,
            day: day as u32,
            hour: rest / 3_600,
            minute: rest % 3_600 / 60,
            second: rest % 60,
        }
    }

    /// Seconds from the start of 1970 to this date. `None` for a date no
    /// calendar has — a thirteenth month, the thirtieth of February — or
    /// one before 1970.
    pub fn to_unix(&self) -> Option<u64> {
        if !(1..=12).contains(&self.month)
            || self.day == 0
            || self.day > days_in(self.year, self.month)
            || self.hour > 23
            || self.minute > 59
            || self.second > 59
        {
            return None;
        }
        let days = days_from_civil(self.year, self.month as i64, self.day as i64);
        let seconds = days * 86_400 + (self.hour * 3_600 + self.minute * 60 + self.second) as i64;
        u64::try_from(seconds).ok()
    }

    /// Which day of the week it is: 0 for Sunday.
    pub fn weekday(&self) -> u32 {
        // The first of January 1970 was a Thursday.
        (days_from_civil(self.year, self.month as i64, self.day as i64) + 4).rem_euclid(7) as u32
    }
}
