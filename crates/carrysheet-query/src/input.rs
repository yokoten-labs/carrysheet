//! What the engine loads: named tables of typed cells, and how their names and
//! column types are worked out.

use std::collections::{HashMap, HashSet};

use carrysheet_model::{CellValue, column_to_letters};
use serde::{Deserialize, Serialize};

/// Longest table or column name kept, in characters. Longer header text is cut.
pub(crate) const MAX_NAME_CHARS: usize = 128;

/// One cell given to the engine: a model value, or a date as ISO 8601 text.
///
/// The model keeps dates as numbers with a date format, so a reader that
/// knows a cell is a date turns its serial into text with
/// [`crate::excel_serial_to_iso`] and passes [`QueryCell::Date`].
#[derive(Debug, Clone, PartialEq)]
pub enum QueryCell {
    Value(CellValue),
    /// A date or time as ISO 8601 text: `2024-03-01`, `2024-03-01T08:30:00`, `08:30:00`.
    Date(String),
}

impl Default for QueryCell {
    fn default() -> Self {
        QueryCell::Value(CellValue::Empty)
    }
}

impl From<CellValue> for QueryCell {
    fn from(v: CellValue) -> Self {
        QueryCell::Value(v)
    }
}

impl From<f64> for QueryCell {
    fn from(n: f64) -> Self {
        QueryCell::Value(CellValue::Number(n))
    }
}

impl From<bool> for QueryCell {
    fn from(b: bool) -> Self {
        QueryCell::Value(CellValue::Bool(b))
    }
}

impl From<&str> for QueryCell {
    fn from(s: &str) -> Self {
        QueryCell::Value(CellValue::Text(s.to_owned()))
    }
}

impl From<String> for QueryCell {
    fn from(s: String) -> Self {
        QueryCell::Value(CellValue::Text(s))
    }
}

/// Where a table's column names come from.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Header {
    /// The first row holds the names; data starts on the second row. The default.
    #[default]
    FirstRow,
    /// No header row: columns are named by their letters (`A`, `B`, ...).
    Letters,
    /// These names, in order; data starts on the first row. Missing or empty
    /// names become column letters.
    Names(Vec<String>),
}

/// One table to load: a sheet's used range or an Excel table.
#[derive(Debug, Clone, PartialEq)]
pub struct TableInput {
    /// The sheet or table name. It becomes the SQL table name as it is
    /// (spaces, quotes and CJK are fine); a repeat gets `_2`.
    pub name: String,
    pub header: Header,
    /// The sheet column of the first cell in each row (0 is `A`), so empty
    /// headers get the letters people see on the sheet.
    pub first_column: u32,
    /// The sheet row of the first row (0 is row 1), so errors can name a
    /// cell as people see it, such as `C12`.
    pub first_row: u32,
    /// The rows, header row included when `header` is [`Header::FirstRow`].
    /// Rows may have different lengths; missing cells are empty.
    pub rows: Vec<Vec<QueryCell>>,
}

impl TableInput {
    /// A table whose first row holds the column names, starting in column `A`.
    pub fn new(name: impl Into<String>, rows: Vec<Vec<QueryCell>>) -> Self {
        TableInput {
            name: name.into(),
            header: Header::FirstRow,
            first_column: 0,
            first_row: 0,
            rows,
        }
    }

    /// The same table with another header mode.
    pub fn with_header(mut self, header: Header) -> Self {
        self.header = header;
        self
    }

    /// The same table starting at another sheet column (0 is `A`).
    pub fn with_first_column(mut self, first_column: u32) -> Self {
        self.first_column = first_column;
        self
    }

    /// The same table starting at another sheet row (0 is row 1).
    pub fn with_first_row(mut self, first_row: u32) -> Self {
        self.first_row = first_row;
        self
    }

    /// The sheet address of `rows[row][col]`, such as `C12`.
    pub(crate) fn address(&self, row: usize, col: usize) -> String {
        let c = self.first_column.saturating_add(col as u32);
        let r = u64::from(self.first_row) + row as u64 + 1;
        format!("{}{r}", column_to_letters(c))
    }

