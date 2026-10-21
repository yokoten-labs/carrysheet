//! Numbers in any common convention: `1,234.5`, `1.234,5`, `1 234,5`,
//! `1'234.5`, `(1,234.50)`, `12-`, `12,5 %`, `€1,234`, `1,23,456`.

use super::value::{CURRENCIES, Currency, NegativeStyle, Number, NumberFormat};
use super::{DecimalMark, Grouping, is_space};

/// The longest field the number and date parsers look at. Longer text is
/// never a number or a date. This bounds the work and memory per cell, and
/// still fits the longest plain `f64` (about 330 digits) with grouping.
pub(crate) const MAX_VALUE_BYTES: usize = 1024;

/// The most significant digits an `f64` keeps exactly, as in spreadsheets.
pub(crate) const MAX_DIGITS: usize = 15;

/// The most digits in an exponent (`1E+9999` is already infinite).
const MAX_EXPONENT_DIGITS: usize = 4;

/// The most significant digits accepted from text that is exactly the
/// shortest form of an `f64` with a fraction or an exponent, as computed
/// values are written (`0.30000000000000004`).
const MAX_SHORTEST_DIGITS: usize = 17;

/// A plus sign before at least this many digits, with nothing but
/// separators between them, is a phone number (`+4915112345678`,
/// `+48 612 345 678`), not a number, in every convention.
const PHONE_DIGITS: usize = 8;

/// Which grouping characters a parse accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GroupAccept {
    /// Only this grouping (the three spaces count as one), or `None` for
    /// any grouping other than the decimal mark.
    pub only: Option<Grouping>,
    /// Whole numbers grouped with spaces (`640 480`) are numbers. Without
    /// this, a space-grouped value needs a fraction (`1 234,5`): spaces
    /// between digits are too common in text (sizes, codes) to mean a
    /// number on their own.
    pub space_integers: bool,
    /// Whole numbers grouped with a no-break or narrow no-break space
    /// (`2 102 650`) are numbers: set when the decimal comma is decided.
    /// These characters almost never group anything but digits.
    pub nbsp_integers: bool,
}

impl GroupAccept {
    /// Any grouping; space-grouped values need a fraction.
    pub(crate) const ANY: GroupAccept = GroupAccept {
        only: None,
        space_integers: false,
        nbsp_integers: false,
    };
}

/// What [`parse_number`] found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum NumberParse {
    /// A number, and the grouping it used.
    Number(Number, Grouping),
    /// A number with a leading zero, such as `007`: text.
    LeadingZero,
    /// A number with more than 15 significant digits: text.
    TooManyDigits,
    /// A plus sign before a long run of digits: a phone number, text.
    PhoneLike,
    /// A whole number grouped with spaces (`640 480`) where such grouping
    /// is not accepted: text, maybe a number.
    SpaceGrouped,
    /// A value so small (subnormal) that an f64 cannot keep the digits as
    /// written: text.
    TooSmall,
    /// Not a number.
    Not,
}

impl NumberParse {
    pub(crate) fn number(&self) -> Option<(Number, Grouping)> {
        match self {
            NumberParse::Number(n, g) => Some((*n, *g)),
            _ => None,
        }
    }
}

/// The parts around the digits: sign, currency, percent.
struct Parts<'a> {
    core: &'a str,
    negative: Option<NegativeStyle>,
    plus: bool,
    percent: bool,
    currency: Option<Currency>,
}

/// Parses normalised text (see `normalize`) as a number with `decimal` as
/// the decimal mark.
pub(crate) fn parse_number(text: &str, decimal: DecimalMark, accept: GroupAccept) -> NumberParse {
    if text.len() > MAX_VALUE_BYTES || !text.bytes().any(|b| b.is_ascii_digit()) {
        return NumberParse::Not;
    }
    let Some(parts) = split_parts(text) else {
        return NumberParse::Not;
    };
    let parsed = match parse_core(&parts, decimal, accept) {
        Ok(n) => n,
        Err(other) => other,
    };
    // A plus sign before eight or more digits is a phone number, whatever
    // the grouping and the convention: `+48 612 345 678` is also valid
    // thousands grouping, and text keeps every character where a number
    // would not. A plus-signed whole number this long that we wrote reads
    // back as text with a note. A value with a fraction in this decimal
    // mark (`+1 234 567,50`) is no phone number.
    let fraction = matches!(&parsed, NumberParse::Number(n, _) if n.format.decimals > 0);
    if parts.plus
        && parts.currency.is_none()
        && !parts.percent
        && !fraction
        && phone_like(parts.core)
    {
        return NumberParse::PhoneLike;
    }
    parsed
}

