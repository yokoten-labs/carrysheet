//! Regional conventions: decimal marks, digit grouping, date order.
//!
//! Files come from everywhere. `1.234,5` in Berlin is `1,234.5` in Boston,
//! and `03/04/2026` is the 3rd of April in London but March 4th in New
//! York. This module reads typed values ([`Value`]) out of CSV text under a
//! [`Convention`], detects the convention when the caller does not name it,
//! and writes values back in any convention.
//!
//! Detection decides per column and reports why, in plain English, in an
//! [`Analysis`]. When the data cannot settle a question (every date has day
//! and month at most 12, every number looks like `1,234`), the values stay
//! text and a [`Note`] says so; nothing is guessed silently.

mod date;
mod detect;
mod format;
mod number;
mod report;
mod value;

use std::borrow::Cow;

use crate::error::CsvError;
use crate::read::{Detected, ReadOptions, ReadResult, delimiter_byte, read};

pub use format::write_values;
pub use report::{Analysis, ColumnKind, ColumnReport, Decision, Note, NoteKind};
pub use value::{Currency, Date, NegativeStyle, Number, NumberFormat, Time, Value};

/// The character between the whole and the fractional part of a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DecimalMark {
    /// `.`, as in `1,234.5` (English, Chinese, Japanese, Korean, Swiss).
    Point,
    /// `,`, as in `1.234,5` (most of continental Europe, Latin America).
    Comma,
}

impl DecimalMark {
    /// The mark as a character.
    pub fn as_char(self) -> char {
        match self {
            DecimalMark::Point => '.',
            DecimalMark::Comma => ',',
        }
    }

    pub(crate) fn index(self) -> usize {
        match self {
            DecimalMark::Point => 0,
            DecimalMark::Comma => 1,
        }
    }

    pub(crate) const ALL: [DecimalMark; 2] = [DecimalMark::Point, DecimalMark::Comma];
}

/// The character between groups of three digits, as in `1,234,567`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Grouping {
    /// No grouping: `1234567`.
    None,
    /// `,`: `1,234,567` (English; also `1,23,45,678` in India).
    Comma,
    /// `.`: `1.234.567` (German, Spanish, Italian, Portuguese).
    Point,
    /// A plain space: `1 234 567`.
    Space,
    /// U+00A0 no-break space (French, Polish, Russian, Czech).
    NoBreakSpace,
    /// U+202F narrow no-break space (French on recent systems).
    NarrowNoBreakSpace,
    /// `'` (or U+2019): `1'234'567` (Switzerland, Liechtenstein).
    Apostrophe,
}

impl Grouping {
    /// The character written between groups, or `None` for no grouping.
    pub fn as_char(self) -> Option<char> {
        match self {
            Grouping::None => None,
            Grouping::Comma => Some(','),
            Grouping::Point => Some('.'),
            Grouping::Space => Some(' '),
            Grouping::NoBreakSpace => Some('\u{A0}'),
            Grouping::NarrowNoBreakSpace => Some('\u{202F}'),
            Grouping::Apostrophe => Some('\''),
        }
    }

