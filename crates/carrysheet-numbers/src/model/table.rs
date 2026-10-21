//! Tables: size, cells and merged ranges.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::cell::{Cell, Date, Formula, Value, kind, parse_stored};
use super::formula::{self, Tables};
use super::merge::{CellRange, Owners, table_merges};
use super::{Spend, Warnings};
use crate::container::Limits;
use crate::error::NumbersError;
use crate::iwa::{Store, types};
use crate::proto::{tst, tswp};

/// A table: a grid of cells with a name, inside a sheet.
#[derive(Clone, Debug)]
pub struct Table {
    pub(crate) name: String,
    pub(crate) row_count: u32,
    pub(crate) column_count: u32,
    pub(crate) header_rows: u32,
    pub(crate) header_columns: u32,
    pub(crate) footer_rows: u32,
    /// Non-empty cells by row, sorted by column.
    pub(crate) cells: BTreeMap<u32, Vec<(u32, Cell)>>,
    /// One more than the last column with content in any row.
    pub(crate) used_columns: u32,
    pub(crate) merges: Vec<CellRange>,
}

static EMPTY: Value = Value::Empty;

impl Table {
    /// The table name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The number of rows, including header and footer rows.
    pub fn row_count(&self) -> u32 {
        self.row_count
    }

    /// The number of columns, including header columns.
    pub fn column_count(&self) -> u32 {
        self.column_count
    }

    /// How many of the first rows are header rows.
    pub fn header_rows(&self) -> u32 {
        self.header_rows
    }

    /// How many of the first columns are header columns.
    pub fn header_columns(&self) -> u32 {
        self.header_columns
    }

    /// How many of the last rows are footer rows.
    pub fn footer_rows(&self) -> u32 {
        self.footer_rows
    }

    /// The cell at a row and column, counted from 0, or `None` when the
    /// cell is empty, hidden by a merge, or outside the table.
    pub fn cell(&self, row: u32, column: u32) -> Option<&Cell> {
        let cells = self.cells.get(&row)?;
        let at = cells.binary_search_by_key(&column, |(c, _)| *c).ok()?;
        Some(&cells[at].1)
    }

    /// The value at a row and column; [`Value::Empty`] for an empty cell.
    pub fn value(&self, row: u32, column: u32) -> &Value {
        self.cell(row, column).map_or(&EMPTY, Cell::value)
    }

    /// The used range of the table, as rows and columns: one more than the
    /// last row and the last column with a non-empty cell. It is the box
    /// from the first cell to the content furthest down and furthest right,
    /// so it can hold many empty cells; [`Limits::max_used_cells`] bounds it
    /// over the document. Empty rows and columns at the end are left out;
    /// the declared size ([`Table::row_count`], [`Table::column_count`])
    /// can be larger.
    ///
    /// [`Limits::max_used_cells`]: crate::Limits::max_used_cells
    pub fn used_size(&self) -> (u32, u32) {
        let rows = self.cells.last_key_value().map_or(0, |(&r, _)| r + 1);
        (rows, self.used_columns)
    }

    /// Every row of the used range ([`Table::used_size`]), empty rows
    /// included, each with one entry for each column of the used range.
    /// To walk only rows with content, use [`Table::non_empty_rows`].
    pub fn rows(&self) -> impl Iterator<Item = Row<'_>> + '_ {
        let (rows, columns) = self.used_size();
        (0..rows).map(move |index| Row {
            index,
            width: columns,
            cells: self.cells.get(&index).map_or(&[][..], Vec::as_slice),
        })
    }

    /// Only the rows that have at least one non-empty cell.
    pub fn non_empty_rows(&self) -> impl Iterator<Item = Row<'_>> + '_ {
        self.cells.iter().map(move |(&index, cells)| Row {
            index,
            width: self.used_columns,
            cells,
        })
    }

    /// The merged ranges, in the order Numbers stores them. The top-left
    /// cell of a range holds its content; the other cells read as empty.
    pub fn merged_ranges(&self) -> &[CellRange] {
        &self.merges
    }

    /// The merged range that covers a cell, if any.
    pub fn merge_at(&self, row: u32, column: u32) -> Option<&CellRange> {
        self.merges.iter().find(|m| m.contains(row, column))
    }
}