    /// The index in `rows` of the first data row.
    pub(crate) fn data_start(&self) -> usize {
        match self.header {
            Header::FirstRow => 1,
            Header::Letters | Header::Names(_) => 0,
        }
    }

    /// The rows that hold data (without the header row).
    pub(crate) fn data_rows(&self) -> &[Vec<QueryCell>] {
        match self.header {
            Header::FirstRow => self.rows.get(1..).unwrap_or(&[]),
            Header::Letters | Header::Names(_) => &self.rows,
        }
    }

    /// The number of columns: the widest row, header included.
    pub(crate) fn width(&self) -> usize {
        let widest = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        match &self.header {
            Header::Names(names) => widest.max(names.len()),
            _ => widest,
        }
    }

    /// The header text for each column, `None` when it is empty.
    fn raw_headers(&self, width: usize) -> Vec<Option<String>> {
        let text = |s: &str| Some(s.to_owned()).filter(|s| !s.trim().is_empty());
        (0..width)
            .map(|i| match &self.header {
                Header::FirstRow => match self.rows.first().and_then(|r| r.get(i)) {
                    Some(QueryCell::Value(CellValue::Empty)) | None => None,
                    Some(QueryCell::Value(v)) => text(&v.to_string()),
                    Some(QueryCell::Date(d)) => text(d),
                },
                Header::Letters => None,
                Header::Names(names) => names.get(i).and_then(|n| text(n)),
            })
            .collect()
    }
}

/// The type a column gets from its values. Each value still keeps its own
/// type; this only sets the SQL declared type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Affinity {
    /// No values (or only error values).
    Empty,
    /// Whole numbers only.
    Integer,
    /// Numbers, some with a fraction.
    Real,
    Text,
    /// TRUE and FALSE only, stored as 1 and 0.
    Boolean,
    /// Dates only, stored as ISO 8601 text.
    Date,
    /// More than one kind: no declared type, so SQLite converts nothing.
    Mixed,
}

impl Affinity {
    /// The declared type in `CREATE TABLE`. Empty for `Empty` and `Mixed`,
    /// which gives the column no affinity, so values are kept as given.
    /// `BOOLEAN` and `DATE` have SQLite's numeric affinity; the values stored
    /// there (0, 1 and ISO text) are never converted by it.
    pub fn sql_type(self) -> &'static str {
        match self {
            Affinity::Integer => "INTEGER",
            Affinity::Real => "REAL",
            Affinity::Text => "TEXT",
            Affinity::Boolean => "BOOLEAN",
            Affinity::Date => "DATE",
            Affinity::Empty | Affinity::Mixed => "",
        }
    }

    /// The affinity of a column with these values. Empty and error cells are
    /// not counted.
    pub(crate) fn infer<'a>(cells: impl Iterator<Item = &'a QueryCell>) -> Affinity {
        let mut seen = Affinity::Empty;
        for cell in cells {
            let kind = match cell {
                QueryCell::Value(CellValue::Empty | CellValue::Error(_)) => continue,
                QueryCell::Value(CellValue::Number(n)) if as_integer(*n).is_some() => {
                    Affinity::Integer
                }
                QueryCell::Value(CellValue::Number(_)) => Affinity::Real,
                QueryCell::Value(CellValue::Text(_)) => Affinity::Text,
                QueryCell::Value(CellValue::Bool(_)) => Affinity::Boolean,
                QueryCell::Date(_) => Affinity::Date,
            };
            seen = match (seen, kind) {
                (Affinity::Empty, k) => k,
                (a, b) if a == b => a,
                (Affinity::Integer, Affinity::Real) | (Affinity::Real, Affinity::Integer) => {
                    Affinity::Real
                }
                _ => return Affinity::Mixed,
            };
        }
        seen
    }
}

/// The number as an `i64` when it is whole and exactly representable.
pub(crate) fn as_integer(n: f64) -> Option<i64> {
    const LIMIT: f64 = 9_007_199_254_740_992.0; // 2^53
    if n.is_finite() && n.fract() == 0.0 && n.abs() <= LIMIT {
        Some(n as i64)
    } else {
        None
    }
}