    pub(crate) fn from_char(c: char) -> Option<Grouping> {
        Some(match c {
            ',' => Grouping::Comma,
            '.' => Grouping::Point,
            ' ' => Grouping::Space,
            '\u{A0}' => Grouping::NoBreakSpace,
            '\u{202F}' => Grouping::NarrowNoBreakSpace,
            '\'' | '\u{2019}' => Grouping::Apostrophe,
            _ => return None,
        })
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Grouping::None => 0,
            Grouping::Comma => 1,
            Grouping::Point => 2,
            Grouping::Space => 3,
            Grouping::NoBreakSpace => 4,
            Grouping::NarrowNoBreakSpace => 5,
            Grouping::Apostrophe => 6,
        }
    }

    pub(crate) const ALL: [Grouping; 7] = [
        Grouping::None,
        Grouping::Comma,
        Grouping::Point,
        Grouping::Space,
        Grouping::NoBreakSpace,
        Grouping::NarrowNoBreakSpace,
        Grouping::Apostrophe,
    ];

    /// The three kinds of space are one family: files mix them freely.
    fn family(self) -> usize {
        match self {
            Grouping::Space | Grouping::NoBreakSpace | Grouping::NarrowNoBreakSpace => 3,
            other => other.index(),
        }
    }

    pub(crate) fn same_family(self, other: Grouping) -> bool {
        self.family() == other.family()
    }

    fn describe(self) -> &'static str {
        match self {
            Grouping::None => "no grouping",
            Grouping::Comma => "','",
            Grouping::Point => "'.'",
            Grouping::Space => "space",
            Grouping::NoBreakSpace => "no-break space",
            Grouping::NarrowNoBreakSpace => "narrow no-break space",
            Grouping::Apostrophe => "apostrophe",
        }
    }
}

/// The order of day, month and year in a date written with numbers only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DateOrder {
    /// Day, month, year: `20/10/2026`, `20.10.2026` (most of the world).
    Dmy,
    /// Month, day, year: `10/20/2026` (United States, Philippines).
    Mdy,
    /// Year, month, day: `2026-10-20`, `2026/10/20` (ISO 8601, East Asia).
    Ymd,
}

impl DateOrder {
    pub(crate) fn index(self) -> usize {
        match self {
            DateOrder::Dmy => 0,
            DateOrder::Mdy => 1,
            DateOrder::Ymd => 2,
        }
    }

    pub(crate) const ALL: [DateOrder; 3] = [DateOrder::Dmy, DateOrder::Mdy, DateOrder::Ymd];

    fn describe(self) -> &'static str {
        match self {
            DateOrder::Dmy => "day first (DMY)",
            DateOrder::Mdy => "month first (MDY)",
            DateOrder::Ymd => "year first (YMD)",
        }
    }
}

/// Whether the first row holds column names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Header {
    /// Decide from the data: the first row is a header when one of its cells
    /// is text above a column of numbers, dates or booleans.
    #[default]
    Auto,
    /// The first row is a header; its cells stay text.
    Yes,
    /// There is no header; the first row is data.
    No,
}

/// How numbers and dates are written in a file.
///
/// The presets cover the common cases; change single fields for others:
///
/// ```
/// use carrysheet_csv::{Convention, DateOrder};
///
/// let austria = Convention { date_order: DateOrder::Dmy, ..Convention::GERMANY };
/// assert_eq!(austria.delimiter, ';');
/// ```
///
/// Start from a preset with `..` as above: fields may still be added before
/// 1.0, and code written that way keeps compiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Convention {
    /// The character between cells.
    pub delimiter: char,
    /// The decimal mark.
    pub decimal: DecimalMark,
    /// The digit grouping written on export, and the one accepted on import
    /// (the three kinds of space are accepted for each other).
    pub grouping: Grouping,
    /// The order of day, month and year in numeric dates.
    pub date_order: DateOrder,
    /// The character between date parts on export: `/`, `-` or `.`.
    pub date_separator: char,
    /// Two-digit years fall in the hundred years starting here. The default
    /// is 1930, as in Excel: `00`–`29` are 2000–2029 and `30`–`99` are
    /// 1930–1999.
    pub two_digit_pivot: u16,
}

/// The two-digit year window Excel uses: 1930 to 2029.
pub const EXCEL_TWO_DIGIT_PIVOT: u16 = 1930;