/// One row of a [`Table`].
#[derive(Clone, Copy, Debug)]
pub struct Row<'a> {
    index: u32,
    width: u32,
    cells: &'a [(u32, Cell)],
}

impl<'a> Row<'a> {
    /// The row number, counted from 0.
    pub fn index(&self) -> u32 {
        self.index
    }

    /// The cell in a column, or `None` when it is empty.
    pub fn cell(&self, column: u32) -> Option<&'a Cell> {
        let at = self.cells.binary_search_by_key(&column, |(c, _)| *c).ok()?;
        Some(&self.cells[at].1)
    }

    /// One entry for each used column of the table, `None` for empty cells.
    pub fn cells(&self) -> impl Iterator<Item = Option<&'a Cell>> + 'a {
        let mut next = self.cells.iter().peekable();
        (0..self.width).map(move |column| next.next_if(|(c, _)| *c == column).map(|(_, cell)| cell))
    }

    /// One value for each used column of the table.
    pub fn values(&self) -> impl Iterator<Item = &'a Value> + 'a {
        self.cells().map(|c| c.map_or(&EMPTY, Cell::value))
    }

    /// The non-empty cells with their column numbers.
    pub fn non_empty(&self) -> impl Iterator<Item = (u32, &'a Cell)> + 'a {
        self.cells.iter().map(|(c, cell)| (*c, cell))
    }
}

/// Everything needed to read one table.
pub(crate) struct TableJob<'a> {
    pub(crate) store: &'a Store,
    pub(crate) limits: &'a Limits,
    pub(crate) owners: &'a Owners,
    pub(crate) tables: &'a Tables,
    pub(crate) index: usize,
    pub(crate) model: &'a tst::TableModelArchive,
    pub(crate) label: String,
}

type Entries = HashMap<u32, tst::table_data_list::ListEntry>;

/// Entries of a table data list (strings, formulas, rich text), by key,
/// including the entries kept in separate segments.
fn data_list(job: &TableJob<'_>, reference: Option<u64>) -> Result<Entries, NumbersError> {
    let mut entries = HashMap::new();
    let Some(id) = reference.filter(|&id| id != 0) else {
        return Ok(entries);
    };
    let list: tst::TableDataList = job.store.get(id, &types::DATA_LIST, || job.label.clone())?;
    for entry in list.entries {
        entries.insert(entry.key, entry);
    }
    for segment in list.segments {
        let segment: tst::TableDataListSegment =
            job.store
                .get(segment.identifier, &types::DATA_LIST_SEGMENT, || {
                    job.label.clone()
                })?;
        for entry in segment.entries {
            entries.insert(entry.key, entry);
        }
    }
    Ok(entries)
}

/// A formula compiled once for the table, with its text when that is the
/// same in every cell, or why it cannot be read.
enum Prepared {
    Ready {
        formula: formula::Compiled,
        fixed: Option<Arc<str>>,
    },
    Failed(Arc<str>),
}

/// The lists a table's cells point into. Strings are shared between cells;
/// rich text and formulas are read once per key, when a cell first needs
/// them, and the result (or the reason they failed) is kept.
struct Lists {
    strings: HashMap<u32, Arc<str>>,
    rich: Entries,
    rich_done: HashMap<u32, Result<Arc<str>, Arc<str>>>,
    formulas: Entries,
    formulas_done: HashMap<u32, Prepared>,
}

/// The default tile height in rows.
const TILE_SIZE: u32 = 256;

