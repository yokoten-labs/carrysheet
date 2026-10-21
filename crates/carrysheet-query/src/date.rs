//! Excel date serial numbers as ISO 8601 text.

use carrysheet_model::DateSystem;

/// Days from 1970-01-01 to 1899-12-30, the day before serial 1 in the 1900 system
/// once the fictitious 1900-02-29 is skipped.
const DAYS_TO_1899_12_30: i64 = -25_569;
/// Days from 1970-01-01 to 1904-01-01, serial 0 in the 1904 system.
const DAYS_TO_1904_01_01: i64 = -24_107;
/// The last serial Excel shows as a date (9999-12-31), per system.
const MAX_SERIAL_1900: f64 = 2_958_466.0;
const MAX_SERIAL_1904: f64 = 2_957_004.0;

/// Turns an Excel date serial number into ISO 8601 text, the way dates are
/// stored for queries: `2024-03-01`, `2024-03-01T08:30:00`, or `08:30:00` for a
/// time with no date. Seconds are rounded.
///
/// Without the cell's number format, a serial below 1 is read as a time with
/// no date, in either system, as Excel shows a time-only cell. In the 1904
/// system serial 0 is also the date 1904-01-01; when the number format is
/// known, use [`excel_serial_to_iso_with_format`], which decides by it.
///
/// Returns `None` for a serial that is not a real date: negative, past
/// 9999-12-31, not finite, or 60 in the 1900 system (Excel's fictitious
/// 1900-02-29). The caller then keeps the number.
pub fn excel_serial_to_iso(serial: f64, system: DateSystem) -> Option<String> {
    convert(serial, system, Style::Auto)
}

/// Like [`excel_serial_to_iso`], but shaped by the cell's number format: a
/// date-only format such as `yyyy-mm-dd` gives the date alone, a format with
/// both a date and a time gives both (so 1904 serial 0.25 is
/// `1904-01-01T06:00:00`), a time-only format such as `h:mm` gives the time
/// alone for serials below 1. A format with neither (such as `0.00`) works
/// like [`excel_serial_to_iso`].
pub fn excel_serial_to_iso_with_format(
    serial: f64,
    system: DateSystem,
    number_format: &str,
) -> Option<String> {
    convert(serial, system, Style::of(number_format))
}

/// What a number format shows of a serial.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Auto,
    Date,
    Time,
    DateTime,
}

impl Style {
    /// Reads the date and time codes in a number format, ignoring quoted
    /// text, escaped characters and `[...]` sections other than elapsed time
    /// (`[h]`, `[mm]`, `[ss]`). An `m` with no hour or second next to it in
    /// the format means month.
    fn of(format: &str) -> Style {
        let (mut date, mut hour_or_second, mut month_or_minute) = (false, false, false);
        let mut chars = format.chars();
        while let Some(c) = chars.next() {
            match c.to_ascii_lowercase() {
                '"' => {
                    for c in chars.by_ref() {
                        if c == '"' {
                            break;
                        }
                    }
                }
                '\\' | '_' | '*' => {
                    chars.next();
                }
                '[' => {
                    let inner: String = chars.by_ref().take_while(|&c| c != ']').collect();
                    let inner = inner.to_ascii_lowercase();
                    if !inner.is_empty() && inner.chars().all(|c| matches!(c, 'h' | 'm' | 's')) {
                        hour_or_second = true;
                    }
                }
                'y' | 'd' => date = true,
                'h' | 's' => hour_or_second = true,
                'm' => month_or_minute = true,
                _ => {}
            }
        }
        // With an hour or second in the format, m is minutes; else months.
        if month_or_minute && !hour_or_second {
            date = true;
        }
        match (date, hour_or_second) {
            (true, true) => Style::DateTime,
            (true, false) => Style::Date,
            (false, true) => Style::Time,
            (false, false) => Style::Auto,
        }
    }
}

fn convert(serial: f64, system: DateSystem, style: Style) -> Option<String> {
    let max = match system {
        DateSystem::Excel1900 => MAX_SERIAL_1900,
        DateSystem::Excel1904 => MAX_SERIAL_1904,
    };
    if !serial.is_finite() || serial < 0.0 || serial >= max {
        return None;
    }
    // Round to the second first, so 0.99999999 becomes the next day.
    let total_secs = (serial * 86_400.0).round() as i64;
    let mut day = total_secs.div_euclid(86_400);
    let secs = total_secs.rem_euclid(86_400);
    let time = format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    );
    // Day 0 is a time with no date, except in the 1904 system when the
    // format shows a date (day 0 is then 1904-01-01). In the 1900 system day
    // 0 is "1900-01-00", not a real date.
    let real_day_zero =
        system == DateSystem::Excel1904 && matches!(style, Style::Date | Style::DateTime);
    if day == 0 && !real_day_zero {
        return match style {
            Style::Date => None,
            _ => Some(time),
        };
    }
    let unix_day = match system {
        DateSystem::Excel1900 => {
            if day == 60 {
                return None;
            }
            if day < 60 {
                // Before the fictitious leap day, serial 1 is 1900-01-01.
                day += 1;
            }
            DAYS_TO_1899_12_30 + day
        }
        DateSystem::Excel1904 => DAYS_TO_1904_01_01 + day,
    };
    let (y, m, d) = civil_from_days(unix_day);
    if y > 9999 {
        return None;
    }
    let date = format!("{y:04}-{m:02}-{d:02}");
    Some(match style {
        Style::Date => date,
        Style::DateTime => format!("{date}T{time}"),
        Style::Auto | Style::Time if secs == 0 => date,
        Style::Auto | Style::Time => format!("{date}T{time}"),
    })
}