impl Convention {
    /// ISO 8601 and plain programs: `,` `1234.5` `2026-10-20`. The default.
    pub const ISO: Convention = Convention {
        delimiter: ',',
        decimal: DecimalMark::Point,
        grouping: Grouping::None,
        date_order: DateOrder::Ymd,
        date_separator: '-',
        two_digit_pivot: EXCEL_TWO_DIGIT_PIVOT,
    };
    /// United States: `,` `1,234.5` `10/20/2026`.
    pub const US: Convention = Convention {
        delimiter: ',',
        decimal: DecimalMark::Point,
        grouping: Grouping::Comma,
        date_order: DateOrder::Mdy,
        date_separator: '/',
        two_digit_pivot: EXCEL_TWO_DIGIT_PIVOT,
    };
    /// United Kingdom, Ireland, Australia: `,` `1,234.5` `20/10/2026`.
    pub const UK: Convention = Convention {
        delimiter: ',',
        decimal: DecimalMark::Point,
        grouping: Grouping::Comma,
        date_order: DateOrder::Dmy,
        date_separator: '/',
        two_digit_pivot: EXCEL_TWO_DIGIT_PIVOT,
    };
    /// Germany (also Austria, the Netherlands, Italy, Spain with their own
    /// date separators): `;` `1.234,5` `20.10.2026`.
    pub const GERMANY: Convention = Convention {
        delimiter: ';',
        decimal: DecimalMark::Comma,
        grouping: Grouping::Point,
        date_order: DateOrder::Dmy,
        date_separator: '.',
        two_digit_pivot: EXCEL_TWO_DIGIT_PIVOT,
    };
    /// France: `;` `1 234,5` (narrow no-break space) `20/10/2026`.
    pub const FRANCE: Convention = Convention {
        delimiter: ';',
        decimal: DecimalMark::Comma,
        grouping: Grouping::NarrowNoBreakSpace,
        date_order: DateOrder::Dmy,
        date_separator: '/',
        two_digit_pivot: EXCEL_TWO_DIGIT_PIVOT,
    };
    /// Switzerland: `;` `1'234.5` `20.10.2026`.
    pub const SWITZERLAND: Convention = Convention {
        delimiter: ';',
        decimal: DecimalMark::Point,
        grouping: Grouping::Apostrophe,
        date_order: DateOrder::Dmy,
        date_separator: '.',
        two_digit_pivot: EXCEL_TWO_DIGIT_PIVOT,
    };
    /// Japan, China, Korea, Taiwan: `,` `1,234.5` `2026/10/20`.
    pub const EAST_ASIA: Convention = Convention {
        delimiter: ',',
        decimal: DecimalMark::Point,
        grouping: Grouping::Comma,
        date_order: DateOrder::Ymd,
        date_separator: '/',
        two_digit_pivot: EXCEL_TWO_DIGIT_PIVOT,
    };

    /// Checks that the convention can be written and read back.
    ///
    /// # Errors
    ///
    /// [`CsvError::InvalidDelimiter`] or [`CsvError::InvalidConvention`].
    pub fn validate(&self) -> Result<(), CsvError> {
        delimiter_byte(self.delimiter)?;
        check_marks(Some(self.decimal), Some(self.grouping))?;
        check_pivot(self.two_digit_pivot)?;
        check_date_separator(self.date_separator)
    }

    /// Read options with this convention's delimiter.
    pub fn read_options(&self) -> ReadOptions {
        ReadOptions {
            delimiter: Some(self.delimiter),
            ..ReadOptions::default()
        }
    }
}

impl Analysis {
    /// Decides the convention of rows that [`read`](crate::read) returned,
    /// column by column, from at most the first 1,000 rows, 1 MiB of text and
    /// 200,000 cells. What `choice` sets is used as is.
    ///
    /// Call [`Analysis::convert`] next to get typed values, for these rows
    /// or for others with the same columns (detect on a sample, convert the
    /// whole file).
    ///
    /// ```
    /// use carrysheet_csv::{Analysis, ConventionChoice, DecimalMark, ReadOptions, read};
    ///
    /// let r = read(b"Name;Preis\nTee;3,5\n", &ReadOptions::default())?;
    /// let mut analysis = Analysis::detect(&r.rows, &r.detected, &ConventionChoice::default())?;
    /// assert_eq!(analysis.convention.decimal, DecimalMark::Comma);
    /// let values = analysis.convert(r.rows);
    /// assert_eq!(values.len(), 2);
    /// # Ok::<(), carrysheet_csv::CsvError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`CsvError::InvalidConvention`] when `choice` cannot work, for
    /// example the same character as decimal mark and grouping.
    pub fn detect<R, S>(
        rows: &[R],
        detected: &Detected,
        choice: &ConventionChoice,
    ) -> Result<Analysis, CsvError>
    where
        R: AsRef<[S]>,
        S: AsRef<str>,
    {
        choice.validate()?;
        Ok(detect::analyze(rows, detected, choice))
    }
}