pub(crate) fn read_table(
    job: &TableJob<'_>,
    spend: &mut Spend,
    warnings: &mut Warnings,
) -> Result<Table, NumbersError> {
    let model = job.model;
    let limits = job.limits;
    let (rows, columns) = (model.number_of_rows, model.number_of_columns);
    if rows > limits.max_rows {
        return Err(NumbersError::TooLarge {
            what: "rows in one table",
            limit: u64::from(limits.max_rows),
        });
    }
    if columns > limits.max_columns {
        return Err(NumbersError::TooLarge {
            what: "columns in one table",
            limit: u64::from(limits.max_columns),
        });
    }
    let store = job.store;
    let bds = &model.base_data_store;
    let strings = data_list(job, Some(bds.string_table.identifier))?
        .into_iter()
        .map(|(key, entry)| (key, Arc::from(entry.string.unwrap_or_default())))
        .collect();
    let mut lists = Lists {
        strings,
        rich: data_list(job, bds.rich_text_table.as_ref().map(|r| r.identifier))?,
        rich_done: HashMap::new(),
        formulas: data_list(job, Some(bds.formula_table.identifier))?,
        formulas_done: HashMap::new(),
    };

    let tile_size = match bds.tiles.tile_size {
        Some(0) | None => TILE_SIZE,
        Some(n) => n,
    };
    let mut cells: BTreeMap<u32, Vec<(u32, Cell)>> = BTreeMap::new();
    for tile_ref in &bds.tiles.tiles {
        let tile: tst::Tile =
            store.get(tile_ref.tile.identifier, &types::TILE, || job.label.clone())?;
        if !tile.last_saved_in_bnc.unwrap_or(false) {
            return Err(NumbersError::Unsupported {
                feature: "the older cell storage of tables (pre-BNC tiles)",
            });
        }
        for info in &tile.row_infos {
            let row =
                u64::from(tile_ref.tileid) * u64::from(tile_size) + u64::from(info.tile_row_index);
            let Some(row) = u32::try_from(row).ok().filter(|&r| r < rows) else {
                warnings.table(
                    job.index,
                    format!("row {} is outside the table and was skipped", row + 1),
                );
                continue;
            };
            let buffer = info.cell_storage_buffer.as_deref().unwrap_or(&[]);
            let wide = info.has_wide_offsets.unwrap_or(false);
            let offsets: Vec<i64> = info
                .cell_offsets
                .as_deref()
                .unwrap_or(&[])
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&b| {
                    let o = i64::from(i16::from_le_bytes(b));
                    if wide { o * 4 } else { o }
                })
                .collect();
            let present = offsets
                .iter()
                .take(columns as usize)
                .filter(|&&o| o >= 0)
                .count();
            let mut row_cells = Vec::with_capacity(present);
            for (col, &start) in offsets.iter().enumerate().take(columns as usize) {
                if start < 0 {
                    continue;
                }
                let end = offsets[col + 1..]
                    .iter()
                    .find(|&&o| o >= 0)
                    .copied()
                    .unwrap_or(buffer.len() as i64);
                let col = col as u32;
                let Some(bytes) = usize::try_from(start)
                    .ok()
                    .zip(usize::try_from(end).ok())
                    .and_then(|(s, e)| buffer.get(s..e))
                else {
                    warnings.cell(
                        job.index,
                        row,
                        col,
                        "the cell data is outside its row buffer",
                    );
                    continue;
                };
                let Some(cell) = read_cell(job, bytes, row, col, &mut lists, spend, warnings)?
                else {
                    continue;
                };
                spend.cell()?;
                row_cells.push((col, cell));
            }
            if !row_cells.is_empty() {
                // A later tile with the same row replaces the earlier one.
                cells.insert(row, row_cells);
            }
        }
    }

    let merges = checked_merges(job, rows, columns, &mut cells, spend, warnings)?;
    let used_columns = cells
        .values()
        .filter_map(|row| row.last().map(|(c, _)| c + 1))
        .max()
        .unwrap_or(0);
    let used_rows = cells.last_key_value().map_or(0, |(&r, _)| r + 1);
    spend.used_cells(u64::from(used_rows) * u64::from(used_columns))?;
    for row_cells in cells.values_mut() {
        // Hidden merged cells may have left room behind.
        row_cells.shrink_to_fit();
    }
    Ok(Table {
        name: model.table_name.clone(),
        row_count: rows,
        column_count: columns,
        header_rows: model.number_of_header_rows.unwrap_or(0),
        header_columns: model.number_of_header_columns.unwrap_or(0),
        footer_rows: model.number_of_footer_rows.unwrap_or(0),
        cells,
        used_columns,
        merges,
    })
}

