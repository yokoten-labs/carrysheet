//! Typed values and the format hints that come with them.

use super::detect::{ColumnPlan, classify};
use super::{Convention, normalize};

/// A typed value read from a CSV field.
///
/// Numbers and dates carry what is needed to write them back the way they
/// came (decimal places, currency, percent, negative style), which is also
/// a number-format hint for a spreadsheet.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// An empty field.
    Empty,
    /// Text, exactly as in the file.
    Text(String),
    /// `TRUE` or `FALSE`, in any letter case.
    Bool(bool),
    /// A finite number.
    Number(Number),
    /// A calendar date.
    Date(Date),
    /// A time of day.
    Time(Time),
    /// A date with a time of day.
    DateTime(Date, Time),
}

impl Value {
    /// Parses one field under a fixed convention, with the same rules as
    /// [`read_values`](crate::read_values) uses for a column of that kind:
    /// leading zeros, more than 15 significant digits, and anything that is
    /// not a number, date, time or boolean in this convention stay text.
    ///
    /// ```
    /// use carrysheet_csv::{Convention, Value};
    ///
    /// let Value::Number(n) = Value::parse("1.234,5", &Convention::GERMANY) else { panic!() };
    /// assert_eq!(n.value, 1234.5);
    /// assert_eq!(Value::parse("007", &Convention::GERMANY), Value::Text("007".into()));
    /// ```
    pub fn parse(text: &str, convention: &Convention) -> Value {
        if text.is_empty() {
            return Value::Empty;
        }
        let mut plan = ColumnPlan::fixed(convention);
        let class = classify(text, normalize(text).as_deref(), &mut plan);
        class
            .into_any_value()
            .unwrap_or_else(|| Value::Text(text.to_owned()))
    }

    /// Writes the value as field text in a convention. Numbers are never
    /// rounded: a value with more decimals than its format shows them all.
    /// They are written in scientific notation when they were read that way,
    /// and when plain digits would not read back as a number: 1e21 or more,
    /// below 1e-6, or a whole number with more than 15 significant digits.
    pub fn to_text(&self, convention: &Convention) -> String {
        super::format::value_text(self, convention)
    }
}

/// A number and how it was written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Number {
    /// The value. A percent is stored divided by 100, as in spreadsheets:
    /// `12.5%` is `0.125`.
    pub value: f64,
    /// How it was written.
    pub format: NumberFormat,
}

impl Number {
    /// A number with the default format: plain, no grouping.
    pub fn new(value: f64) -> Self {
        Self {
            value,
            format: NumberFormat::default(),
        }
    }
}

/// How a number was written: a hint for writing it back and for the
/// spreadsheet's number format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NumberFormat {
    /// Digits after the decimal mark as written (`1.50` has 2), at most
    /// 255. On write, at least this many are shown.
    pub decimals: u8,
    /// Written with an explicit plus sign: `+12`.
    pub plus: bool,
    /// Written with digit grouping, such as `1,234`.
    pub grouped: bool,
    /// Written as a percent: `12.5%`.
    pub percent: bool,
    /// Written in scientific notation: `1.5E+10`.
    pub scientific: bool,
    /// The currency symbol and where it stood.
    pub currency: Option<Currency>,
    /// How a negative value was marked.
    pub negative: NegativeStyle,
}

/// How a negative number is marked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NegativeStyle {
    /// A leading minus: `-12`.
    #[default]
    Minus,
    /// Parentheses, as in accounting: `(12)`.
    Parentheses,
    /// A trailing minus, as SAP and some banks write: `12-`.
    Trailing,
}

/// A currency symbol and where it stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Currency {
    index: u8,
    /// After the number (`12 €`) rather than before (`€12`).
    pub after: bool,
    /// A space between the symbol and the number.
    pub space: bool,
}

/// The currency symbols and codes the number parser knows.
pub(crate) const CURRENCIES: [&str; 66] = [
    "€", "$", "£", "¥", "₩", "₹", "₽", "₺", "₫", "฿", "₱", "₪", "₴", "₦", "₡", "₲", "₵", "₸", "₼",
    "₾", "¢", "円", "元", "원", "US$", "A$", "C$", "HK$", "NT$", "NZ$", "S$", "R$", "Mex$", "kr",
    "zł", "Kč", "Ft", "CHF", "EUR", "USD", "GBP", "JPY", "CNY", "RMB", "KRW", "INR", "CAD", "AUD",
    "NZD", "HKD", "TWD", "SGD", "SEK", "NOK", "DKK", "PLN", "CZK", "HUF", "RUB", "TRY", "BRL",
    "MXN", "ZAR", "THB", "IDR", "VND",
];

impl Currency {
    /// A currency from a symbol or code in [`Currency::symbols`], or `None`.
    pub fn new(symbol: &str, after: bool, space: bool) -> Option<Self> {
        let index = CURRENCIES.iter().position(|&s| s == symbol)?;
        Some(Self {
            index: u8::try_from(index).ok()?,
            after,
            space,
        })
    }

    pub(crate) fn from_index(index: usize, after: bool, space: bool) -> Option<Self> {
        Some(Self {
            index: u8::try_from(index).ok()?,
            after,
            space,
        })
    }

    /// The symbol or code, such as `€`, `CHF` or `US$`.
    pub fn symbol(&self) -> &'static str {
        CURRENCIES
            .get(usize::from(self.index))
            .copied()
            .unwrap_or("")
    }

    /// Every symbol and code the reader knows.
    pub fn symbols() -> &'static [&'static str] {
        &CURRENCIES
    }
}

/// A calendar date. [`Date::new`] checks it; the fields are public so a
/// date can be built in a pattern, and an invalid one is written as is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    /// The year, 1000 to 9999.
    pub year: u16,
    /// The month, 1 to 12.
    pub month: u8,
    /// The day of the month, from 1.
    pub day: u8,
}

impl Date {
    /// A date, or `None` when it does not exist (`2026-02-29`) or the year
    /// is outside 1000 to 9999.
    pub fn new(year: u16, month: u8, day: u8) -> Option<Self> {
        if !(1000..=9999).contains(&year) || day == 0 || day > days_in_month(year, month)? {
            return None;
        }
        Some(Self { year, month, day })
    }
}

fn days_in_month(year: u16, month: u8) -> Option<u8> {
    Some(match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400)) => {
            29
        }
        2 => 28,
        _ => return None,
    })
}

/// A time of day.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Time {
    /// The hour, 0 to 23.
    pub hour: u8,
    /// The minute, 0 to 59.
    pub minute: u8,
    /// The second, 0 to 59.
    pub second: u8,
    /// The fraction of the second in nanoseconds.
    pub nanosecond: u32,
}

impl Time {
    /// A time, or `None` when a part is out of range.
    pub fn new(hour: u8, minute: u8, second: u8, nanosecond: u32) -> Option<Self> {
        (hour < 24 && minute < 60 && second < 60 && nanosecond < 1_000_000_000).then_some(Self {
            hour,
            minute,
            second,
            nanosecond,
        })
    }
}
