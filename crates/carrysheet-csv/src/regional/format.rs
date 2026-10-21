//! Writing values in a convention.

use super::value::{Date, NegativeStyle, Number, Time, Value};
use super::{Convention, DateOrder};
use crate::error::CsvError;
use crate::write::{Render, WriteOptions, write_rendered};

/// Writes typed rows as CSV bytes in `convention`: its delimiter (which
/// wins over `options.delimiter`), decimal mark, grouping, date order and
/// date separator. Everything else comes from `options`.
///
/// Numbers are never rounded. They are written as plain decimals, or in
/// scientific notation when they were read that way or plain digits would
/// not read back as a number (see [`Value::to_text`]). A minus or plus sign
/// goes after a leading currency symbol (`EUR-12`), so no number reads as a
/// formula. Dates are written
/// with four-digit years and two-digit days and months; times as
/// `HH:MM:SS` with a fraction when there is one. Formula escaping applies to
/// text only: a number such as `-1.234,5` is never escaped.
///
/// Text is written as it is, so text that looks like a number in the
/// target convention reads back as one: US text `1,234` written for
/// [`Convention::GERMANY`] reads back as 1.234. Convert such text to a
/// [`Value::Number`] first (for example with [`Value::parse`] in its own
/// convention) when it is meant as a number.
///
/// ```
/// use carrysheet_csv::{Convention, Number, Value, WriteOptions, write_values};
///
/// let rows = vec![vec![Value::Text("Tee".into()), Value::Number(Number::new(-1234.5))]];
/// let out = write_values(&rows, &Convention::GERMANY, &WriteOptions::for_convention(&Convention::GERMANY))?;
/// assert_eq!(out, "\u{FEFF}Tee;-1234,5\r\n".as_bytes());
/// # Ok::<(), carrysheet_csv::CsvError>(())
/// ```
///
/// # Errors
///
/// As [`write()`](crate::write()), plus [`CsvError::InvalidConvention`] when the
/// convention cannot be read back.
pub fn write_values<R: AsRef<[Value]>>(
    rows: &[R],
    convention: &Convention,
    options: &WriteOptions,
) -> Result<Vec<u8>, CsvError> {
    convention.validate()?;
    let render = ValueRender {
        convention,
        escape_formulas: options.escape_formulas,
    };
    write_rendered(rows, convention.delimiter, options, &render)
}

struct ValueRender<'c> {
    convention: &'c Convention,
    escape_formulas: bool,
}

impl<R: AsRef<[Value]>> Render<R> for ValueRender<'_> {
    fn count(&self, row: &R) -> usize {
        row.as_ref().len()
    }

    fn each(&self, row: &R, field: &mut dyn FnMut(&str, bool)) {
        for value in row.as_ref() {
            match value {
                Value::Text(text) => field(text, self.escape_formulas),
                other => field(&value_text(other, self.convention), false),
            }
        }
    }
}

/// The field text of a value in a convention.
pub(crate) fn value_text(value: &Value, c: &Convention) -> String {
    match value {
        Value::Empty => String::new(),
        Value::Text(text) => text.clone(),
        Value::Bool(true) => "TRUE".to_owned(),
        Value::Bool(false) => "FALSE".to_owned(),
        Value::Number(n) => number_text(n, c),
        Value::Date(d) => date_text(*d, c),
        Value::Time(t) => time_text(*t),
        Value::DateTime(d, t) => format!("{} {}", date_text(*d, c), time_text(*t)),
    }
}

/// The shortest digits that read back as `x` (positive, finite), and where
/// the decimal point goes: `0.0125` is `("125", -1)`.
fn shortest_digits(x: f64) -> (String, i64) {
    let s = format!("{x:e}");
    let (mantissa, exponent) = s.split_once('e').unwrap_or((&s, "0"));
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i64 = exponent.parse().unwrap_or(0);
    (digits, exponent + 1)
}

/// Plain digits would not read back as a number: 1e21 or more, below 1e-6,
/// or a whole number with more than 15 significant digits (which reads as
/// an ID). `point` is where the decimal point goes in `digits`.
fn needs_scientific(digits: &str, point: i64) -> bool {
    let len = i64::try_from(digits.len()).unwrap_or(i64::MAX);
    digits != "0" && (!(-5..=21).contains(&point) || (point >= len && digits.len() > 15))
}

