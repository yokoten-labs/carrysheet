//! What detection decided, and why, in short plain English.

use std::fmt;

use super::detect::ColumnPlan;
use super::{Convention, DateOrder, DecimalMark};
use crate::encoding::Confidence;

/// What [`Analysis::detect`] decided about a file, and why.
///
/// `Display` prints it as short lines of plain English for people and
/// agents, at most 50 columns and a line that counts the rest. Notes never
/// quote cell contents, and header names have control, bidirectional and
/// invisible characters escaped as `\u{...}`, so the report is safe to log.
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    /// The file's convention. Parts that were not decided take the
    /// [`Convention::ISO`] value; [`Analysis::decimal`] and the other
    /// decisions say which.
    pub convention: Convention,
    /// How the delimiter was decided.
    pub delimiter: Decision,
    /// How the decimal mark was decided, over the whole file. Each column
    /// also decides for itself; see [`ColumnReport::decimal`].
    pub decimal: Decision,
    /// How the digit grouping was decided.
    pub grouping: Decision,
    /// How the date order was decided, over the whole file. Each column also
    /// decides for itself; see [`ColumnReport::date_order`].
    pub date_order: Decision,
    /// The first row is a header and stays text.
    pub header: bool,
    /// How many rows after the header the detection read.
    pub sample_rows: usize,
    /// How many bytes of field text the detection read.
    pub sample_bytes: usize,
    /// One report per column, in order.
    pub columns: Vec<ColumnReport>,
    /// Notes about the file as a whole.
    pub notes: Vec<Note>,
    pub(crate) plans: Vec<ColumnPlan>,
}

/// One decision: whether the data settled it, how surely, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// `false` when nothing settled it and a default stands in.
    pub decided: bool,
    /// How sure the decision is; [`Confidence::Certain`] when chosen.
    pub confidence: Confidence,
    /// Why, in one or two short sentences.
    pub why: String,
}

impl Default for Decision {
    fn default() -> Self {
        Self {
            decided: false,
            confidence: Confidence::Low,
            why: String::new(),
        }
    }
}

/// What a column holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColumnKind {
    /// No values.
    Empty,
    /// Text: kept exactly as written.
    Text,
    /// Numbers.
    Number,
    /// Dates, maybe with times.
    Date,
    /// Times of day.
    Time,
    /// `TRUE` and `FALSE`.
    Bool,
}

impl ColumnKind {
    fn plural(self) -> &'static str {
        match self {
            ColumnKind::Empty => "empty",
            ColumnKind::Text => "text",
            ColumnKind::Number => "numbers",
            ColumnKind::Date => "dates",
            ColumnKind::Time => "times",
            ColumnKind::Bool => "TRUE or FALSE",
        }
    }
}

/// What was decided about one column, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnReport {
    /// The column number, from 1.
    pub column: usize,
    /// The header text, when the file has a header: cut to 64 bytes, with
    /// control, bidirectional and invisible characters written as
    /// `\u{202E}` so that a header cannot change how the report reads.
    pub header: Option<String>,
    /// What the column holds.
    pub kind: ColumnKind,
    /// How sure the decision is.
    pub confidence: Confidence,
    /// The decimal mark of a number column, when its values settle it.
    pub decimal: Option<DecimalMark>,
    /// The date order of a date column, when its values settle it.
    pub date_order: Option<DateOrder>,
    /// Why, in one or two short sentences.
    pub why: String,
    /// Values that stay text or need a look, by reason.
    pub notes: Vec<Note>,
}

/// A group of values that stay text or need a look, for one reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Note {
    /// The reason.
    pub kind: NoteKind,
    /// How many values.
    pub count: usize,
    /// The first row with such a value, from 1.
    pub first_row: usize,
}

