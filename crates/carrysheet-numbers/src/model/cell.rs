//! Cells and their values, and the binary cell storage inside table tiles.

use std::fmt;
use std::sync::Arc;

/// A cell with content: its value and, when it has one, its formula.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub(crate) value: Value,
    pub(crate) formula: Option<Formula>,
}

impl Cell {
    /// The value. For a formula cell, the result Numbers last computed.
    pub fn value(&self) -> &Value {
        &self.value
    }

    /// The formula, when the cell has one.
    pub fn formula(&self) -> Option<&Formula> {
        self.formula.as_ref()
    }
}

/// A cell value.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// No value. A cell can be empty and still have a formula.
    Empty,
    /// A number, including currency amounts. Numbers stores 34 significant
    /// digits; this is the nearest `f64`.
    Number(f64),
    /// Text. Rich text (styles, links, lists) is given as its plain text.
    /// Cells with the same stored text share one string.
    Text(Arc<str>),
    /// A checkbox or `TRUE`/`FALSE`.
    Bool(bool),
    /// A date and time, with no time zone, as Numbers shows it.
    Date(Date),
    /// A duration, in seconds. Always a finite number.
    Duration(f64),
    /// A formula that ends in an error, such as a division by zero.
    Error,
}

/// A date and time with no time zone, counted in seconds from
/// 2001-01-01 00:00:00 as Numbers stores it.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Date {
    seconds: f64,
}

/// The calendar parts of a [`Date`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateParts {
    /// The year, such as 2001.
    pub year: i64,
    /// The month, 1 to 12.
    pub month: u8,
    /// The day of the month, 1 to 31.
    pub day: u8,
    /// The hour, 0 to 23.
    pub hour: u8,
    /// The minute, 0 to 59.
    pub minute: u8,
    /// The second, 0 to 59.
    pub second: u8,
    /// The rest of the second, in nanoseconds.
    pub nanosecond: u32,
}

/// Seconds in a day.
const DAY: f64 = 86_400.0;
/// Days from 0000-03-01 (proleptic Gregorian) to 2001-01-01.
const DAYS_TO_2001: i64 = 730_791;
/// The Excel serial number (1900 date system) of 2001-01-01.
const EXCEL_2001: f64 = 36_892.0;

impl Date {
    /// A date from seconds after 2001-01-01 00:00:00.
    pub fn from_seconds_since_2001(seconds: f64) -> Self {
        Self { seconds }
    }

    /// Seconds after 2001-01-01 00:00:00 (negative before it).
    pub fn seconds_since_2001(self) -> f64 {
        self.seconds
    }

    /// The Excel serial number in the 1900 date system, with the time of
    /// day as the fraction.
    ///
    /// Excel counts 1900 as a leap year (Lotus 1-2-3 did), so its serial 60
    /// is a 1900-02-29 that never was. From 1900-03-01 (serial 61) on, the
    /// serial is the number of days since 1899-12-30. Before that it is one
    /// less: 1900-01-01 is 1, 1900-02-28 is 59, and 1899-12-31 is 0 (Excel
    /// shows it as 1900-01-00). Earlier dates, dates after 9999-12-31 and
    /// values that are not finite have no serial number: `None`.
    pub fn excel_serial(self) -> Option<f64> {
        /// Serial number of 1900-03-01, and of 10000-01-01.
        const LEAP_BUG: f64 = 61.0;
        const AFTER_9999: f64 = 2_958_466.0;
        if !self.seconds.is_finite() {
            return None;
        }
        let days = EXCEL_2001 + self.seconds / DAY;
        if (LEAP_BUG..AFTER_9999).contains(&days) {
            Some(days)
        } else if (1.0..LEAP_BUG).contains(&days) {
            Some(days - 1.0)
        } else {
            None
        }
    }

    /// The calendar date and time, or `None` when the value is not a finite
    /// number of seconds within about a million years of 2001.
    pub fn parts(self) -> Option<DateParts> {
        const LIMIT: f64 = 3.2e13;
        if !self.seconds.is_finite() || self.seconds.abs() > LIMIT {
            return None;
        }
        let whole = self.seconds.floor();
        let nanosecond = ((self.seconds - whole) * 1e9).round().min(999_999_999.0) as u32;
        let whole = whole as i64;
        let days = whole.div_euclid(86_400);
        let secs = whole.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days + DAYS_TO_2001);
        Some(DateParts {
            year,
            month,
            day,
            hour: (secs / 3600) as u8,
            minute: (secs / 60 % 60) as u8,
            second: (secs % 60) as u8,
            nanosecond,
        })
    }
}