/// Reads one cell. A damaged cell is a warning and `None`; only a limit
/// for the whole document is an error.
fn read_cell(
    job: &TableJob<'_>,
    bytes: &[u8],
    row: u32,
    col: u32,
    lists: &mut Lists,
    spend: &mut Spend,
    warnings: &mut Warnings,
) -> Result<Option<Cell>, NumbersError> {
    let stored = match parse_stored(bytes) {
        Ok(stored) => stored,
        Err(why) => {
            warnings.cell(job.index, row, col, why);
            return Ok(None);
        }
    };
    let mut problem = |what: &str| warnings.cell(job.index, row, col, what.to_owned());
    let finite = |v: Option<f64>| v.filter(|v| v.is_finite());
    let value = match stored.kind {
        kind::EMPTY => Value::Empty,
        kind::NUMBER | kind::CURRENCY => match finite(stored.decimal) {
            Some(n) => Value::Number(n),
            None => {
                problem("the number is missing or not finite");
                Value::Empty
            }
        },
        kind::TEXT => match stored.string_id.and_then(|k| lists.strings.get(&k)) {
            Some(text) => Value::Text(Arc::clone(text)),
            None => {
                problem("the text is missing from the string list");
                Value::Empty
            }
        },
        kind::DATE => match finite(stored.seconds) {
            Some(s) => Value::Date(Date::from_seconds_since_2001(s)),
            None => {
                problem("the date is missing or not finite");
                Value::Empty
            }
        },
        kind::BOOL => match stored.double {
            Some(d) => Value::Bool(d > 0.0),
            None => {
                problem("the true/false value is missing");
                Value::Empty
            }
        },
        kind::DURATION => match finite(stored.double) {
            Some(d) => Value::Duration(d),
            None => {
                problem("the duration is missing or not finite");
                Value::Empty
            }
        },
        kind::ERROR => Value::Error,
        kind::RICH_TEXT => match stored.rich_id.map(|k| rich_text(job, lists, k)) {
            Some(Ok(text)) => Value::Text(text),
            Some(Err(why)) => {
                problem(&why);
                Value::Empty
            }
            None => {
                problem("the rich text is missing from the rich text list");
                Value::Empty
            }
        },
        other => {
            problem(&format!("cell type {other} is unknown"));
            Value::Empty
        }
    };
    if let Value::Text(text) = &value {
        spend.text(text.len())?;
    }
    let formula = match stored.formula_id {
        Some(id) => Some(formula_of(job, lists, id, row, col, spend, warnings)?),
        None => None,
    };
    if value == Value::Empty && formula.is_none() {
        return Ok(None);
    }
    Ok(Some(Cell { value, formula }))
}

/// The plain text of a rich text entry, read once per key.
fn rich_text(job: &TableJob<'_>, lists: &mut Lists, key: u32) -> Result<Arc<str>, Arc<str>> {
    if let Some(done) = lists.rich_done.get(&key) {
        return done.clone();
    }
    let result = match lists.rich.get(&key) {
        None => Err(Arc::from(
            "the rich text is missing from the rich text list",
        )),
        Some(entry) => {
            let id = entry.rich_text_payload.as_ref().map_or(0, |r| r.identifier);
            read_rich_text(job, id)
                .map_err(|e| Arc::from(format!("the rich text cannot be read: {}", e.what())))
        }
    };
    lists.rich_done.insert(key, result.clone());
    result
}

fn read_rich_text(job: &TableJob<'_>, id: u64) -> Result<Arc<str>, NumbersError> {
    let payload: tst::RichTextPayloadArchive =
        job.store
            .get(id, &types::RICH_TEXT_PAYLOAD, || job.label.clone())?;
    let storage: tswp::StorageArchive =
        job.store
            .get(payload.storage.identifier, &types::STORAGE, || {
                job.label.clone()
            })?;
    Ok(Arc::from(
        storage.text.into_iter().next().unwrap_or_default(),
    ))
}

