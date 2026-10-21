//! Workbooks, sheets and cells.

use std::collections::BTreeMap;
use std::fmt;

use crate::reference::{CellRef, MAX_COLS, Range, RefError, validate_sheet_name};
use crate::value::CellValue;

/// Problems changing a workbook.
#[derive(Debug, Clone, PartialEq)]
pub enum ModelError {
    /// A reference or sheet name is invalid.
    Ref(RefError),
    /// Another sheet already has this name (names are compared without case).
    DuplicateSheet(String),
    /// No sheet has this name.
    NoSuchSheet(String),
    /// A merged range would overlap one that is already there.
    OverlappingMerge { new: String, existing: String },
    /// A column width must be a finite number from 0 to 255.
    InvalidColumnWidth(f64),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::Ref(e) => write!(f, "{e}"),
            ModelError::DuplicateSheet(name) => {
                write!(f, "a sheet named \"{name}\" already exists")
            }
            ModelError::NoSuchSheet(name) => write!(f, "there is no sheet named \"{name}\""),
            ModelError::OverlappingMerge { new, existing } => {
                write!(f, "merged range {new} overlaps merged range {existing}")
            }
            ModelError::InvalidColumnWidth(w) => {
                write!(f, "column width {w} is not a number from 0 to 255")
            }
        }
    }
}

impl std::error::Error for ModelError {}

impl From<RefError> for ModelError {
    fn from(e: RefError) -> Self {
        ModelError::Ref(e)
    }
}

/// Which day serial number 0 or 1 means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DateSystem {
    /// Day 1 is 1900-01-01 (with Excel's 1900 leap-year bug). The default.
    #[default]
    Excel1900,
    /// Day 0 is 1904-01-01, used by old Mac files.
    Excel1904,
}

impl DateSystem {
    /// `1900` or `1904`.
    pub fn as_str(self) -> &'static str {
        match self {
            DateSystem::Excel1900 => "1900",
            DateSystem::Excel1904 => "1904",
        }
    }
}

/// One cell: its value, its formula (if any) and its number format.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Cell {
    /// The value; for a formula, the last computed result.
    pub value: CellValue,
    /// The formula as en-US A1 text without the leading `=`, such as `SUM(A1:A3)`.
    pub formula: Option<String>,
    /// The number format code, such as `0.00%`. `None` means General.
    pub number_format: Option<String>,
}

impl Cell {
    /// A cell holding a value.
    pub fn value(value: impl Into<CellValue>) -> Self {
        Cell {
            value: value.into(),
            ..Cell::default()
        }
    }

    /// A cell holding a formula, with no computed result yet. A leading `=` is removed.
    pub fn formula(formula: impl Into<String>) -> Self {
        let formula = formula.into();
        let formula = match formula.strip_prefix('=') {
            Some(rest) => rest.to_owned(),
            None => formula,
        };
        Cell {
            formula: Some(formula),
            ..Cell::default()
        }
    }

    /// The same cell with a number format.
    pub fn with_number_format(mut self, format: impl Into<String>) -> Self {
        self.number_format = Some(format.into());
        self
    }

    /// True when the cell has no value, no formula and no format, so it need not be stored.
    pub fn is_blank(&self) -> bool {
        self.value.is_empty() && self.formula.is_none() && self.number_format.is_none()
    }
}

/// One sheet. Cells are stored sparsely, in row-major order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sheet {
    name: String,
    cells: BTreeMap<(u32, u32), Cell>,
    merged: Vec<Range>,
    column_widths: BTreeMap<u32, f64>,
}