/// Why a value stays text or needs a look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum NoteKind {
    /// Leading zeros, as in ZIP codes, phone numbers and IDs (`007`): text,
    /// because a number would drop the zeros.
    LeadingZeros,
    /// More than 15 significant digits, as in IDs and card numbers: text,
    /// because a spreadsheet number would round them.
    TooManyDigits,
    /// Values like `3/4` or `1-2` that Excel turns into dates: text.
    LooksLikeDate,
    /// Gene names like `SEPT2` or `MARCH1` that Excel turns into dates: text.
    GeneName,
    /// Numbers like `1,234` that read as 1234 or 1.234 depending on the
    /// decimal mark, which nothing settled: text.
    AmbiguousDecimal,
    /// Numbers that only read with the other decimal mark: text.
    OtherDecimal,
    /// Dates like `03/04/2026` that read as day first or month first,
    /// which nothing settled: text.
    AmbiguousDate,
    /// Dates that only read in another order: text.
    OtherDateOrder,
    /// Dates with two-digit years, read in the hundred years starting at
    /// `window_start`.
    TwoDigitYears {
        /// The first year of the window.
        window_start: u16,
    },
    /// Values that are not of the column's kind: text.
    OtherKind(ColumnKind),
    /// Cells in columns past the ones detection looked at: text.
    Unchecked,
    /// A plus sign before eight or more digits, like `+4915112345678`:
    /// phone numbers, text.
    PhoneNumbers,
    /// Numbers like `1,234` read with the decimal mark of the other number
    /// columns, because no value in this column settles it.
    BorrowedDecimal,
    /// Dates like `03/04/2026` read in the order of the other date columns,
    /// because no date in this column settles it.
    BorrowedDateOrder,
    /// Values read with a decimal mark or date order that a value after the
    /// detection sample settled.
    SettledLate,
    /// The column's own values settle on this decimal mark, but the other
    /// number columns disagree. `count` values read only this way.
    DecimalConflict(DecimalMark),
    /// The column's own dates settle on this order, but the other date
    /// columns disagree. `count` dates read only this way.
    DateOrderConflict(DateOrder),
    /// Values so small (subnormal) that a number cannot keep the digits as
    /// written: text.
    TooSmall,
    /// Values like `1E5` read as numbers in scientific notation, as Excel
    /// reads them; they may be codes (part or batch numbers).
    LikeCode,
    /// Whole numbers grouped with plain spaces (`2 102 650`) in a file with
    /// a decimal comma: text, because spaces also write sizes and codes
    /// (`640 480`); choose space grouping to read them as numbers.
    SpaceGrouped,
    /// Space-grouped whole numbers in a number column that came before a
    /// value after the sample settled the decimal mark: text.
    BeforeSettled,
}

impl Note {
    /// The note as one short sentence.
    pub fn message(&self) -> String {
        let n = self.count;
        let row = self.first_row;
        let one = n == 1;
        // "1 value stays", "2 values stay".
        let values = if one { "value" } else { "values" };
        let dates = if one { "date" } else { "dates" };
        let stay = if one { "stays" } else { "stay" };
        let is = if one { "is" } else { "are" };
        let has = if one { "has" } else { "have" };
        let reason = match self.kind {
            NoteKind::LeadingZeros => format!(
                "{n} {values} with leading zeros (like ZIP codes, phone numbers or IDs) {stay} text so the zeros are kept"
            ),
            NoteKind::TooManyDigits => format!(
                "{n} {values} with more than 15 significant digits (like IDs or card numbers) {stay} text; a spreadsheet number would round them"
            ),
            NoteKind::LooksLikeDate => format!(
                "{n} {values} like 3/4 or 1-2 {stay} text; Excel would turn them into dates"
            ),
            NoteKind::GeneName => format!(
                "{n} {values} like SEPT2 or MARCH1 (gene names) {stay} text; Excel would turn them into dates"
            ),
            NoteKind::AmbiguousDecimal => format!(
                "{n} {values} like 1,234 could mean 1234 or 1.234 and {stay} text; set the decimal mark to read them"
            ),
            NoteKind::OtherDecimal => {
                format!("{n} {values} with a decimal mark that does not fit the column {stay} text")
            }
            NoteKind::AmbiguousDate => format!(
                "{n} {dates} like 03/04/2026 could be day first or month first and {stay} text; set the date order to read them"
            ),
            NoteKind::OtherDateOrder => {
                format!("{n} {dates} in another day and month order {stay} text")
            }
            NoteKind::TwoDigitYears { window_start } => {
                let end = window_start.saturating_add(99);
                format!("{n} {dates} {has} two-digit years, read as {window_start} to {end}")
            }
            NoteKind::OtherKind(kind) => {
                format!("{n} {values} {is} not {} and {stay} text", kind.plural())
            }
            NoteKind::Unchecked => {
                format!("{n} {values} past the columns detection looked at {stay} text")
            }
            NoteKind::PhoneNumbers => {
                format!("{n} {values} like +49 151 12345678 (phone numbers) {stay} text")
            }
            NoteKind::BorrowedDecimal => format!(
                "{n} {values} like 1,234 {is} read with the decimal mark of the other number columns; check them"
            ),
            NoteKind::BorrowedDateOrder => format!(
                "{n} {dates} like 03/04/2026 {is} read in the order of the other date columns; check them"
            ),
            NoteKind::SettledLate => format!(
                "{n} {values} {is} read the way a value after the sample settled; check them"
            ),
            NoteKind::DecimalConflict(mark) => format!(
                "{n} {values} read only with '{}', but the other number columns use the other mark; check the column",
                mark.as_char()
            ),
            NoteKind::DateOrderConflict(order) => format!(
                "{n} {dates} read only as {}, but the other date columns disagree; check the column",
                order.describe()
            ),
            NoteKind::TooSmall => {
                let (its, they) = if one {
                    ("its", "it")
                } else {
                    ("their", "they")
                };
                format!(
                    "{n} {values} {is} too small for a number to keep {its} digits; {they} {stay} text"
                )
            }
            NoteKind::SpaceGrouped => {
                let (numbers, them) = if one {
                    ("a number", "it")
                } else {
                    ("numbers", "them")
                };
                format!(
                    "{n} {values} like 2 102 650 may be {numbers} grouped with spaces and {stay} text; choose space grouping to read {them}"
                )
            }
            NoteKind::BeforeSettled => {
                let they = if one { "it" } else { "they" };
                format!(
                    "{n} {values} like 2 102 650 came before a value settled the decimal mark, so {they} {stay} text"
                )
            }
            NoteKind::LikeCode => format!(
                "{n} {values} like 1E5 {is} read as numbers in scientific notation, as Excel does; if they are codes (part or batch numbers), the text is lost"
            ),
        };
        format!("{reason}; first in row {row}.")
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = self.message();
        let mut chars = message.chars();
        if let Some(first) = chars.next() {
            write!(f, "{}{}", first.to_uppercase(), chars.as_str())?;
        }
        Ok(())
    }
}