fn split_parts(text: &str) -> Option<Parts<'_>> {
    let mut t = text.trim_matches(is_space);
    let mut negative = None;
    if let Some(inner) = t.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        negative = Some(NegativeStyle::Parentheses);
        t = inner.trim_matches(is_space);
    }
    let mut signed = negative.is_some();
    let mut plus = false;
    take_sign(&mut t, &mut signed, &mut negative, &mut plus);
    let mut currency = None;
    // Symbols never start or end with a digit: skip the scans then.
    let digit_at = |b: Option<&u8>| b.is_some_and(u8::is_ascii_digit);
    if !digit_at(t.as_bytes().first())
        && let Some((index, rest)) = currency_prefix(t)
    {
        let trimmed = rest.trim_start_matches(is_space);
        currency = Currency::from_index(index, false, trimmed.len() != rest.len());
        t = trimmed;
        take_sign(&mut t, &mut signed, &mut negative, &mut plus);
    }
    if !signed && let Some(rest) = t.strip_suffix('-') {
        negative = Some(NegativeStyle::Trailing);
        t = rest;
    }
    let mut percent = false;
    if currency.is_none() {
        if let Some(rest) = t.strip_suffix('%') {
            percent = true;
            t = rest.trim_end_matches(is_space);
        } else if !digit_at(t.as_bytes().last())
            && let Some((index, rest)) = currency_suffix(t)
        {
            let trimmed = rest.trim_end_matches(is_space);
            currency = Currency::from_index(index, true, trimmed.len() != rest.len());
            t = trimmed;
        }
    }
    if t.is_empty() {
        return None;
    }
    Some(Parts {
        core: t,
        negative,
        plus,
        percent,
        currency,
    })
}

fn take_sign(
    t: &mut &str,
    signed: &mut bool,
    negative: &mut Option<NegativeStyle>,
    plus: &mut bool,
) {
    if *signed {
        return;
    }
    if let Some(rest) = t.strip_prefix('-') {
        *negative = Some(NegativeStyle::Minus);
        *signed = true;
        *t = rest;
    } else if let Some(rest) = t.strip_prefix('+') {
        *plus = true;
        *signed = true;
        *t = rest;
    }
}

/// The longest currency symbol at the start, and the rest.
fn currency_prefix(t: &str) -> Option<(usize, &str)> {
    let (index, symbol) = CURRENCIES
        .iter()
        .enumerate()
        .filter(|(_, s)| t.starts_with(**s))
        .max_by_key(|(_, s)| s.len())?;
    Some((index, t.get(symbol.len()..)?))
}

/// The longest currency symbol at the end, and the rest.
fn currency_suffix(t: &str) -> Option<(usize, &str)> {
    let (index, symbol) = CURRENCIES
        .iter()
        .enumerate()
        .filter(|(_, s)| t.ends_with(**s))
        .max_by_key(|(_, s)| s.len())?;
    Some((index, t.get(..t.len() - symbol.len())?))
}