impl Sheet {
    /// An empty sheet. The name is checked against Excel's rules.
    pub fn new(name: impl Into<String>) -> Result<Self, ModelError> {
        let name = name.into();
        validate_sheet_name(&name)?;
        Ok(Sheet {
            name,
            ..Sheet::default()
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn set_name(&mut self, name: String) {
        self.name = name;
    }

    /// The cell at a 0-based row and column, if anything is stored there.
    pub fn get(&self, row: u32, col: u32) -> Option<&Cell> {
        self.cells.get(&(row, col))
    }

    /// The cell at a reference, if anything is stored there.
    pub fn cell(&self, at: CellRef) -> Option<&Cell> {
        self.get(at.row, at.col)
    }

    /// Stores a cell. A blank cell removes what was there.
    pub fn set(&mut self, row: u32, col: u32, cell: Cell) -> Result<(), ModelError> {
        CellRef::new(row, col)?;
        if cell.is_blank() {
            self.cells.remove(&(row, col));
        } else {
            self.cells.insert((row, col), cell);
        }
        Ok(())
    }

    /// Stores a cell at a reference.
    pub fn set_cell(&mut self, at: CellRef, cell: Cell) -> Result<(), ModelError> {
        self.set(at.row, at.col, cell)
    }

    /// Removes a cell and returns what was there.
    pub fn clear(&mut self, row: u32, col: u32) -> Option<Cell> {
        self.cells.remove(&(row, col))
    }

    /// Number of stored cells.
    pub fn cell_count(&self) -> usize {
        self.cells.len()
    }

    /// Stored cells in row-major order, with 0-based row and column.
    pub fn cells(&self) -> impl Iterator<Item = (u32, u32, &Cell)> {
        self.cells.iter().map(|(&(r, c), cell)| (r, c, cell))
    }

    /// Stored cells inside a range, in row-major order.
    pub fn cells_in(&self, range: Range) -> impl Iterator<Item = (u32, u32, &Cell)> {
        // A range built by hand may have its corners reversed; BTreeMap::range
        // panics when the start is after the end.
        let range = range.sorted();
        self.cells
            .range((range.start.row, 0)..=(range.end.row, MAX_COLS - 1))
            .filter(move |&(&(_, c), _)| c >= range.start.col && c <= range.end.col)
            .map(|(&(r, c), cell)| (r, c, cell))
    }

    /// The smallest range holding every stored cell, or `None` for an empty sheet.
    pub fn used_range(&self) -> Option<Range> {
        let (&(first_row, _), _) = self.cells.first_key_value()?;
        let (&(last_row, _), _) = self.cells.last_key_value()?;
        let (min_col, max_col) = self
            .cells
            .keys()
            .fold((u32::MAX, 0), |(lo, hi), &(_, c)| (lo.min(c), hi.max(c)));
        Some(Range::new(
            CellRef::new(first_row, min_col).ok()?,
            CellRef::new(last_row, max_col).ok()?,
        ))
    }

    /// Merged ranges, in the order they were added.
    pub fn merged_ranges(&self) -> &[Range] {
        &self.merged
    }

    /// Adds a merged range. Single cells are ignored; overlaps are refused.
    pub fn merge(&mut self, range: Range) -> Result<(), ModelError> {
        let range = range.sorted();
        if range.is_single_cell() {
            return Ok(());
        }
        if let Some(existing) = self.merged.iter().find(|m| m.overlaps(&range)) {
            return Err(ModelError::OverlappingMerge {
                new: range.to_string(),
                existing: existing.to_string(),
            });
        }
        self.merged.push(range);
        Ok(())
    }

    /// Removes the merged range that starts at a cell. Returns it if there was one.
    pub fn unmerge(&mut self, top_left: CellRef) -> Option<Range> {
        let i = self
            .merged
            .iter()
            .position(|m| m.start.same_cell(&top_left))?;
        Some(self.merged.remove(i))
    }

    /// The width of a 0-based column in characters, if one is set.
    pub fn column_width(&self, col: u32) -> Option<f64> {
        self.column_widths.get(&col).copied()
    }

    /// Sets (or with `None` clears) a column width in characters, 0 to 255.
    pub fn set_column_width(&mut self, col: u32, width: Option<f64>) -> Result<(), ModelError> {
        CellRef::new(0, col)?;
        match width {
            None => {
                self.column_widths.remove(&col);
            }
            Some(w) if w.is_finite() && (0.0..=255.0).contains(&w) => {
                self.column_widths.insert(col, w);
            }
            Some(w) => return Err(ModelError::InvalidColumnWidth(w)),
        }
        Ok(())
    }

    /// Columns with a set width, in order.
    pub fn column_widths(&self) -> impl Iterator<Item = (u32, f64)> {
        self.column_widths.iter().map(|(&c, &w)| (c, w))
    }
}

/// A name for a formula or range, such as `TaxRate` → `Sheet1!$B$1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinedName {
    pub name: String,
    /// What the name refers to, as en-US A1 formula text without `=`.
    pub refers_to: String,
    /// The index of the sheet it belongs to, or `None` for the whole workbook.
    pub sheet: Option<usize>,
    pub hidden: bool,
}

/// A part or record of the source file that the model does not understand,
/// kept as raw bytes so a save can write it back unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownPart {
    /// Where it came from: a package path such as `xl/vbaProject.bin`, or a
    /// record id such as `BIFF8:0x00EF`.
    pub origin: String,
    /// The content type, when the file format records one.
    pub content_type: Option<String>,
    pub data: Vec<u8>,
}

/// A workbook: sheets in order, defined names, the date system, and parts
/// kept for round-trip.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Workbook {
    sheets: Vec<Sheet>,
    pub defined_names: Vec<DefinedName>,
    pub date_system: DateSystem,
    /// Parts and records the model does not understand, written back on save.
    pub unknown_parts: Vec<UnknownPart>,
}

