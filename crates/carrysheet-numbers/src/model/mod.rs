//! The document model: sheets, tables and cells, read from the objects.

mod cell;
mod formula;
mod functions;
mod merge;
mod table;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::Arc;

pub use cell::{Cell, Date, DateParts, Formula, Value};
pub use merge::CellRange;
pub use table::{Row, Table};

use crate::container::{self, Budget, Limits, Parts};
use crate::error::NumbersError;
use crate::iwa::{Store, types};
use crate::plist;
use crate::proto::{tn, tst};
use formula::{TableRef, Tables};
use merge::Owners;
use table::{TableJob, read_table};

/// The object number of the document root.
const DOCUMENT_ID: u64 = 1;
/// At most this many warnings are kept; the rest are counted.
const MAX_WARNINGS: usize = 1000;

/// A Numbers document, read completely into memory.
///
/// ```no_run
/// use carrysheet_numbers::Document;
///
/// let doc = Document::open("Budget.numbers")?;
/// for sheet in doc.sheets() {
///     for table in sheet.tables() {
///         for row in table.rows() {
///             for value in row.values() {
///                 print!("{value:?}\t");
///             }
///             println!();
///         }
///     }
/// }
/// # Ok::<(), carrysheet_numbers::NumbersError>(())
/// ```
#[derive(Clone, Debug)]
pub struct Document {
    sheets: Vec<Sheet>,
    version: Option<String>,
    build_history: Vec<String>,
    warnings: Vec<Warning>,
    more_warnings: usize,
}

/// A sheet: a named page with tables on it.
#[derive(Clone, Debug)]
pub struct Sheet {
    name: String,
    tables: Vec<Table>,
}

impl Sheet {
    /// The sheet name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The tables on the sheet, in the order Numbers stores them.
    pub fn tables(&self) -> &[Table] {
        &self.tables
    }

    /// The first table with this name.
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name() == name)
    }
}

/// Something the reader could not read exactly, but that did not stop it.
/// Sheets and tables are counted from 1, as people count them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// The sheet, counted from 1, when the problem is in one.
    pub sheet: Option<usize>,
    /// The table on that sheet, counted from 1.
    pub table: Option<usize>,
    /// The cell as row and column, counted from 0.
    pub cell: Option<(u32, u32)>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(sheet) = self.sheet {
            write!(f, "sheet {sheet}")?;
            if let Some(table) = self.table {
                write!(f, ", table {table}")?;
            }
            if let Some((row, column)) = self.cell {
                write!(
                    f,
                    ", cell {}{}",
                    formula::column_name(column),
                    u64::from(row) + 1
                )?;
            }
            write!(f, ": ")?;
        }
        write!(f, "{}", self.message)
    }
}

/// Collects warnings, keeping the first [`MAX_WARNINGS`].
pub(crate) struct Warnings {
    list: Vec<Warning>,
    more: usize,
    /// Sheet and table (counted from 1) for each table index.
    places: Vec<(usize, usize)>,
}

impl Warnings {
    fn add(&mut self, warning: Warning) {
        if self.list.len() < MAX_WARNINGS {
            self.list.push(warning);
        } else {
            self.more += 1;
        }
    }

    pub(crate) fn document(&mut self, message: String) {
        self.add(Warning {
            sheet: None,
            table: None,
            cell: None,
            message,
        });
    }

    pub(crate) fn table(&mut self, table: usize, message: String) {
        let (sheet, number) = self.places.get(table).copied().unwrap_or_default();
        self.add(Warning {
            sheet: Some(sheet),
            table: Some(number),
            cell: None,
            message,
        });
    }

    pub(crate) fn cell(&mut self, table: usize, row: u32, column: u32, message: impl Into<String>) {
        let (sheet, number) = self.places.get(table).copied().unwrap_or_default();
        self.add(Warning {
            sheet: Some(sheet),
            table: Some(number),
            cell: Some((row, column)),
            message: message.into(),
        });
    }
}