impl fmt::Display for ColumnReport {
    /// `Column 2 (Price): numbers, high confidence. Numbers in ... Notes.`
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Column {}", self.column)?;
        if let Some(name) = &self.header {
            write!(f, " ({name})")?;
        }
        write!(
            f,
            ": {}, {} confidence. {}",
            self.kind.plural(),
            confidence_word(self.confidence),
            self.why
        )?;
        for note in &self.notes {
            write!(f, " {note}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Analysis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = &self.convention;
        writeln!(f, "Delimiter: {}", self.delimiter.why)?;
        writeln!(
            f,
            "Decimal mark '{}' ({}): {}",
            c.decimal.as_char(),
            decision_word(&self.decimal),
            self.decimal.why
        )?;
        writeln!(
            f,
            "Grouping {} ({}): {}",
            c.grouping.describe(),
            decision_word(&self.grouping),
            self.grouping.why
        )?;
        writeln!(
            f,
            "Date order {} ({}): {}",
            c.date_order.describe(),
            decision_word(&self.date_order),
            self.date_order.why
        )?;
        writeln!(
            f,
            "Header row: {}. Sample: {} rows, {} bytes.",
            if self.header { "yes" } else { "no" },
            self.sample_rows,
            self.sample_bytes
        )?;
        for column in self.columns.iter().take(REPORT_COLUMNS) {
            writeln!(f, "{column}")?;
        }
        if let Some(rest) = self.columns.get(REPORT_COLUMNS..).filter(|r| !r.is_empty()) {
            let mut kinds: Vec<(ColumnKind, usize)> = Vec::new();
            let mut notes = 0usize;
            for c in rest {
                notes += c.notes.iter().map(|n| n.count).sum::<usize>();
                match kinds.iter_mut().find(|(k, _)| *k == c.kind) {
                    Some((_, n)) => *n += 1,
                    None => kinds.push((c.kind, 1)),
                }
            }
            let kinds: Vec<String> = kinds
                .iter()
                .map(|(k, n)| format!("{n} {}", k.plural()))
                .collect();
            writeln!(
                f,
                "{} more columns ({}), with {notes} noted values; see Analysis::columns.",
                rest.len(),
                kinds.join(", ")
            )?;
        }
        for note in &self.notes {
            writeln!(f, "{note}")?;
        }
        Ok(())
    }
}

/// `Display` for [`Analysis`] lists at most this many columns.
pub(crate) const REPORT_COLUMNS: usize = 50;

/// `text` cut to at most `max_bytes`, with characters that could change
/// how the report reads written as `\u{202E}`: controls, format and
/// default-ignorable characters (bidirectional marks, zero-width
/// characters, tag characters, variation selectors, fillers), line and
/// paragraph separators, and private use. A backslash is written as `\\`,
/// so text cannot fake an escape. Escaping comes first, so a cut never
/// leaves half an escape or a raw control.
pub(crate) fn printable(text: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let piece = if c == '\\' {
            "\\\\".to_owned()
        } else if hidden(c) {
            format!("\\u{{{:X}}}", u32::from(c))
        } else {
            c.to_string()
        };
        if out.len() + piece.len() > max_bytes {
            break;
        }
        out.push_str(&piece);
    }
    out
}

