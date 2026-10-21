//! Dates and times: `2026-10-20`, `20.10.2026`, `10/20/2026`, `2026年10月20日`,
//! `20-Oct-2026`, `Oct 20, 2026`, `14:30`, `2:30 PM`, `2026-10-20T14:30:00`.

use super::DateOrder;
use super::value::{Date, Time, Value};

/// What [`parse_date`] found: the value under each date order (indexed by
/// [`DateOrder::index`]). A form that does not depend on the order, such as
/// ISO 8601, has the same value in all three.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DateParse {
    pub by_order: [Option<Value>; 3],
    /// The value under that order used a two-digit year.
    pub two_digit: [bool; 3],
    /// The separator between numeric date parts.
    pub separator: Option<char>,
}

impl DateParse {
    fn fixed(value: Value, two_digit: bool, separator: Option<char>) -> Self {
        Self {
            by_order: [Some(value.clone()), Some(value.clone()), Some(value)],
            two_digit: [two_digit; 3],
            separator,
        }
    }
}

const MONTHS: [(&str, u8); 24] = [
    ("january", 1),
    ("february", 2),
    ("march", 3),
    ("april", 4),
    ("may", 5),
    ("june", 6),
    ("july", 7),
    ("august", 8),
    ("september", 9),
    ("october", 10),
    ("november", 11),
    ("december", 12),
    ("jan", 1),
    ("feb", 2),
    ("mar", 3),
    ("apr", 4),
    ("jun", 6),
    ("jul", 7),
    ("aug", 8),
    ("sep", 9),
    ("sept", 9),
    ("oct", 10),
    ("nov", 11),
    ("dec", 12),
];

/// Parses normalised text (see `normalize`) as a date, a time, or both.
/// Returns `None` for anything else.
pub(crate) fn parse_date(text: &str, pivot: u16) -> Option<DateParse> {
    if let Some(time) = parse_time(text) {
        return Some(DateParse::fixed(Value::Time(time), false, None));
    }
    if let Some((date, rest)) = cjk_date(text) {
        let value = with_time(date, rest)?;
        return Some(DateParse::fixed(value, false, None));
    }
    if let Some((date, two_digit, rest)) = named_month_date(text, pivot) {
        let value = with_time(date, rest)?;
        return Some(DateParse::fixed(value, two_digit, None));
    }
    numeric_date(text, pivot)
}

/// A date, or a date with the time in `rest` after a space or `T`.
fn with_time(date: Date, rest: &str) -> Option<Value> {
    if rest.is_empty() {
        return Some(Value::Date(date));
    }
    let after = rest.strip_prefix(['T', ' '])?.trim_start();
    Some(Value::DateTime(date, parse_time(after)?))
}

/// The leading ASCII digits and the rest.
fn digits(s: &str) -> (&str, &str) {
    let end = s
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    (s.get(..end).unwrap_or(""), s.get(end..).unwrap_or(""))
}

fn number(s: &str) -> Option<u16> {
    s.parse().ok()
}

/// A year from four digits, or from two through the window starting at
/// `pivot`.
fn year(s: &str, pivot: u16) -> Option<(u16, bool)> {
    match s.len() {
        4 => Some((number(s)?, false)),
        2 => {
            let yy = number(s)?;
            let base = pivot - pivot % 100;
            let y = base + yy;
            Some((if y < pivot { y + 100 } else { y }, true))
        }
        _ => None,
    }
}

fn date(y: u16, m: &str, d: &str) -> Option<Date> {
    Date::new(
        y,
        u8::try_from(number(m)?).ok()?,
        u8::try_from(number(d)?).ok()?,
    )
}

/// `a/b/c`, `a-b-c` or `a.b.c` with digits only.
fn numeric_date(text: &str, pivot: u16) -> Option<DateParse> {
    let (a, rest) = digits(text);
    let separator = rest
        .chars()
        .next()
        .filter(|c| matches!(c, '/' | '-' | '.'))?;
    let (b, rest) = digits(rest.get(1..)?);
    let rest = rest.strip_prefix(separator)?;
    let (c, rest) = digits(rest);
    if !(1..=2).contains(&b.len()) {
        return None;
    }
    if a.len() == 4 {
        // Year first with four digits means year, month, day everywhere.
        if !(1..=2).contains(&c.len()) {
            return None;
        }
        let value = with_time(date(number(a)?, b, c)?, rest)?;
        return Some(DateParse::fixed(value, false, Some(separator)));
    }
    if !(1..=2).contains(&a.len()) || c.is_empty() {
        return None;
    }
    let mut parse = DateParse {
        by_order: [None, None, None],
        two_digit: [false; 3],
        separator: Some(separator),
    };
    if let Some((y, two)) = year(c, pivot) {
        if let Some(d) = date(y, b, a) {
            parse.by_order[DateOrder::Dmy.index()] = with_time(d, rest);
            parse.two_digit[DateOrder::Dmy.index()] = two;
        }
        if let Some(d) = date(y, a, b) {
            parse.by_order[DateOrder::Mdy.index()] = with_time(d, rest);
            parse.two_digit[DateOrder::Mdy.index()] = two;
        }
    }
    if a.len() == 2
        && c.len() <= 2
        && let Some((y, _)) = year(a, pivot)
        && let Some(d) = date(y, b, c)
    {
        parse.by_order[DateOrder::Ymd.index()] = with_time(d, rest);
        parse.two_digit[DateOrder::Ymd.index()] = true;
    }
    parse.by_order.iter().any(Option::is_some).then_some(parse)
}