/// What the whole document may still produce: cells, cell text, formula
/// nodes written out, and merged ranges. Everything the reader makes from
/// a stored thing that cells can share is charged here, once per cell, so
/// sharing cannot multiply it past the [`Limits`].
pub(crate) struct Spend {
    cells: usize,
    text: u64,
    formula_nodes: u64,
    merges: usize,
    used_cells: u64,
    limits: Limits,
}

impl Spend {
    fn new(limits: &Limits) -> Self {
        Self {
            cells: limits.max_cells,
            text: limits.max_text_bytes,
            formula_nodes: limits.max_document_formula_nodes,
            merges: limits.max_document_merges,
            used_cells: limits.max_used_cells,
            limits: limits.clone(),
        }
    }

    pub(crate) fn cell(&mut self) -> Result<(), NumbersError> {
        self.cells = self.cells.checked_sub(1).ok_or(NumbersError::TooLarge {
            what: "non-empty cells",
            limit: self.limits.max_cells as u64,
        })?;
        Ok(())
    }

    pub(crate) fn text(&mut self, bytes: usize) -> Result<(), NumbersError> {
        self.text = self
            .text
            .checked_sub(bytes as u64)
            .ok_or(NumbersError::TooLarge {
                what: "bytes of text in cells",
                limit: self.limits.max_text_bytes,
            })?;
        Ok(())
    }

    pub(crate) fn formula_nodes(&mut self, nodes: u64) -> Result<(), NumbersError> {
        self.formula_nodes =
            self.formula_nodes
                .checked_sub(nodes)
                .ok_or(NumbersError::TooLarge {
                    what: "formula parts read in the document",
                    limit: self.limits.max_document_formula_nodes,
                })?;
        Ok(())
    }

    /// Charges a table's used range: what reading all its rows walks.
    pub(crate) fn used_cells(&mut self, count: u64) -> Result<(), NumbersError> {
        self.used_cells = self
            .used_cells
            .checked_sub(count)
            .ok_or(NumbersError::TooLarge {
                what: "cells in the used ranges of the tables (from the first cell to the \
                       last row and column with content)",
                limit: self.limits.max_used_cells,
            })?;
        Ok(())
    }

    pub(crate) fn merges(&mut self, count: usize) -> Result<(), NumbersError> {
        self.merges = self
            .merges
            .checked_sub(count)
            .ok_or(NumbersError::TooLarge {
                what: "merged ranges in the document",
                limit: self.limits.max_document_merges as u64,
            })?;
        Ok(())
    }
}

/// The newest file format version the reader was tested with (Numbers
/// 15.4), and the oldest (Numbers 11.0).
const NEWEST_TESTED: [u32; 2] = [26, 4];
const OLDEST_TESTED: [u32; 2] = [11, 0];

/// A warning when the version is unknown or outside the tested range.
fn version_warning(version: Option<&str>) -> Option<String> {
    let Some(version) = version else {
        return Some(
            "the document does not say which Numbers version saved it; it was read like \
             one from Numbers 11.0 to 15.4"
                .to_owned(),
        );
    };
    let parts: Option<Vec<u32>> = version.split('.').map(|p| p.parse().ok()).collect();
    let Some(parts) = parts.filter(|p| !p.is_empty()) else {
        return Some("the document's file format version cannot be read".to_owned());
    };
    let major_minor = [parts[0], parts.get(1).copied().unwrap_or(0)];
    let (oldest, newest) = (
        format!("{}.{}", OLDEST_TESTED[0], OLDEST_TESTED[1]),
        format!("{}.{}", NEWEST_TESTED[0], NEWEST_TESTED[1]),
    );
    if major_minor > NEWEST_TESTED {
        Some(format!(
            "file format {} is newer than this reader was tested with ({newest}, \
             Numbers 15.4); some content may be missing",
            short(version)
        ))
    } else if major_minor < OLDEST_TESTED {
        Some(format!(
            "file format {} is older than this reader was tested with ({oldest}, \
             Numbers 11.0); some content may be missing",
            short(version)
        ))
    } else {
        None
    }
}