impl Default for Convention {
    fn default() -> Self {
        Convention::ISO
    }
}

/// What the caller already knows about a file's convention. Every `None` is
/// detected from the data.
///
/// Like [`Convention`], the fields are public so that a choice reads as a
/// struct literal; build one with `..ConventionChoice::default()` so that
/// fields added in a later 0.x release keep the code compiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConventionChoice {
    /// The delimiter. When set, it wins over
    /// [`ReadOptions::delimiter`](crate::ReadOptions::delimiter) in
    /// [`read_values`].
    pub delimiter: Option<char>,
    /// The decimal mark.
    pub decimal: Option<DecimalMark>,
    /// The digit grouping. When set, only this grouping is accepted.
    pub grouping: Option<Grouping>,
    /// The date order.
    pub date_order: Option<DateOrder>,
    /// The date separator for export. When `None`, the one most used in the
    /// date columns, or the order's usual one. Reading accepts `/`, `-` and
    /// `.` either way.
    pub date_separator: Option<char>,
    /// The first year of the two-digit year window; see
    /// [`Convention::two_digit_pivot`].
    pub two_digit_pivot: u16,
    /// Whether the first row is a header.
    pub header: Header,
}

impl Default for ConventionChoice {
    fn default() -> Self {
        Self {
            delimiter: None,
            decimal: None,
            grouping: None,
            date_order: None,
            date_separator: None,
            two_digit_pivot: EXCEL_TWO_DIGIT_PIVOT,
            header: Header::Auto,
        }
    }
}

impl From<Convention> for ConventionChoice {
    /// Every field set: nothing is detected except the header row.
    fn from(c: Convention) -> Self {
        Self {
            delimiter: Some(c.delimiter),
            decimal: Some(c.decimal),
            grouping: Some(c.grouping),
            date_order: Some(c.date_order),
            date_separator: Some(c.date_separator),
            two_digit_pivot: c.two_digit_pivot,
            header: Header::Auto,
        }
    }
}

impl ConventionChoice {
    fn validate(&self) -> Result<(), CsvError> {
        if let Some(d) = self.delimiter {
            delimiter_byte(d)?;
        }
        check_marks(self.decimal, self.grouping)?;
        if let Some(s) = self.date_separator {
            check_date_separator(s)?;
        }
        check_pivot(self.two_digit_pivot)
    }
}

fn check_marks(decimal: Option<DecimalMark>, grouping: Option<Grouping>) -> Result<(), CsvError> {
    if let (Some(d), Some(g)) = (decimal, grouping)
        && g.as_char() == Some(d.as_char())
    {
        return Err(CsvError::InvalidConvention {
            problem: "the decimal mark and the digit grouping are the same character",
        });
    }
    Ok(())
}

fn check_date_separator(s: char) -> Result<(), CsvError> {
    if !matches!(s, '/' | '-' | '.') {
        return Err(CsvError::InvalidConvention {
            problem: "the date separator must be '/', '-' or '.'",
        });
    }
    Ok(())
}

fn check_pivot(pivot: u16) -> Result<(), CsvError> {
    if !(1000..=9900).contains(&pivot) {
        return Err(CsvError::InvalidConvention {
            problem: "the two-digit year window must start between 1000 and 9900",
        });
    }
    Ok(())
}