/// One column of a loaded table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnInfo {
    /// The SQL column name.
    pub name: String,
    /// The sheet column letter, such as `C`.
    pub letter: String,
    /// The header text as given, when it differs from `name` (cut, renamed
    /// for a repeat). `None` when the name is the header text or the letter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(rename = "type")]
    pub affinity: Affinity,
    /// Error cells (such as `#N/A`) in the column, stored as their text.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub errors: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// One loaded table, as queries see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableInfo {
    /// The SQL table name.
    pub name: String,
    /// The sheet or table name as given, when it differs from `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub columns: Vec<ColumnInfo>,
    /// Data rows (the header row is not counted).
    pub rows: usize,
}

/// Quotes a name for SQL: `Sales "Q1"` becomes `"Sales ""Q1"""`.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Characters of a name from a query shown at most in an error.
const SHOWN_NAME_CHARS: usize = 64;

/// A name from a query as an error shows it: cut to 64 characters (with
/// "..." when cut) and quoted, so a long or odd name cannot fill the error.
pub(crate) fn shown(name: &str) -> String {
    let mut chars = name.chars();
    let mut cut: String = chars.by_ref().take(SHOWN_NAME_CHARS).collect();
    if chars.next().is_some() {
        cut.push_str("...");
    }
    quote_ident(&cut)
}

/// Like [`shown`], but a short plain word (letters, digits, `_`) stays as
/// it is: `PRAGMA query_only`.
pub(crate) fn shown_word(word: &str) -> String {
    if word.len() <= SHOWN_NAME_CHARS
        && !word.is_empty()
        && word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        word.to_owned()
    } else {
        shown(word)
    }
}

/// A name SQLite accepts and people can type: control characters become
/// spaces, the ends are trimmed, and it is cut to [`MAX_NAME_CHARS`].
fn clean_name(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    replaced.trim().chars().take(MAX_NAME_CHARS).collect()
}

/// Names already used in one scope (the tables, or one table's columns),
/// compared like SQLite compares them: ASCII case is ignored.
#[derive(Debug, Default)]
pub(crate) struct Names {
    taken: HashSet<String>,
    /// The next suffix to try for a repeated name, so many repeats stay fast.
    next: HashMap<String, u32>,
}

impl Names {
    /// `name`, or `name_2`, `name_3`, ... when it is taken; then records it.
    fn unique(&mut self, name: String) -> String {
        let key = name.to_ascii_lowercase();
        if self.taken.insert(key.clone()) {
            return name;
        }
        let mut n = self.next.get(&key).copied().unwrap_or(2);
        loop {
            let candidate = format!("{name}_{n}");
            n += 1;
            if self.taken.insert(candidate.to_ascii_lowercase()) {
                self.next.insert(key, n);
                return candidate;
            }
        }
    }
}