/// A version string cut to a sensible length for a message.
fn short(version: &str) -> &str {
    match version.char_indices().nth(32) {
        Some((at, _)) => &version[..at],
        None => version,
    }
}

impl Document {
    /// Opens a `.numbers` file or package folder with the default [`Limits`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, NumbersError> {
        Self::open_with_limits(path, &Limits::default())
    }

    /// Opens a `.numbers` file or package folder.
    pub fn open_with_limits(path: impl AsRef<Path>, limits: &Limits) -> Result<Self, NumbersError> {
        let mut budget = Budget::new(limits.max_total_bytes);
        let parts = container::read_path(path.as_ref(), limits, &mut budget)?;
        Self::from_parts(parts, limits, &mut budget)
    }

    /// Reads a `.numbers` zip file held in memory, with the default [`Limits`].
    /// (A package folder can only be read with [`Document::open`].)
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, NumbersError> {
        Self::from_bytes_with_limits(bytes, &Limits::default())
    }

    /// Reads a `.numbers` zip file held in memory.
    pub fn from_bytes_with_limits(bytes: &[u8], limits: &Limits) -> Result<Self, NumbersError> {
        let mut budget = Budget::new(limits.max_total_bytes);
        let parts = container::read_bytes(bytes, limits, &mut budget)?;
        Self::from_parts(parts, limits, &mut budget)
    }

    fn from_parts(
        parts: Parts,
        limits: &Limits,
        budget: &mut Budget,
    ) -> Result<Self, NumbersError> {
        let version = parts
            .properties
            .as_deref()
            .and_then(plist::parse)
            .and_then(|p| p.string_for("fileFormatVersion").map(str::to_owned))
            .filter(|v| !v.is_empty());
        let build_history = parts
            .build_history
            .as_deref()
            .and_then(plist::parse)
            .map(|p| p.strings())
            .unwrap_or_default();
        let parts_skipped = parts.skipped;
        let store = Store::load(parts.archives, limits, budget)?;
        let mut warnings = Warnings {
            list: Vec::new(),
            more: 0,
            places: Vec::new(),
        };
        for name in &parts_skipped {
            warnings.document(format!(
                "{name} is larger than {} KiB, too large to be real, and was not read",
                plist::MAX_INPUT >> 10
            ));
        }
        if let Some(warning) = version_warning(version.as_deref()) {
            warnings.document(warning);
        }
        let sheets = read_sheets(&store, limits, &mut warnings)?;
        Ok(Self {
            sheets,
            version,
            build_history,
            warnings: warnings.list,
            more_warnings: warnings.more,
        })
    }

    /// The sheets, in the order Numbers shows them.
    pub fn sheets(&self) -> &[Sheet] {
        &self.sheets
    }

    /// The first sheet with this name.
    pub fn sheet(&self, name: &str) -> Option<&Sheet> {
        self.sheets.iter().find(|s| s.name() == name)
    }

    /// The file format version Numbers recorded, such as `"14.2.2"`, when
    /// the document has a readable `Metadata/Properties.plist`.
    pub fn numbers_version(&self) -> Option<&str> {
        self.version.as_deref()
    }

    /// The app builds that saved the document, oldest first, such as
    /// `"M14.3-7042.0.76-4"`, from `Metadata/BuildVersionHistory.plist`.
    pub fn build_history(&self) -> &[String] {
        &self.build_history
    }

    /// Problems that did not stop reading: cells left empty because their
    /// data was damaged, formulas that are not decoded, and the like. At
    /// most 1000 are kept; [`Document::more_warnings`] counts the rest.
    pub fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    /// How many warnings were left out of [`Document::warnings`].
    pub fn more_warnings(&self) -> usize {
        self.more_warnings
    }
}

/// A table found on a sheet, before its cells are read.
struct Found {
    sheet: usize,
    model: tst::TableModelArchive,
}