fn number_text(n: &Number, c: &Convention) -> String {
    let f = &n.format;
    if !n.value.is_finite() {
        return n.value.to_string();
    }
    let negative = n.value < 0.0;
    let (digits, mut point) = shortest_digits(n.value.abs());
    if f.percent {
        point += 2;
    }
    let decimals = usize::from(f.decimals);
    let mark = c.decimal.as_char();
    let mut body = String::new();
    if f.scientific || needs_scientific(&digits, point) {
        let (first, rest) = digits.split_at(1.min(digits.len()));
        body.push_str(first);
        let mut frac = rest.trim_end_matches('0').to_owned();
        while frac.len() < decimals {
            frac.push('0');
        }
        if !frac.is_empty() {
            body.push(mark);
            body.push_str(&frac);
        }
        let exponent = point - 1;
        let sign = if exponent < 0 { '-' } else { '+' };
        body.push_str(&format!("E{sign}{:02}", exponent.unsigned_abs()));
    } else {
        let len = i64::try_from(digits.len()).unwrap_or(i64::MAX);
        let (int, mut frac) = if point <= 0 {
            let zeros = usize::try_from(-point).unwrap_or(0);
            ("0".to_owned(), "0".repeat(zeros) + &digits)
        } else if point >= len {
            let zeros = usize::try_from(point - len).unwrap_or(0);
            (digits.clone() + &"0".repeat(zeros), String::new())
        } else {
            let at = usize::try_from(point).unwrap_or(0);
            let (a, b) = digits.split_at(at);
            (a.to_owned(), b.to_owned())
        };
        if digits == "0" {
            frac.clear();
        }
        let trimmed = frac.trim_end_matches('0').len();
        frac.truncate(trimmed);
        while frac.len() < decimals {
            frac.push('0');
        }
        match c.grouping.as_char().filter(|_| f.grouped) {
            Some(g) => group(&mut body, &int, g),
            None => body.push_str(&int),
        }
        if !frac.is_empty() {
            body.push(mark);
            body.push_str(&frac);
        }
    }
    if f.percent {
        body.push('%');
    }
    // The sign goes right before the digits. After a leading symbol it
    // keeps `EUR-12` from reading as the formula `-EUR12` (a minus and a
    // cell reference).
    let sign = match (negative, f.negative) {
        (true, NegativeStyle::Minus) => "-",
        (false, _) if f.plus => "+",
        _ => "",
    };
    let trailing = if negative && f.negative == NegativeStyle::Trailing {
        "-"
    } else {
        ""
    };
    let mut out = match f.currency.filter(|_| !f.percent) {
        Some(currency) => {
            let space = if currency.space { " " } else { "" };
            let symbol = currency.symbol();
            if currency.after {
                format!("{sign}{body}{space}{symbol}{trailing}")
            } else {
                format!("{symbol}{space}{sign}{body}{trailing}")
            }
        }
        None => format!("{sign}{body}{trailing}"),
    };
    if negative && f.negative == NegativeStyle::Parentheses {
        out = format!("({out})");
    }
    out
}

/// Writes `int` with `g` between groups of three digits.
fn group(out: &mut String, int: &str, g: char) {
    let len = int.len();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(g);
        }
        out.push(c);
    }
}

fn date_text(d: Date, c: &Convention) -> String {
    let s = c.date_separator;
    let (y, m, day) = (d.year, d.month, d.day);
    match c.date_order {
        DateOrder::Dmy => format!("{day:02}{s}{m:02}{s}{y:04}"),
        DateOrder::Mdy => format!("{m:02}{s}{day:02}{s}{y:04}"),
        DateOrder::Ymd => format!("{y:04}{s}{m:02}{s}{day:02}"),
    }
}

fn time_text(t: Time) -> String {
    let mut out = format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second);
    if t.nanosecond > 0 {
        let frac = format!("{:09}", t.nanosecond);
        out.push('.');
        out.push_str(frac.trim_end_matches('0'));
    }
    out
}