fn parse_core(
    parts: &Parts<'_>,
    decimal: DecimalMark,
    accept: GroupAccept,
) -> Result<NumberParse, NumberParse> {
    use NumberParse::Not;
    let core = parts.core;
    let (mantissa, exponent) = match core.find(['e', 'E']) {
        Some(at) => (
            core.get(..at).ok_or(Not)?,
            Some(core.get(at + 1..).ok_or(Not)?),
        ),
        None => (core, None),
    };
    let mark = decimal.as_char();
    let (int_part, frac) = match mantissa.find(mark) {
        Some(at) => (
            mantissa.get(..at).ok_or(Not)?,
            Some(mantissa.get(at + mark.len_utf8()..).ok_or(Not)?),
        ),
        None => (mantissa, None),
    };
    if let Some(f) = frac
        && (f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(Not);
    }

    // The text for the f64 parser: sign, every digit, `e`, exponent. Built
    // on the stack; the longest field fits.
    let mut buf = Buf::new();
    if parts.negative.is_some() {
        buf.push(b'-')?;
    }
    let digits_start = buf.len;

    // The integer part: digits, maybe in groups.
    let mut separator: Option<char> = None;
    let mut groups = Groups::default();
    let mut run = 0usize;
    for c in int_part.chars() {
        if c.is_ascii_digit() {
            buf.push(c as u8)?;
            run += 1;
            continue;
        }
        let grouping = Grouping::from_char(c).ok_or(Not)?;
        let accepted = match accept.only {
            None => grouping.as_char() != Some(mark),
            Some(only) => only != Grouping::None && grouping.same_family(only),
        };
        if !accepted || separator.is_some_and(|s| s != c) {
            return Err(Not);
        }
        separator = Some(c);
        groups.push(run);
        run = 0;
    }
    groups.push(run);
    let int_digits = buf.len - digits_start;
    let first_digit = buf.b.get(digits_start).copied();
    let grouping = match separator {
        Some(c) => {
            if !groups.valid(c == ',') {
                return Err(Not);
            }
            if first_digit == Some(b'0') {
                // `012 345 678`: the zero would be lost. A lone `0`
                // (`0.123` read with a decimal comma) is just not a number.
                return Err(if groups.first > 1 {
                    NumberParse::LeadingZero
                } else {
                    Not
                });
            }
            Grouping::from_char(c).ok_or(Not)?
        }
        None => Grouping::None,
    };
    if int_digits == 0 && frac.is_none() {
        return Err(Not);
    }

    let mut exp: i32 = 0;
    if let Some(e) = exponent {
        if grouping != Grouping::None || int_digits == 0 {
            return Err(Not);
        }
        let (sign, body) = match e.as_bytes().first() {
            Some(b'-') => (-1, e.get(1..).ok_or(Not)?),
            Some(b'+') => (1, e.get(1..).ok_or(Not)?),
            _ => (1, e),
        };
        if body.is_empty()
            || body.len() > MAX_EXPONENT_DIGITS
            || !body.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(Not);
        }
        exp = sign * body.parse::<i32>().map_err(|_| Not)?;
    }
    // Spaces between digits are common in text (sizes, codes): a whole
    // number grouped with them needs the grouping accepted.
    if grouping.same_family(Grouping::Space)
        && frac.is_none()
        && !accept.space_integers
        && !(grouping != Grouping::Space && accept.nbsp_integers)
    {
        return Err(NumberParse::SpaceGrouped);
    }

    if int_digits > 1 && first_digit == Some(b'0') {
        return Err(NumberParse::LeadingZero);
    }
    let whole = frac.is_none() && exponent.is_none();
    let frac = frac.unwrap_or("");
    for b in frac.bytes() {
        buf.push(b)?;
    }
    // Zeros at either end only place the point: `1000` and `0.001` have
    // one significant digit, and an f64 keeps them exactly.
    let digits_end = buf.len;
    let significant_len = trim_zeros(buf.b.get(digits_start..digits_end).ok_or(Not)?).len();
    if significant_len > MAX_DIGITS && (whole || significant_len > MAX_SHORTEST_DIGITS) {
        return Err(NumberParse::TooManyDigits);
    }

    let frac_len = i32::try_from(frac.len()).map_err(|_| Not)?;
    exp -= frac_len;
    if parts.percent {
        exp -= 2;
    }
    buf.push(b'e')?;
    buf.push_int(exp)?;
    let value: f64 = buf.as_str().parse().map_err(|_| Not)?;
    // Overflow to infinity and underflow to zero both lose the value.
    if !value.is_finite() || (value == 0.0 && significant_len > 0) {
        return Err(Not);
    }
    let significant = trim_zeros(buf.b.get(digits_start..digits_end).ok_or(Not)?);
    // 16 or 17 digits only when they are exactly what an f64 writes for
    // itself; anything else would be rounded.
    if significant_len > MAX_DIGITS && !same_digits(value, significant) {
        return Err(NumberParse::TooManyDigits);
    }
    // A subnormal keeps fewer digits than it shows; take it only when it
    // reads back as written.
    if value != 0.0 && value.abs() < f64::MIN_POSITIVE && !same_digits(value, significant) {
        return Err(NumberParse::TooSmall);
    }
    let format = NumberFormat {
        decimals: u8::try_from(frac.len()).unwrap_or(u8::MAX),
        plus: parts.plus,
        grouped: grouping != Grouping::None,
        percent: parts.percent,
        scientific: exponent.is_some(),
        currency: parts.currency,
        negative: parts.negative.unwrap_or_default(),
    };
    Ok(NumberParse::Number(Number { value, format }, grouping))
}