/// Characters a person does not see but a program (or a language model)
/// reads: controls, and every character in [`HIDDEN`].
pub(crate) fn hidden(c: char) -> bool {
    if c.is_control() {
        return true;
    }
    let c = u32::from(c);
    HIDDEN
        .binary_search_by(|&(start, end)| {
            if end < c {
                std::cmp::Ordering::Less
            } else if start > c {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// Sorted, merged ranges of the characters of general category Cf
/// (format), Co (private use), Zl and Zp (line and paragraph separators),
/// and of the property Default_Ignorable_Code_Point, from Unicode 15.0.
/// Generated from the Unicode Character Database (UnicodeData.txt and
/// DerivedCoreProperties.txt); no dependency needed at run time.
const HIDDEN: [(u32, u32); 28] = [
    (0xAD, 0xAD),
    (0x34F, 0x34F),
    (0x600, 0x605),
    (0x61C, 0x61C),
    (0x6DD, 0x6DD),
    (0x70F, 0x70F),
    (0x890, 0x891),
    (0x8E2, 0x8E2),
    (0x115F, 0x1160),
    (0x17B4, 0x17B5),
    (0x180B, 0x180F),
    (0x200B, 0x200F),
    (0x2028, 0x202E),
    (0x2060, 0x206F),
    (0x3164, 0x3164),
    (0xE000, 0xF8FF),
    (0xFE00, 0xFE0F),
    (0xFEFF, 0xFEFF),
    (0xFFA0, 0xFFA0),
    (0xFFF0, 0xFFFB),
    (0x110BD, 0x110BD),
    (0x110CD, 0x110CD),
    (0x13430, 0x1343F),
    (0x1BCA0, 0x1BCA3),
    (0x1D173, 0x1D17A),
    (0xE0000, 0xE0FFF),
    (0xF0000, 0xFFFFD),
    (0x100000, 0x10FFFD),
];

// The binary search needs the ranges sorted and apart; checked at compile
// time.
const _: () = {
    let mut i = 1;
    while i < HIDDEN.len() {
        assert!(HIDDEN[i - 1].1 < HIDDEN[i].0 && HIDDEN[i].0 <= HIDDEN[i].1);
        i += 1;
    }
};

fn confidence_word(c: Confidence) -> &'static str {
    match c {
        Confidence::Low => "low",
        Confidence::Medium => "medium",
        Confidence::High => "high",
        Confidence::Certain => "certain",
    }
}

fn decision_word(d: &Decision) -> &'static str {
    if d.decided {
        confidence_word(d.confidence)
    } else {
        "not settled"
    }
}

/// Counts notes by kind for one column.
#[derive(Clone, Debug, Default)]
pub(crate) struct Notes(Vec<Note>);

impl Notes {
    pub(crate) fn add(&mut self, kind: NoteKind, row: usize) {
        match self.0.iter_mut().find(|n| n.kind == kind) {
            Some(note) => note.count += 1,
            None => self.0.push(Note {
                kind,
                count: 1,
                first_row: row,
            }),
        }
    }

    pub(crate) fn into_vec(self) -> Vec<Note> {
        self.0
    }

    /// Counts the notes of kind `from` as `to`, keeping the first row.
    pub(crate) fn rename(&mut self, from: NoteKind, to: NoteKind) {
        let Some(at) = self.0.iter().position(|n| n.kind == from) else {
            return;
        };
        let moved = self.0.remove(at);
        match self.0.iter_mut().find(|n| n.kind == to) {
            Some(note) => {
                note.count += moved.count;
                note.first_row = note.first_row.min(moved.first_row);
            }
            None => self.0.insert(at, Note { kind: to, ..moved }),
        }
    }
}