impl Workbook {
    /// A workbook with no sheets.
    pub fn new() -> Self {
        Workbook::default()
    }

    /// A workbook with one empty sheet, `Sheet1`.
    pub fn with_default_sheet() -> Self {
        let mut wb = Workbook::new();
        wb.sheets.push(Sheet {
            name: "Sheet1".into(),
            ..Sheet::default()
        });
        wb
    }

    pub fn sheets(&self) -> &[Sheet] {
        &self.sheets
    }

    pub fn sheets_mut(&mut self) -> &mut [Sheet] {
        &mut self.sheets
    }

    /// The index of a sheet by name, compared without case as Excel does.
    pub fn sheet_index(&self, name: &str) -> Option<usize> {
        self.sheets
            .iter()
            .position(|s| s.name.to_lowercase() == name.to_lowercase())
    }

    /// A sheet by name, compared without case.
    pub fn sheet(&self, name: &str) -> Option<&Sheet> {
        self.sheet_index(name).map(|i| &self.sheets[i])
    }

    /// A sheet by name, compared without case.
    pub fn sheet_mut(&mut self, name: &str) -> Option<&mut Sheet> {
        let i = self.sheet_index(name)?;
        Some(&mut self.sheets[i])
    }

    /// Adds an empty sheet at the end and returns it.
    pub fn add_sheet(&mut self, name: impl Into<String>) -> Result<&mut Sheet, ModelError> {
        let sheet = Sheet::new(name)?;
        self.push_sheet(sheet)
    }

    /// Adds a sheet built elsewhere at the end and returns it.
    pub fn push_sheet(&mut self, sheet: Sheet) -> Result<&mut Sheet, ModelError> {
        if self.sheet_index(sheet.name()).is_some() {
            return Err(ModelError::DuplicateSheet(sheet.name().to_owned()));
        }
        self.sheets.push(sheet);
        let last = self.sheets.len() - 1;
        Ok(&mut self.sheets[last])
    }

    /// Renames a sheet. Formulas that mention the old name are not changed here;
    /// that is the engine's job.
    pub fn rename_sheet(&mut self, from: &str, to: &str) -> Result<(), ModelError> {
        validate_sheet_name(to)?;
        let i = self
            .sheet_index(from)
            .ok_or_else(|| ModelError::NoSuchSheet(from.to_owned()))?;
        if let Some(j) = self.sheet_index(to)
            && j != i
        {
            return Err(ModelError::DuplicateSheet(to.to_owned()));
        }
        self.sheets[i].set_name(to.to_owned());
        Ok(())
    }

    /// Removes a sheet and returns it. Names scoped to it are removed too, and
    /// scopes of later sheets are shifted down.
    pub fn remove_sheet(&mut self, name: &str) -> Result<Sheet, ModelError> {
        let i = self
            .sheet_index(name)
            .ok_or_else(|| ModelError::NoSuchSheet(name.to_owned()))?;
        self.defined_names.retain(|n| n.sheet != Some(i));
        for n in &mut self.defined_names {
            if let Some(s) = n.sheet
                && s > i
            {
                n.sheet = Some(s - 1);
            }
        }
        Ok(self.sheets.remove(i))
    }

