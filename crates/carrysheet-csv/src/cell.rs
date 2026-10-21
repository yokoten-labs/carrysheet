//! A typed view of a CSV field.

use std::fmt;

/// A typed value guessed from the text of a CSV field.
///
/// Reading keeps every field as text, so nothing is lost on a round trip.
/// Call [`Cell::infer`] when you want numbers and booleans, for example to
/// load the rows into a workbook.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    /// An empty field.
    Empty,
    /// Text that is not a number or a boolean.
    Text(String),
    /// A finite number.
    Number(f64),
    /// `TRUE` or `FALSE`, in any letter case.
    Bool(bool),
}

/// The most significant digits an `f64` keeps exactly, as in spreadsheets.
const MAX_DIGITS: usize = 15;

impl Cell {
    /// Guesses the type of a field the way a spreadsheet does when you type
    /// it, but keeps text whenever a number would change what you see:
    ///
    /// - `""` is [`Cell::Empty`].
    /// - `TRUE` and `FALSE` (any case) are [`Cell::Bool`].
    /// - A plain decimal such as `-12`, `3.5`, `.5` or `1e3` is
    ///   [`Cell::Number`]. A leading `+`, spaces, thousands separators,
    ///   currency signs, `inf` and `NaN` stay text.
    /// - Leading zeros (`007`, ZIP codes, IDs) stay text.
    /// - More than 15 digits (card numbers, long IDs) stay text, because a
    ///   number would round them.
    pub fn infer(field: &str) -> Cell {
        if field.is_empty() {
            return Cell::Empty;
        }
        if field.eq_ignore_ascii_case("true") {
            return Cell::Bool(true);
        }
        if field.eq_ignore_ascii_case("false") {
            return Cell::Bool(false);
        }
        if let Some(shape) = number_shape(field)
            && !field.starts_with('+')
            && !shape.leading_zero
            && shape.digits <= MAX_DIGITS
            && let Ok(n) = field.parse::<f64>()
            && n.is_finite()
        {
            return Cell::Number(n);
        }
        Cell::Text(field.to_owned())
    }
}

impl fmt::Display for Cell {
    /// Writes the cell as CSV field text: empty, the text, the shortest
    /// decimal that reads back as the same number, or `TRUE`/`FALSE`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Cell::Empty => Ok(()),
            Cell::Text(text) => f.write_str(text),
            Cell::Number(n) => write!(f, "{n}"),
            Cell::Bool(true) => f.write_str("TRUE"),
            Cell::Bool(false) => f.write_str("FALSE"),
        }
    }
}

/// What [`number_shape`] found in a plain decimal number.
pub(crate) struct NumberShape {
    /// Digits before the exponent.
    pub digits: usize,
    /// The integer part has a leading zero and more than one digit.
    pub leading_zero: bool,
}

/// Checks for `[+-]? (digits [. digits*] | . digits) ([eE] [+-]? digits)?`
/// and nothing else.
pub(crate) fn number_shape(text: &str) -> Option<NumberShape> {
    let b = text.as_bytes();
    let mut i = 0;
    if matches!(b.first(), Some(b'+' | b'-')) {
        i += 1;
    }
    let int_start = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let int_digits = i - int_start;
    let mut frac_digits = 0;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        frac_digits = i - start;
    }
    if int_digits + frac_digits == 0 {
        return None;
    }
    if matches!(b.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(b.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    if i != b.len() {
        return None;
    }
    Some(NumberShape {
        digits: int_digits + frac_digits,
        leading_zero: int_digits > 1 && b.get(int_start) == Some(&b'0'),
    })
}