/// Year, month and day for a count of days since 1970-01-01 (proleptic
/// Gregorian calendar). Howard Hinnant's public-domain `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iso(serial: f64) -> Option<String> {
        excel_serial_to_iso(serial, DateSystem::Excel1900)
    }

    #[test]
    fn known_1900_dates() {
        assert_eq!(iso(1.0).as_deref(), Some("1900-01-01"));
        assert_eq!(iso(59.0).as_deref(), Some("1900-02-28"));
        assert_eq!(iso(60.0), None);
        assert_eq!(iso(61.0).as_deref(), Some("1900-03-01"));
        assert_eq!(iso(25_569.0).as_deref(), Some("1970-01-01"));
        assert_eq!(iso(45_352.0).as_deref(), Some("2024-03-01"));
        assert_eq!(iso(45_352.5).as_deref(), Some("2024-03-01T12:00:00"));
        assert_eq!(iso(2_958_465.0).as_deref(), Some("9999-12-31"));
        assert_eq!(iso(2_958_466.0), None);
    }

    #[test]
    fn times_and_rounding() {
        assert_eq!(iso(0.25).as_deref(), Some("06:00:00"));
        assert_eq!(iso(0.0).as_deref(), Some("00:00:00"));
        // A hair before midnight rounds into the next day.
        assert_eq!(iso(45_352.999_999_99).as_deref(), Some("2024-03-02"));
    }

    #[test]
    fn known_1904_dates() {
        let iso = |s| excel_serial_to_iso(s, DateSystem::Excel1904);
        assert_eq!(iso(0.25).as_deref(), Some("06:00:00"));
        assert_eq!(iso(1.0).as_deref(), Some("1904-01-02"));
        assert_eq!(iso(1.5).as_deref(), Some("1904-01-02T12:00:00"));
        assert_eq!(iso(43_890.0).as_deref(), Some("2024-03-01"));
    }

    #[test]
    fn number_formats_decide() {
        let f = |serial, system, format| excel_serial_to_iso_with_format(serial, system, format);
        let (d1900, d1904) = (DateSystem::Excel1900, DateSystem::Excel1904);
        assert_eq!(f(0.0, d1904, "yyyy-mm-dd").as_deref(), Some("1904-01-01"));
        assert_eq!(
            f(0.25, d1904, "yyyy-mm-dd h:mm").as_deref(),
            Some("1904-01-01T06:00:00")
        );
        assert_eq!(f(0.25, d1904, "h:mm AM/PM").as_deref(), Some("06:00:00"));
        assert_eq!(f(0.25, d1904, "0.00").as_deref(), Some("06:00:00"));
        assert_eq!(
            f(0.25, d1900, "yyyy-mm-dd h:mm").as_deref(),
            Some("06:00:00")
        );
        assert_eq!(f(0.25, d1900, "dd/mm/yyyy"), None);
        assert_eq!(
            f(45_352.5, d1900, "d-mmm-yy").as_deref(),
            Some("2024-03-01")
        );
        assert_eq!(
            f(45_352.0, d1900, "m/d/yyyy h:mm").as_deref(),
            Some("2024-03-01T00:00:00")
        );
        assert_eq!(
            f(1.5, d1900, "[h]:mm:ss").as_deref(),
            Some("1900-01-01T12:00:00")
        );
        assert_eq!(f(0.5, d1900, "mm:ss").as_deref(), Some("12:00:00"));
        assert_eq!(
            f(45_352.0, d1900, "\"day\" 0").as_deref(),
            Some("2024-03-01")
        );
        assert_eq!(Style::of("mmm"), Style::Date);
        assert_eq!(Style::of("[$-409]h:mm"), Style::Time);
    }

    #[test]
    fn rejects_non_dates() {
        assert_eq!(iso(-1.0), None);
        assert_eq!(iso(f64::NAN), None);
        assert_eq!(iso(f64::INFINITY), None);
    }
}