/// `2026年10月20日` (Chinese, Japanese) or `2026년 10월 20일` (Korean).
fn cjk_date(text: &str) -> Option<(Date, &str)> {
    let (y, rest) = digits(text);
    if y.len() != 4 {
        return None;
    }
    let rest = rest.trim_start().strip_prefix(['年', '년'])?.trim_start();
    let (m, rest) = digits(rest);
    let rest = rest.trim_start().strip_prefix(['月', '월'])?.trim_start();
    let (d, rest) = digits(rest);
    let rest = rest.trim_start().strip_prefix(['日', '일'])?;
    if !(1..=2).contains(&m.len()) || !(1..=2).contains(&d.len()) {
        return None;
    }
    Some((date(number(y)?, m, d)?, rest))
}

/// The English month name at the start, and the rest (after an optional
/// period).
fn month_name(s: &str) -> Option<(u8, &str)> {
    let end = s
        .bytes()
        .position(|b| !b.is_ascii_alphabetic())
        .unwrap_or(s.len());
    let word = s.get(..end)?;
    let month = MONTHS
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(word))?
        .1;
    let rest = s.get(end..)?;
    Some((month, rest.strip_prefix('.').unwrap_or(rest)))
}

/// `20-Oct-2026`, `20 October 2026`, `Oct 20, 2026`, `October 20 2026`.
fn named_month_date(text: &str, pivot: u16) -> Option<(Date, bool, &str)> {
    let (d, rest) = digits(text);
    if !d.is_empty() {
        if d.len() > 2 {
            return None;
        }
        let separator = rest
            .chars()
            .next()
            .filter(|c| matches!(c, '-' | ' ' | '/'))?;
        let (month, rest) = month_name(rest.get(1..)?)?;
        let rest = rest.strip_prefix(separator)?;
        let (y, rest) = digits(rest);
        let (y, two) = year(y, pivot)?;
        let date = Date::new(y, month, u8::try_from(number(d)?).ok()?)?;
        return Some((date, two, rest));
    }
    let (month, rest) = month_name(text)?;
    let (d, rest) = digits(rest.strip_prefix(' ')?);
    let rest = rest.strip_prefix(',').unwrap_or(rest);
    let (y, rest) = digits(rest.strip_prefix(' ')?);
    if d.is_empty() || d.len() > 2 || y.len() != 4 {
        return None;
    }
    let date = Date::new(number(y)?, month, u8::try_from(number(d)?).ok()?)?;
    Some((date, false, rest))
}

/// `14:30`, `14:30:05`, `14:30:05.250`, `2:30 PM`, `2:30:00 a.m.`.
pub(crate) fn parse_time(text: &str) -> Option<Time> {
    let (h, rest) = digits(text);
    if !(1..=2).contains(&h.len()) {
        return None;
    }
    let (m, mut rest) = digits(rest.strip_prefix(':')?);
    if m.len() != 2 {
        return None;
    }
    let mut second = 0;
    let mut nanosecond = 0;
    if let Some(after) = rest.strip_prefix(':') {
        let (s, after) = digits(after);
        if s.len() != 2 {
            return None;
        }
        second = u8::try_from(number(s)?).ok()?;
        rest = after;
        if let Some(after) = rest.strip_prefix(['.', ',']) {
            let (f, after) = digits(after);
            if f.is_empty() || f.len() > 9 {
                return None;
            }
            let scale = 10u32.pow(9 - u32::try_from(f.len()).ok()?);
            nanosecond = f.parse::<u32>().ok()?.checked_mul(scale)?;
            rest = after;
        }
    }
    let mut hour = u8::try_from(number(h)?).ok()?;
    let minute = u8::try_from(number(m)?).ok()?;
    let rest = rest.trim_start();
    if !rest.is_empty() {
        let pm = match rest.to_ascii_lowercase().as_str() {
            "am" | "a.m." => false,
            "pm" | "p.m." => true,
            _ => return None,
        };
        if !(1..=12).contains(&hour) {
            return None;
        }
        hour = match (hour, pm) {
            (12, false) => 0,
            (12, true) => 12,
            (h, true) => h + 12,
            (h, false) => h,
        };
    }
    Time::new(hour, minute, second, nanosecond)
}

/// Text that Excel turns into a date although it is not one here: `3/4`,
/// `1-2`, `10/2026`, `2-Sep`.
pub(crate) fn looks_like_excel_date(text: &str) -> bool {
    let (a, rest) = digits(text);
    if !(1..=2).contains(&a.len()) {
        return false;
    }
    let Some(rest) = rest.strip_prefix(['/', '-']) else {
        return false;
    };
    let (b, tail) = digits(rest);
    if tail.is_empty() {
        return (1..=4).contains(&b.len());
    }
    b.is_empty() && month_name(rest).is_some_and(|(_, tail)| tail.is_empty())
}

/// Gene names that Excel turns into dates: `SEPT2`, `MARCH1`, `DEC1`,
/// `OCT4`, `Mar-3`.
pub(crate) fn looks_like_gene(text: &str) -> bool {
    let end = text
        .bytes()
        .position(|b| !b.is_ascii_alphabetic())
        .unwrap_or(text.len());
    let (word, rest) = (text.get(..end).unwrap_or(""), text.get(end..).unwrap_or(""));
    if !MONTHS
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(word))
    {
        return false;
    }
    let rest = rest.strip_prefix('-').unwrap_or(rest);
    (1..=2).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_digit())
}