/// A plus sign's digits look like a phone number: eight or more digits with
/// only spaces, `-`, `/`, `.`, `,` or apostrophes between them (no
/// exponent, no letters).
fn phone_like(core: &str) -> bool {
    let mut digits = 0usize;
    for c in core.chars() {
        match c {
            '0'..='9' => digits += 1,
            ' ' | '\u{A0}' | '\u{202F}' | '-' | '/' | '.' | ',' | '\'' | '\u{2019}' => {}
            _ => return false,
        }
    }
    digits >= PHONE_DIGITS
}

fn trim_zeros(digits: &[u8]) -> &[u8] {
    let start = digits
        .iter()
        .position(|&b| b != b'0')
        .unwrap_or(digits.len());
    let end = digits
        .iter()
        .rposition(|&b| b != b'0')
        .map_or(start, |i| i + 1);
    digits.get(start..end).unwrap_or_default()
}

/// The shortest digits of `x` are exactly `significant`.
fn same_digits(x: f64, significant: &[u8]) -> bool {
    shortest_digits(x).as_bytes() == significant
}

/// The longest text the f64 parser gets: every digit of the longest field,
/// a sign, `e` and an exponent.
const BUF_BYTES: usize = MAX_VALUE_BYTES + 16;

/// A fixed buffer on the stack, so parsing a number does not allocate.
struct Buf {
    b: [u8; BUF_BYTES],
    len: usize,
}

impl Buf {
    fn new() -> Self {
        Self {
            b: [0; BUF_BYTES],
            len: 0,
        }
    }

    fn push(&mut self, byte: u8) -> Result<(), NumberParse> {
        let slot = self.b.get_mut(self.len).ok_or(NumberParse::Not)?;
        *slot = byte;
        self.len += 1;
        Ok(())
    }

    fn push_int(&mut self, n: i32) -> Result<(), NumberParse> {
        if n < 0 {
            self.push(b'-')?;
        }
        let mut digits = [0u8; 10];
        let mut count = 0;
        let mut rest = n.unsigned_abs();
        loop {
            let slot = digits.get_mut(count).ok_or(NumberParse::Not)?;
            *slot = b'0' + (rest % 10) as u8;
            count += 1;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        for &d in digits.get(..count).unwrap_or_default().iter().rev() {
            self.push(d)?;
        }
        Ok(())
    }

    fn as_str(&self) -> &str {
        self.b
            .get(..self.len)
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("")
    }
}

/// Digit groups, checked as they come: groups of three after a first group
/// of one to three digits, or with a comma the Indian style: three at the
/// end, twos before (`1,23,45,678`).
#[derive(Default)]
struct Groups {
    count: usize,
    first: usize,
    /// The last group seen after the first.
    last: Option<usize>,
    rest_all_three: bool,
    middle_all_two: bool,
}

impl Groups {
    fn push(&mut self, run: usize) {
        if self.count == 0 {
            self.first = run;
            self.rest_all_three = true;
            self.middle_all_two = true;
        } else {
            self.rest_all_three &= run == 3;
            if let Some(previous) = self.last {
                self.middle_all_two &= previous == 2;
            }
            self.last = Some(run);
        }
        self.count += 1;
    }

    fn valid(&self, indian_ok: bool) -> bool {
        let Some(last) = self.last else {
            return false;
        };
        if self.first == 0 {
            return false;
        }
        if (1..=3).contains(&self.first) && self.rest_all_three {
            return true;
        }
        indian_ok
            && self.count >= 3
            && self.middle_all_two
            && last == 3
            && (1..=2).contains(&self.first)
    }
}

/// The significant digits of the shortest text that reads back as `x`:
/// `0.0125` gives `125`.
pub(crate) fn shortest_digits(x: f64) -> String {
    let s = format!("{:e}", x.abs());
    let mantissa = s.split_once('e').map_or(s.as_str(), |(m, _)| m);
    mantissa.chars().filter(char::is_ascii_digit).collect()
}