    /// Looks up a defined name, compared without case. A name scoped to
    /// `sheet` wins over a workbook-wide one.
    pub fn defined_name(&self, name: &str, sheet: Option<usize>) -> Option<&DefinedName> {
        let matches = |n: &&DefinedName| n.name.to_lowercase() == name.to_lowercase();
        if sheet.is_some()
            && let Some(local) = self
                .defined_names
                .iter()
                .filter(matches)
                .find(|n| n.sheet == sheet)
        {
            return Some(local);
        }
        self.defined_names
            .iter()
            .filter(matches)
            .find(|n| n.sheet.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reference::{MAX_ROWS, RangeShape};
    use crate::value::ErrorKind;

    fn r(s: &str) -> Range {
        Range::parse(s).unwrap()
    }

    #[test]
    fn sheet_set_get_clear() {
        let mut s = Sheet::new("Data").unwrap();
        s.set(0, 0, Cell::value(1.0)).unwrap();
        s.set(2, 3, Cell::value("x")).unwrap();
        assert_eq!(s.get(0, 0).unwrap().value, CellValue::Number(1.0));
        assert_eq!(
            s.cell(CellRef::parse("D3").unwrap()).unwrap().value.kind(),
            "text"
        );
        assert_eq!(s.cell_count(), 2);
        s.set(2, 3, Cell::default()).unwrap();
        assert_eq!(s.cell_count(), 1);
        assert!(s.clear(0, 0).is_some());
        assert!(s.used_range().is_none());
    }

    #[test]
    fn sheet_rejects_out_of_range_cells() {
        let mut s = Sheet::new("Data").unwrap();
        assert!(s.set(MAX_ROWS, 0, Cell::value(1.0)).is_err());
        assert!(s.set(0, MAX_COLS, Cell::value(1.0)).is_err());
        assert!(s.set(MAX_ROWS - 1, MAX_COLS - 1, Cell::value(1.0)).is_ok());
    }

    #[test]
    fn used_range_and_cells_in() {
        let mut s = Sheet::new("Data").unwrap();
        s.set(1, 2, Cell::value(1.0)).unwrap();
        s.set(4, 0, Cell::value(2.0)).unwrap();
        s.set(3, 5, Cell::formula("=A1+1")).unwrap();
        assert_eq!(s.used_range().unwrap().to_string(), "A2:F5");
        let inside: Vec<_> = s.cells_in(r("B1:F4")).map(|(r, c, _)| (r, c)).collect();
        assert_eq!(inside, vec![(1, 2), (3, 5)]);
        assert_eq!(s.get(3, 5).unwrap().formula.as_deref(), Some("A1+1"));
    }

    #[test]
    fn hand_built_reversed_ranges_do_not_panic() {
        let mut s = Sheet::new("Data").unwrap();
        s.set(1, 2, Cell::value(1.0)).unwrap();
        s.set(3, 5, Cell::value(2.0)).unwrap();
        let sorted = r("B1:F4");
        let reversed = Range {
            start: sorted.end,
            end: sorted.start,
            shape: sorted.shape,
        };
        let inside: Vec<_> = s.cells_in(reversed).map(|(r, c, _)| (r, c)).collect();
        assert_eq!(inside, vec![(1, 2), (3, 5)]);
        s.merge(reversed).unwrap();
        assert_eq!(s.merged_ranges()[0].to_string(), "B1:F4");
        assert!(s.merge(r("C2")).is_ok());
        assert!(s.merge(r("A1:B2")).is_err());
        let far = Range {
            start: CellRef::new(0, 0).unwrap(),
            end: CellRef {
                row: u32::MAX,
                col: u32::MAX,
                row_absolute: false,
                col_absolute: false,
            },
            shape: RangeShape::Cells,
        };
        assert_eq!(s.cells_in(far).count(), 2);
    }

    #[test]
    fn cells_iterate_row_major() {
        let mut s = Sheet::new("Data").unwrap();
        s.set(1, 0, Cell::value(3.0)).unwrap();
        s.set(0, 1, Cell::value(2.0)).unwrap();
        s.set(0, 0, Cell::value(1.0)).unwrap();
        let order: Vec<_> = s.cells().map(|(r, c, _)| (r, c)).collect();
        assert_eq!(order, vec![(0, 0), (0, 1), (1, 0)]);
    }

    #[test]
    fn number_format_and_error_values() {
        let c = Cell::value(0.25).with_number_format("0%");
        assert_eq!(c.number_format.as_deref(), Some("0%"));
        let e = Cell::value(ErrorKind::Div0);
        assert_eq!(e.value.to_string(), "#DIV/0!");
        assert!(!Cell::default().with_number_format("0").is_blank());
    }

    #[test]
    fn merges() {
        let mut s = Sheet::new("Data").unwrap();
        s.merge(r("A1:B2")).unwrap();
        s.merge(r("C1")).unwrap();
        assert_eq!(s.merged_ranges().len(), 1);
        assert!(matches!(
            s.merge(r("B2:C3")),
            Err(ModelError::OverlappingMerge { .. })
        ));
        s.merge(r("C3:D4")).unwrap();
        assert_eq!(s.unmerge(CellRef::parse("A1").unwrap()), Some(r("A1:B2")));
        assert_eq!(s.merged_ranges(), &[r("C3:D4")]);
        assert!(s.unmerge(CellRef::parse("A1").unwrap()).is_none());
    }

    #[test]
    fn column_widths() {
        let mut s = Sheet::new("Data").unwrap();
        assert_eq!(s.column_width(0), None);
        s.set_column_width(0, Some(12.5)).unwrap();
        s.set_column_width(3, Some(0.0)).unwrap();
        assert_eq!(s.column_width(0), Some(12.5));
        assert_eq!(s.column_widths().count(), 2);
        assert!(s.set_column_width(1, Some(f64::NAN)).is_err());
        assert!(s.set_column_width(1, Some(256.0)).is_err());
        assert!(s.set_column_width(MAX_COLS, Some(1.0)).is_err());
        s.set_column_width(0, None).unwrap();
        assert_eq!(s.column_width(0), None);
    }

    #[test]
    fn workbook_sheets_case_insensitive() {
        let mut wb = Workbook::new();
        wb.add_sheet("売上").unwrap();
        wb.add_sheet("Data").unwrap();
        assert!(matches!(
            wb.add_sheet("DATA"),
            Err(ModelError::DuplicateSheet(_))
        ));
        assert_eq!(wb.sheet_index("data"), Some(1));
        assert_eq!(wb.sheet("売上").unwrap().name(), "売上");
        wb.sheet_mut("data")
            .unwrap()
            .set(0, 0, Cell::value(true))
            .unwrap();
        assert_eq!(wb.sheets()[1].cell_count(), 1);
        assert!(wb.add_sheet("bad/name").is_err());
    }

    #[test]
    fn workbook_default_sheet_and_date_system() {
        let wb = Workbook::with_default_sheet();
        assert_eq!(wb.sheets()[0].name(), "Sheet1");
        assert_eq!(wb.date_system, DateSystem::Excel1900);
        assert_eq!(DateSystem::Excel1904.as_str(), "1904");
    }

    #[test]
    fn rename_sheet() {
        let mut wb = Workbook::new();
        wb.add_sheet("A").unwrap();
        wb.add_sheet("B").unwrap();
        wb.rename_sheet("a", "Alpha").unwrap();
        assert_eq!(wb.sheets()[0].name(), "Alpha");
        // Changing only the case of a name is allowed.
        wb.rename_sheet("alpha", "ALPHA").unwrap();
        assert!(matches!(
            wb.rename_sheet("ALPHA", "b"),
            Err(ModelError::DuplicateSheet(_))
        ));
        assert!(matches!(
            wb.rename_sheet("nope", "x"),
            Err(ModelError::NoSuchSheet(_))
        ));
        assert!(wb.rename_sheet("B", "").is_err());
    }

    #[test]
    fn remove_sheet_fixes_name_scopes() {
        let mut wb = Workbook::new();
        for n in ["A", "B", "C"] {
            wb.add_sheet(n).unwrap();
        }
        let name = |n: &str, sheet| DefinedName {
            name: n.into(),
            refers_to: "1".into(),
            sheet,
            hidden: false,
        };
        wb.defined_names = vec![name("g", None), name("b", Some(1)), name("c", Some(2))];
        wb.remove_sheet("B").unwrap();
        let names: Vec<_> = wb
            .defined_names
            .iter()
            .map(|n| (n.name.as_str(), n.sheet))
            .collect();
        assert_eq!(names, vec![("g", None), ("c", Some(1))]);
        assert!(wb.remove_sheet("B").is_err());
    }

    #[test]
    fn defined_name_lookup_prefers_local() {
        let mut wb = Workbook::new();
        wb.add_sheet("A").unwrap();
        wb.defined_names.push(DefinedName {
            name: "Rate".into(),
            refers_to: "A!$B$1".into(),
            sheet: None,
            hidden: false,
        });
        wb.defined_names.push(DefinedName {
            name: "rate".into(),
            refers_to: "A!$C$1".into(),
            sheet: Some(0),
            hidden: false,
        });
        assert_eq!(wb.defined_name("RATE", None).unwrap().refers_to, "A!$B$1");
        assert_eq!(
            wb.defined_name("RATE", Some(0)).unwrap().refers_to,
            "A!$C$1"
        );
        assert!(wb.defined_name("other", Some(0)).is_none());
    }

    #[test]
    fn unknown_parts_are_kept() {
        let mut wb = Workbook::with_default_sheet();
        wb.unknown_parts.push(UnknownPart {
            origin: "xl/vbaProject.bin".into(),
            content_type: Some("application/vnd.ms-office.vbaProject".into()),
            data: vec![1, 2, 3],
        });
        let copy = wb.clone();
        assert_eq!(copy.unknown_parts, wb.unknown_parts);
    }

    #[test]
    fn formula_strips_leading_equals_once() {
        assert_eq!(Cell::formula("=1+1").formula.as_deref(), Some("1+1"));
        assert_eq!(
            Cell::formula("SUM(A:A)").formula.as_deref(),
            Some("SUM(A:A)")
        );
        assert_eq!(Cell::formula("==1").formula.as_deref(), Some("=1"));
    }
}