impl fmt::Display for Date {
    /// ISO 8601 without a time zone, such as `2001-01-20T00:00:00`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.parts() {
            Some(p) => {
                write!(
                    f,
                    "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                    p.year, p.month, p.day, p.hour, p.minute, p.second
                )?;
                if p.nanosecond != 0 {
                    let frac = format!("{:09}", p.nanosecond);
                    write!(f, ".{}", frac.trim_end_matches('0'))?;
                }
                Ok(())
            }
            None => write!(f, "{} seconds after 2001-01-01", self.seconds),
        }
    }
}

/// Year, month and day from days since 0000-03-01 (Howard Hinnant's
/// public-domain `civil_from_days`, shifted to that epoch).
pub(crate) fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// A formula, as text in the notation Numbers shows.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Formula {
    /// The formula text, such as `SUM(A1:A2)` or `A1×2`. Numbers writes
    /// `×`, `÷`, `≠`, `≤` and `≥` where Excel writes `*`, `/`, `<>`, `<=`
    /// and `>=`, and refers to other tables as `Table 2::B3`. References
    /// are always given as cell addresses, even where Numbers shows a
    /// header name.
    Text(Arc<str>),
    /// The cell has a formula that this reader cannot turn into text yet.
    /// The string says why.
    NotDecoded(Arc<str>),
}

impl Formula {
    /// The formula text, when it was decoded.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(t) => Some(t),
            Self::NotDecoded(_) => None,
        }
    }
}

/// Cell types in the storage buffer (`TST.CellType`, plus 10 for currency).
pub(crate) mod kind {
    pub(crate) const EMPTY: u8 = 0;
    pub(crate) const NUMBER: u8 = 2;
    pub(crate) const TEXT: u8 = 3;
    pub(crate) const DATE: u8 = 5;
    pub(crate) const BOOL: u8 = 6;
    pub(crate) const DURATION: u8 = 7;
    pub(crate) const ERROR: u8 = 8;
    pub(crate) const RICH_TEXT: u8 = 9;
    pub(crate) const CURRENCY: u8 = 10;
}

/// The fields of one cell's storage buffer that this reader uses.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Stored {
    pub(crate) kind: u8,
    pub(crate) decimal: Option<f64>,
    pub(crate) double: Option<f64>,
    pub(crate) seconds: Option<f64>,
    pub(crate) string_id: Option<u32>,
    pub(crate) rich_id: Option<u32>,
    pub(crate) formula_id: Option<u32>,
}

/// The only cell storage version this reader knows.
const STORAGE_VERSION: u8 = 5;

/// Parses a cell storage buffer: a version byte, the cell type, two bytes
/// of extra bits at 6, 32-bit flags at 8, then one field for each flag set,
/// in flag order.
pub(crate) fn parse_stored(buf: &[u8]) -> Result<Stored, String> {
    let header = buf
        .get(..12)
        .ok_or("the cell data is shorter than 12 bytes")?;
    if header[0] != STORAGE_VERSION {
        return Err(format!(
            "the cell uses storage version {}, not {STORAGE_VERSION}",
            header[0]
        ));
    }
    let flags = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    let mut stored = Stored {
        kind: header[1],
        ..Stored::default()
    };
    let mut pos = 12;
    let mut take = |n: usize| -> Result<&[u8], String> {
        let bytes = buf
            .get(pos..pos + n)
            .ok_or_else(|| "the cell data is cut short".to_owned())?;
        pos += n;
        Ok(bytes)
    };
    let le_f64 = |b: &[u8]| f64::from_le_bytes(b.try_into().unwrap_or([0; 8]));
    let le_u32 = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap_or([0; 4]));
    if flags & 0x1 != 0 {
        stored.decimal = Some(decimal128(take(16)?));
    }
    if flags & 0x2 != 0 {
        stored.double = Some(le_f64(take(8)?));
    }
    if flags & 0x4 != 0 {
        stored.seconds = Some(le_f64(take(8)?));
    }
    if flags & 0x8 != 0 {
        stored.string_id = Some(le_u32(take(4)?));
    }
    if flags & 0x10 != 0 {
        stored.rich_id = Some(le_u32(take(4)?));
    }
    // Cell style, text style, conditional style and conditional rule style.
    for flag in [0x20, 0x40, 0x80, 0x100] {
        if flags & flag != 0 {
            take(4)?;
        }
    }
    if flags & 0x200 != 0 {
        stored.formula_id = Some(le_u32(take(4)?));
    }
    Ok(stored)
}