/// Typed rows read from a file, and how they were decided.
#[derive(Clone, Debug, PartialEq)]
pub struct ValuesResult {
    /// The rows, each a list of typed values. Text that is not a number,
    /// date, time or boolean in its column stays [`Value::Text`], unchanged.
    pub values: Vec<Vec<Value>>,
    /// The encoding, delimiter and other facts about the file.
    pub detected: Detected,
    /// The convention, and per column what was decided and why.
    pub analysis: Analysis,
}

/// Reads CSV from bytes into typed values under a regional convention,
/// detecting what `choice` leaves open.
///
/// ```
/// use carrysheet_csv::{ConventionChoice, DecimalMark, ReadOptions, Value, read_values};
///
/// let file = "Artikel;Preis\nTee;1.234,50\nKaffee;3,99\n";
/// let r = read_values(file.as_bytes(), &ReadOptions::default(), &ConventionChoice::default())?;
/// assert_eq!(r.analysis.convention.decimal, DecimalMark::Comma);
/// let Value::Number(price) = &r.values[1][1] else { panic!() };
/// assert_eq!(price.value, 1234.5);
/// // What was decided, and why, in plain English:
/// assert!(r.analysis.to_string().contains("Column 2 (Preis): numbers"));
/// # Ok::<(), carrysheet_csv::CsvError>(())
/// ```
///
/// # Errors
///
/// As [`read`](crate::read), plus [`CsvError::InvalidConvention`] for a
/// `choice` that cannot work.
pub fn read_values(
    bytes: &[u8],
    options: &ReadOptions,
    choice: &ConventionChoice,
) -> Result<ValuesResult, CsvError> {
    choice.validate()?;
    let mut options = *options;
    if choice.delimiter.is_some() {
        options.delimiter = choice.delimiter;
    }
    let ReadResult { rows, detected } = read(bytes, &options)?;
    let mut analysis = detect::analyze(&rows, &detected, choice);
    let values = analysis.convert(rows);
    Ok(ValuesResult {
        values,
        detected,
        analysis,
    })
}

/// Text normalised for the number and date parsers: full-width digits,
/// signs and marks become ASCII, U+2212 minus becomes `-`, outer spaces go.
/// `None` for text longer than [`number::MAX_VALUE_BYTES`], which is never
/// a number or a date; this bounds the work per cell. ASCII text is
/// borrowed, not copied.
pub(crate) fn normalize(text: &str) -> Option<Cow<'_, str>> {
    if text.len() > number::MAX_VALUE_BYTES {
        return None;
    }
    if text.is_ascii() {
        return Some(Cow::Borrowed(text.trim_matches(is_space)));
    }
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let mapped = match c {
            '\u{FF10}'..='\u{FF19}' => char::from_digit(u32::from(c) - 0xFF10, 10).unwrap_or(c),
            '\u{FF0B}' => '+',
            '\u{FF0D}' | '\u{2212}' => '-',
            '\u{FF0E}' => '.',
            '\u{FF0C}' => ',',
            '\u{FF05}' => '%',
            '\u{FF08}' => '(',
            '\u{FF09}' => ')',
            '\u{FF04}' => '$',
            '\u{FFE5}' => '¥',
            '\u{FFE1}' => '£',
            '\u{FFE6}' => '₩',
            '\u{FF07}' => '\'',
            '\u{FF1A}' => ':',
            '\u{FF0F}' => '/',
            '\u{3000}' => ' ',
            other => other,
        };
        out.push(mapped);
    }
    let trimmed = out.trim_matches(is_space);
    if trimmed.len() != out.len() {
        return Some(Cow::Owned(trimmed.to_owned()));
    }
    Some(Cow::Owned(out))
}

/// Spaces that may surround a value or sit between a number and its sign.
pub(crate) fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\u{A0}' | '\u{202F}')
}