/// The plan for one table: its final names and column types.
pub(crate) fn plan_table(input: &TableInput, index: usize, taken: &mut Names) -> TableInfo {
    let mut name = clean_name(&input.name);
    if name.is_empty() {
        name = format!("Table{}", index + 1);
    }
    let mut name = taken.unique(name);
    // SQLite keeps names starting with `sqlite_` for itself. Checked after
    // the repeat suffix, which can make `SQLite` into `SQLite_2`.
    while name.to_ascii_lowercase().starts_with("sqlite_") {
        name = taken.unique(format!("_{name}"));
    }
    let source = (name != input.name).then(|| input.name.clone());

    // SQLite needs at least one column; an empty sheet gets column A.
    let width = input.width().max(1);
    let data = input.data_rows();
    let mut column_names = Names::default();
    let columns = input
        .raw_headers(width)
        .into_iter()
        .enumerate()
        .map(|(i, raw)| {
            let col = input.first_column.saturating_add(i as u32);
            let letter = column_to_letters(col);
            let cleaned = raw.as_deref().map(clean_name).unwrap_or_default();
            let base = if cleaned.is_empty() {
                letter.clone()
            } else {
                cleaned
            };
            let name = column_names.unique(base);
            let header = raw.filter(|r| *r != name);
            let cells = || data.iter().filter_map(|row| row.get(i));
            let affinity = Affinity::infer(cells());
            let errors = cells()
                .filter(|c| matches!(c, QueryCell::Value(CellValue::Error(_))))
                .count();
            ColumnInfo {
                name,
                letter,
                header,
                affinity,
                errors,
            }
        })
        .collect();
    TableInfo {
        name,
        source,
        columns,
        rows: data.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[&str]) -> Vec<QueryCell> {
        cells
            .iter()
            .map(|c| {
                if c.is_empty() {
                    QueryCell::default()
                } else {
                    QueryCell::from(*c)
                }
            })
            .collect()
    }

    #[test]
    fn headers_empty_duplicate_and_cjk() {
        let input = TableInput::new(
            "Sheet1",
            vec![
                row(&["Name", "", "name", "地区", "Name_2", "  "]),
                row(&["a"]),
            ],
        );
        let info = plan_table(&input, 0, &mut Names::default());
        let names: Vec<&str> = info.columns.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Name", "B", "name_2", "地区", "Name_2_2", "F"]);
        assert_eq!(info.columns[2].header.as_deref(), Some("name"));
        assert_eq!(info.columns[0].header, None);
        assert_eq!(info.rows, 1);
        assert_eq!(info.source, None);
    }

    #[test]
    fn letters_follow_first_column() {
        let input = TableInput::new("T", vec![row(&["", "x"])]).with_first_column(2);
        let info = plan_table(&input, 0, &mut Names::default());
        assert_eq!(info.columns[0].name, "C");
        assert_eq!(info.columns[1].letter, "D");
    }

    #[test]
    fn table_names_are_made_usable() {
        let mut taken = Names::default();
        let a = plan_table(&TableInput::new("Data", vec![]), 0, &mut taken);
        let b = plan_table(&TableInput::new("DATA", vec![]), 1, &mut taken);
        let c = plan_table(&TableInput::new("sqlite_master", vec![]), 2, &mut taken);
        let d = plan_table(&TableInput::new(" \t", vec![]), 3, &mut taken);
        let e = plan_table(&TableInput::new("SQLite", vec![]), 4, &mut taken);
        let f = plan_table(&TableInput::new("sqlite", vec![]), 5, &mut taken);
        assert_eq!(a.name, "Data");
        assert_eq!(b.name, "DATA_2");
        assert_eq!(b.source.as_deref(), Some("DATA"));
        assert_eq!(c.name, "_sqlite_master");
        assert_eq!(d.name, "Table4");
        assert_eq!(e.name, "SQLite");
        assert_eq!(f.name, "_sqlite_2");
    }

    #[test]
    fn long_and_control_names_are_cleaned() {
        assert_eq!(clean_name("a\nb\0c"), "a b c");
        assert_eq!(clean_name(&"x".repeat(500)).chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn affinity_inference() {
        let cells = |v: Vec<QueryCell>| Affinity::infer(v.iter());
        assert_eq!(cells(vec![1.0.into(), 2.0.into()]), Affinity::Integer);
        assert_eq!(cells(vec![1.0.into(), 2.5.into()]), Affinity::Real);
        assert_eq!(
            cells(vec!["a".into(), QueryCell::default()]),
            Affinity::Text
        );
        assert_eq!(cells(vec![true.into(), false.into()]), Affinity::Boolean);
        assert_eq!(
            cells(vec![QueryCell::Date("2024-01-01".into())]),
            Affinity::Date
        );
        assert_eq!(cells(vec![1.0.into(), "a".into()]), Affinity::Mixed);
        assert_eq!(cells(vec![true.into(), 1.0.into()]), Affinity::Mixed);
        assert_eq!(cells(vec![]), Affinity::Empty);
        assert_eq!(
            cells(vec![
                CellValue::Error(carrysheet_model::ErrorKind::NA).into()
            ]),
            Affinity::Empty
        );
    }

    #[test]
    fn many_repeated_headers_stay_fast() {
        let header: Vec<QueryCell> = (0..2000).map(|_| QueryCell::from("x")).collect();
        let info = plan_table(
            &TableInput::new("T", vec![header]),
            0,
            &mut Names::default(),
        );
        assert_eq!(info.columns[1999].name, "x_2000");
    }

    #[test]
    fn quoting() {
        assert_eq!(quote_ident(r#"Sales "Q1""#), r#""Sales ""Q1""""#);
        assert_eq!(quote_ident("地区"), "\"地区\"");
    }
}