/// Decodes the 16-byte decimal Numbers uses for numbers: a 113-bit
/// coefficient in bytes 0 to 14, a 14-bit exponent biased by 0x1820 in
/// bytes 14 and 15, and the sign in the top bit.
pub(crate) fn decimal128(b: &[u8]) -> f64 {
    let exponent = (i32::from(b[15] & 0x7f) << 7 | i32::from(b[14] >> 1)) - 0x1820;
    let mut coefficient = u128::from(b[14] & 1);
    for &byte in b[..14].iter().rev() {
        coefficient = coefficient << 8 | u128::from(byte);
    }
    let sign = if b[15] & 0x80 != 0 { "-" } else { "" };
    // Parsing the decimal text gives the correctly rounded f64.
    format!("{sign}{coefficient}e{exponent}")
        .parse()
        .unwrap_or(f64::NAN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        let d = Date::from_seconds_since_2001(0.0);
        assert_eq!(d.to_string(), "2001-01-01T00:00:00");
        assert_eq!(d.excel_serial(), Some(36_892.0));
        let d = Date::from_seconds_since_2001(19.0 * DAY + 3661.5);
        assert_eq!(d.to_string(), "2001-01-20T01:01:01.5");
        let d = Date::from_seconds_since_2001(-DAY);
        assert_eq!(d.to_string(), "2000-12-31T00:00:00");
        let p = Date::from_seconds_since_2001(365.0 * DAY + 59.0 * DAY)
            .parts()
            .unwrap();
        assert_eq!((p.year, p.month, p.day), (2002, 3, 1));
        assert_eq!(Date::from_seconds_since_2001(f64::NAN).parts(), None);
        assert_eq!(Date::from_seconds_since_2001(1e300).parts(), None);
    }

    #[test]
    fn excel_serials_around_the_1900_leap_year_bug() {
        let serial = |days_since_1899_12_30: f64| {
            Date::from_seconds_since_2001((days_since_1899_12_30 - EXCEL_2001) * DAY).excel_serial()
        };
        assert_eq!(serial(61.0), Some(61.0), "1900-03-01");
        assert_eq!(serial(61.5), Some(61.5), "1900-03-01 12:00");
        assert_eq!(serial(60.0), Some(59.0), "1900-02-28");
        assert_eq!(serial(60.25), Some(59.25), "1900-02-28 06:00");
        assert_eq!(serial(2.0), Some(1.0), "1900-01-01");
        assert_eq!(serial(1.0), Some(0.0), "1899-12-31");
        assert_eq!(serial(0.5), None, "1899-12-30 12:00");
        assert_eq!(serial(-1000.0), None);
        assert_eq!(serial(2_958_465.5), Some(2_958_465.5), "9999-12-31 12:00");
        assert_eq!(serial(2_958_466.0), None, "10000-01-01");
        assert_eq!(Date::from_seconds_since_2001(f64::NAN).excel_serial(), None);
        assert_eq!(
            Date::from_seconds_since_2001(f64::INFINITY).excel_serial(),
            None
        );
        // Check the calendar: serial 61 is 1900-03-01, serial 1 is 1900-01-01.
        let date = |serial: f64| Date::from_seconds_since_2001((serial - EXCEL_2001) * DAY);
        assert_eq!(date(61.0).to_string(), "1900-03-01T00:00:00");
        assert_eq!(date(2.0).to_string(), "1900-01-01T00:00:00");
    }

    #[test]
    fn decimals() {
        // 123456 × 10^-3
        let mut b = [0u8; 16];
        b[..3].copy_from_slice(&123_456u32.to_le_bytes()[..3]);
        let exp = (0x1820 - 3) as u16;
        b[14] = ((exp & 0x7f) << 1) as u8;
        b[15] = (exp >> 7) as u8;
        assert_eq!(decimal128(&b), 123.456);
        b[15] |= 0x80;
        assert_eq!(decimal128(&b), -123.456);
    }

    #[test]
    fn stored_buffers() {
        assert!(parse_stored(&[5, 2, 0, 0]).is_err());
        assert!(parse_stored(&[4, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
        // A bool cell whose double is missing.
        assert!(parse_stored(&[5, 6, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0]).is_err());
        let mut buf = vec![5, 3, 0, 0, 0, 0, 0, 0, 0x08, 0x02, 0, 0];
        buf.extend_from_slice(&7u32.to_le_bytes());
        buf.extend_from_slice(&9u32.to_le_bytes());
        let s = parse_stored(&buf).unwrap();
        assert_eq!((s.kind, s.string_id, s.formula_id), (3, Some(7), Some(9)));
    }
}