fn read_sheets(
    store: &Store,
    limits: &Limits,
    warnings: &mut Warnings,
) -> Result<Vec<Sheet>, NumbersError> {
    let document: tn::DocumentArchive =
        store.get(DOCUMENT_ID, &types::DOCUMENT, || "the document".to_owned())?;

    // Table infos by the sheet that holds them, in package order, as
    // numbers-parser lists them.
    let mut infos: HashMap<u64, Vec<tst::TableInfoArchive>> = HashMap::new();
    for id in store.ids_of_kind(types::TABLE_INFO.ids) {
        let info: tst::TableInfoArchive = store.get(id, &types::TABLE_INFO, String::new)?;
        let parent = info.super_.parent.as_ref().map_or(0, |p| p.identifier);
        infos.entry(parent).or_default().push(info);
    }

    let mut sheets = Vec::new();
    let mut found: Vec<Found> = Vec::new();
    let mut seen_sheets = HashSet::new();
    let mut seen_models = HashSet::new();
    for sheet_ref in &document.sheets {
        let id = sheet_ref.identifier;
        // Form sheets (type 3) are data-entry views of a table, not sheets.
        if store
            .kind(id)
            .is_some_and(|k| !types::SHEET.ids.contains(&k))
        {
            continue;
        }
        if !seen_sheets.insert(id) {
            warnings.document(format!(
                "sheet object {id} is listed twice; it is read once"
            ));
            continue;
        }
        let number = sheets.len() + 1;
        let sheet: tn::SheetArchive = store.get(id, &types::SHEET, || format!("sheet {number}"))?;
        let mut table_number = 0;
        for info in infos.remove(&id).unwrap_or_default() {
            let model_id = info.table_model.identifier;
            if !seen_models.insert(model_id) {
                warnings.document(format!(
                    "table object {model_id} is shown twice; it is read once"
                ));
                continue;
            }
            table_number += 1;
            if found.len() >= limits.max_tables {
                return Err(NumbersError::TooLarge {
                    what: "tables in the document",
                    limit: limits.max_tables as u64,
                });
            }
            let model: tst::TableModelArchive = store.get(model_id, &types::TABLE_MODEL, || {
                format!("sheet {number}, table {table_number}")
            })?;
            warnings.places.push((number, table_number));
            found.push(Found {
                sheet: sheets.len(),
                model,
            });
        }
        sheets.push(Sheet {
            name: sheet.name,
            tables: Vec::new(),
        });
    }

    let owners = Owners::load(store, limits, warnings)?;
    // Names are shared with the formulas, never copied for each table.
    let sheet_names: Vec<Arc<str>> = sheets.iter().map(|s| Arc::from(s.name.as_str())).collect();
    let refs: Vec<TableRef> = found
        .iter()
        .map(|f| TableRef {
            sheet: f.sheet,
            sheet_name: Arc::clone(&sheet_names[f.sheet]),
            name: Arc::from(f.model.table_name.as_str()),
            uid: owners.table_uid(&f.model),
        })
        .collect();
    let tables = Tables::new(refs);
    let mut spend = Spend::new(limits);
    for (index, f) in found.iter().enumerate() {
        let (sheet_number, table_number) = warnings.places[index];
        let job = TableJob {
            store,
            limits,
            owners: &owners,
            tables: &tables,
            index,
            model: &f.model,
            label: format!("sheet {sheet_number}, table {table_number}"),
        };
        let table = read_table(&job, &mut spend, warnings)?;
        sheets[f.sheet].tables.push(table);
    }
    Ok(sheets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_outside_the_tested_range_warn() {
        assert_eq!(version_warning(Some("11.0.3")), None);
        assert_eq!(version_warning(Some("14.2.2")), None);
        assert_eq!(version_warning(Some("26.4.0")), None);
        assert_eq!(version_warning(Some("26.4")), None);
        assert!(version_warning(Some("26.5")).unwrap().contains("newer"));
        assert!(version_warning(Some("27.0.1")).unwrap().contains("newer"));
        assert!(version_warning(Some("10.3.9")).unwrap().contains("older"));
        assert!(version_warning(Some("x.y")).is_some());
        assert!(version_warning(None).is_some());
    }
}