/// The formula of a cell. The stored formula is compiled once per table;
/// writing it out for each cell is charged to the document's formula and
/// text budgets.
#[allow(clippy::too_many_arguments)]
fn formula_of(
    job: &TableJob<'_>,
    lists: &mut Lists,
    id: u32,
    row: u32,
    col: u32,
    spend: &mut Spend,
    warnings: &mut Warnings,
) -> Result<Formula, NumbersError> {
    if !lists.formulas_done.contains_key(&id) {
        let compiled = match lists.formulas.get(&id).and_then(|e| e.formula.as_ref()) {
            None => Prepared::Failed(Arc::from(format!(
                "formula {id} is missing from the formula list"
            ))),
            Some(f) => {
                spend.formula_nodes(f.ast_node_array.ast_node.len() as u64)?;
                match formula::compile(f, job.index, job.tables, job.limits.max_formula_nodes) {
                    Ok(formula) => {
                        // Text that is the same in every cell is written once.
                        let fixed = if formula.uses_cell() {
                            None
                        } else {
                            spend.formula_nodes(formula.size())?;
                            formula.text(0, 0).ok().map(Arc::from)
                        };
                        Prepared::Ready { formula, fixed }
                    }
                    Err(why) => Prepared::Failed(Arc::from(why)),
                }
            }
        };
        lists.formulas_done.insert(id, compiled);
    }
    let result = match &lists.formulas_done[&id] {
        Prepared::Failed(why) => Err(Arc::clone(why)),
        Prepared::Ready {
            fixed: Some(text), ..
        } => Ok(Arc::clone(text)),
        Prepared::Ready { formula, .. } => {
            spend.formula_nodes(formula.size())?;
            formula.text(row, col).map(Arc::from).map_err(Arc::from)
        }
    };
    Ok(match result {
        Ok(text) => {
            spend.text(text.len())?;
            Formula::Text(text)
        }
        Err(why) => {
            warnings.cell(
                job.index,
                row,
                col,
                format!("the formula is not decoded: {why}"),
            );
            Formula::NotDecoded(why)
        }
    })
}

/// Checks the merged ranges against the table, and hides the cells they
/// cover (all but the top-left one), as Numbers shows them.
fn checked_merges(
    job: &TableJob<'_>,
    rows: u32,
    columns: u32,
    cells: &mut BTreeMap<u32, Vec<(u32, Cell)>>,
    spend: &mut Spend,
    warnings: &mut Warnings,
) -> Result<Vec<CellRange>, NumbersError> {
    let found = table_merges(
        job.store,
        job.owners,
        job.model,
        &job.label,
        job.limits.max_merges,
    )?;
    spend.merges(found.len())?;
    let mut merges = Vec::with_capacity(found.len());
    let mut seen = HashSet::with_capacity(found.len());
    for m in found {
        if m.first_row > m.last_row
            || m.first_column > m.last_column
            || m.last_row >= rows
            || m.last_column >= columns
        {
            warnings.table(
                job.index,
                format!("merged range {m} is outside the table and was skipped"),
            );
            continue;
        }
        if seen.insert(m) {
            merges.push(m);
        }
    }
    // Each step visits one stored row of a range or one covered cell; the
    // bound keeps many large overlapping ranges from taking unbounded time.
    let limit = job.limits.max_cells.saturating_mul(4);
    let mut steps = 0usize;
    let mut hidden: Vec<(u32, u32)> = Vec::new();
    for m in &merges {
        for (&row, row_cells) in cells.range(m.first_row..=m.last_row) {
            let lo = row_cells.partition_point(|(c, _)| *c < m.first_column);
            let hi = row_cells.partition_point(|(c, _)| *c <= m.last_column);
            steps = steps.saturating_add(1 + hi.saturating_sub(lo));
            if steps > limit {
                return Err(NumbersError::TooLarge {
                    what: "merged range checks",
                    limit: limit as u64,
                });
            }
            for (col, _) in &row_cells[lo..hi.max(lo)] {
                if !(row == m.first_row && *col == m.first_column) {
                    hidden.push((row, *col));
                }
            }
        }
    }
    hidden.sort_unstable();
    hidden.dedup();
    for (row, group) in hidden.chunk_by(|a, b| a.0 == b.0).map(|g| (g[0].0, g)) {
        if let Some(row_cells) = cells.get_mut(&row) {
            let mut drop = group.iter().map(|&(_, c)| c).peekable();
            row_cells.retain(|(col, _)| {
                while drop.next_if(|&c| c < *col).is_some() {}
                drop.next_if(|&c| c == *col).is_none()
            });
        }
    }
    cells.retain(|_, row_cells| !row_cells.is_empty());
    Ok(merges)
}
