//! `edit`: operations parsed and checked once, then applied through
//! GridCraft's command layer, which checks protection, tables and array
//! formulas, and recalculates only what depends on the changed cells.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use gridcraft_engine::EngineError;
use gridcraft_engine::core::{CellRef, RangeRef, col_to_letters};
use gridcraft_engine::formula::{Expr, SheetSel, parse};
use gridcraft_engine::model::{Sheet, Workbook};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub use crate::args::Scalar;
use crate::args::{OPERATION_EXAMPLE, Operation, Operations, input, shown, text};
pub use crate::commands::limits::{MAX_BATCH_CELLS, MAX_FILL_CELLS, MAX_OPS};
use crate::error::{CommandError, ErrorCode};
use crate::open::Book;
use crate::range::SheetRange;
use crate::save::{EncodeOptions, Loss, finish_save, output_format};
use crate::text::escape;
use crate::view::value_json;
use crate::{Answer, Kernel, internal};

/// Most cell changes listed in an edit result; the rest are counted.
pub const MAX_LISTED_CHANGES: usize = 200;

/// What an operation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EditKind {
    SetValue,
    SetFormula,
    Clear,
    InsertRows,
    DeleteRows,
    InsertColumns,
    DeleteColumns,
    AddSheet,
    RenameSheet,
    DeleteSheet,
}

impl EditKind {
    /// The name clients send, such as `set_value`.
    pub fn name(self) -> String {
        json!(self).as_str().unwrap_or_default().to_owned()
    }
}

/// What an operation works on: a sheet name for sheet operations, else
/// cells, rows or columns (no sheet name means the first sheet).
#[derive(Debug, Clone, PartialEq)]
pub enum EditTarget {
    Range(SheetRange),
    Sheet(String),
}

/// One checked operation.
#[derive(Debug, Clone, PartialEq)]
pub struct EditOp {
    pub kind: EditKind,
    pub target: EditTarget,
    /// For `set_value`.
    pub value: Option<Scalar>,
    /// For `set_formula` (without `=`), `rename_sheet` (the new name).
    pub text: Option<String>,
    /// For row and column operations; `None` means the target's rows or columns.
    pub count: Option<u32>,
}

impl EditOp {
    /// The target as written back in results, such as `Data!B2:B9`.
    pub fn target_text(&self) -> String {
        match &self.target {
            EditTarget::Range(r) => r.to_string(),
            EditTarget::Sheet(s) => s.clone(),
        }
    }

    /// The cells a `set_value` or `set_formula` operation fills.
    fn fill_cells(&self) -> u64 {
        match (&self.target, self.kind) {
            (EditTarget::Range(r), EditKind::SetValue | EditKind::SetFormula) => r.range.count(),
            _ => 0,
        }
    }
}

fn op_error(i: usize, why: impl Into<String>, fix: impl Into<String>) -> CommandError {
    CommandError::new(ErrorCode::InvalidArgument, format!("ops[{i}] is not valid"), why, fix)
}

/// Reads the `ops` argument of `edit`. The argument layer checked its
/// shape; this checks what each operation needs, and the fill limits.
pub fn parse_ops(ops: &Value) -> Result<Vec<EditOp>, CommandError> {
    let items = Operations::deserialize(ops).map_err(|e| {
        let why = format!("ops is not a list of operations: {}", escape(&e.to_string(), 200));
        let fix = format!("Pass a list like [{OPERATION_EXAMPLE}].");
        CommandError::invalid_argument("ops", why, fix)
    })?;
    if items.is_empty() {
        return Err(CommandError::invalid_argument(
            "ops",
            "the list of operations is empty",
            "Add at least one operation, like {\"op\":\"set_value\",\"target\":\"B2\",\"value\":3}.",
        ));
    }
    if items.len() > MAX_OPS {
        return Err(CommandError::new(
            ErrorCode::LimitExceeded,
            format!("{} operations in one call", items.len()),
            format!("one edit call takes at most {MAX_OPS} operations"),
            "Split the operations over several calls, or fill a range with one set_value or set_formula.",
        ));
    }
    let ops: Vec<EditOp> = items
        .into_iter()
        .enumerate()
        .map(|(i, item)| parse_op(i, item))
        .collect::<Result<_, _>>()?;
    let filled: u64 = ops.iter().map(EditOp::fill_cells).sum();
    if filled > MAX_BATCH_CELLS {
        return Err(CommandError::new(
            ErrorCode::LimitExceeded,
            format!("the operations fill {filled} cells in one call"),
            format!(
                "set_value and set_formula fill at most {MAX_BATCH_CELLS} cells in one edit call, all operations together"
            ),
            "Split the operations over several edit calls, each writing to the file the last one wrote.",
        ));
    }
    Ok(ops)
}

fn parse_op(i: usize, item: Operation) -> Result<EditOp, CommandError> {
    let kind = item.op;
    // Excel counts UTF-16 units, GridCraft counts characters; GridCraft checks the rest.
    let check_new_name = |name: &str| {
        if name.chars().any(char::is_control) || name.encode_utf16().count() > 31 {
            let why = "the new sheet name has a control character or is too long for Excel";
            let fix = "Use 1 to 31 characters without : \\ / ? * [ ].";
            return Err(op_error(i, why, fix));
        }
        Ok(())
    };
    let is_sheet_op =
        matches!(kind, EditKind::AddSheet | EditKind::RenameSheet | EditKind::DeleteSheet);
    let target = if is_sheet_op {
        let name = item.target.trim();
        if name.is_empty() {
            return Err(op_error(
                i,
                "target is empty; sheet operations name a sheet",
                "Set target to the sheet name, like \"Q1\".",
            ));
        }
        if kind == EditKind::AddSheet {
            check_new_name(name)?;
        }
        EditTarget::Sheet(name.to_owned())
    } else {
        let range = SheetRange::parse(&item.target).map_err(|e| {
            op_error(
                i,
                format!("target is not a range: {e}"),
                "Write a range like Sheet1!B2:B9, B2, 'My sheet'!A:C or 5:7.",
            )
        })?;
        EditTarget::Range(range)
    };

    let (mut value, mut text) = (None, None);
    match (kind, item.value) {
        (EditKind::SetValue, Some(given)) => value = Some(given),
        (EditKind::SetValue, None) => {
            return Err(op_error(
                i,
                "set_value needs a value",
                "Add \"value\": text, a number or true/false. To empty cells, use op clear.",
            ));
        }
        (EditKind::SetFormula, Some(Scalar::Text(formula))) => {
            let formula = formula.trim();
            let formula = formula.strip_prefix('=').unwrap_or(formula).trim();
            if formula.is_empty() {
                return Err(op_error(i, "the formula is empty", "Add \"value\": \"SUM(B2:B9)\"."));
            }
            check_formula(i, formula)?;
            text = Some(formula.to_owned());
        }
        (EditKind::SetFormula, _) => {
            return Err(op_error(
                i,
                "set_formula needs the formula as text in value",
                "Add \"value\": \"SUM(B2:B9)\" (with or without =).",
            ));
        }
        (EditKind::RenameSheet, Some(Scalar::Text(name))) => {
            let name = name.trim();
            check_new_name(name)?;
            text = Some(name.to_owned());
        }
        (EditKind::RenameSheet, _) => {
            return Err(op_error(
                i,
                "rename_sheet needs the new name as text in value",
                "Add \"value\": \"New name\".",
            ));
        }
        _ => {}
    }
    let op = EditOp { kind, target, value, text, count: item.count };
    let cells = op.fill_cells();
    if cells > MAX_FILL_CELLS {
        return Err(CommandError::new(
            ErrorCode::LimitExceeded,
            format!("ops[{i}] fills {cells} cells"),
            format!("{} fills at most {MAX_FILL_CELLS} cells per operation", kind.name()),
            "Give a smaller range, like B2:B1000 rather than B:B.",
        ));
    }
    Ok(op)
}

/// Functions an agent may not write, one per line with the reason. They
/// reach outside the file (the web, other files or programs, an outside
/// service), or run code. Matched as names, called or passed as a value.
pub(crate) const REFUSED_FUNCTIONS: &str = "\
WEBSERVICE: it fetches a web address
FILTERXML: it is used with WEBSERVICE to read fetched pages
IMPORTXML IMPORTHTML: it fetches a web page
IMPORTDATA: it fetches a file from the web
STOCKHISTORY GOOGLEFINANCE: it fetches data from an online service
COPILOT: it sends cell data to an online AI service
PY: it runs Python code (in Microsoft's cloud)
CALL: it calls a function in a program library (DLL)
REGISTER REGISTER.ID: it loads a function from a program library (DLL)
EXEC: it starts another program (an Excel 4 macro function)
DDE: it talks to another program over DDE
RTD: it reads live data from another program
SQL.REQUEST: it queries an outside database
HYPERLINK: it makes a link that can open files or web addresses
IMAGE: it downloads a picture from a web address
TRANSLATE: it sends cell text to an online translation service
DETECTLANGUAGE: it sends cell text to an online language service
IMPORTRANGE: it reads data from another online spreadsheet
IMPORTFEED: it fetches a web feed
CUBEVALUE CUBEMEMBER CUBEMEMBERPROPERTY CUBERANKEDMEMBER CUBESET CUBESETCOUNT CUBEKPIMEMBER: \
it queries an outside data cube (OLAP) server";

/// Prefixes files put before function and parameter names. Any number of
/// them, in any order, is stripped before a name is compared.
const NAME_PREFIXES: &[&str] = &["_XLFN.", "_XLWS.", "_XLUDF.", "_XLPM.", "_XLETA."];

/// Why a refused function name is refused, or `None`. The name is
/// compared in upper case without the prefixes of [`NAME_PREFIXES`].
fn refused_name(word: &str) -> Option<String> {
    let mut name = word.to_ascii_uppercase();
    while let Some(p) = NAME_PREFIXES.iter().find(|p| name.starts_with(**p)) {
        name.drain(..p.len());
    }
    let mut table = REFUSED_FUNCTIONS.lines().filter_map(|l| l.split_once(": "));
    let (_, why) = table.find(|(names, _)| names.split(' ').any(|n| n == name))?;
    Some(format!("{name} is refused: {why}"))
}

/// A sheet name that names another file, as `'C:\x\[a.xlsx]S'!A1` does.
fn outside_file(sheets: &[&str]) -> Option<String> {
    let names_file = sheets.iter().any(|s| s.contains(['[', ']', '\\', '/']));
    names_file.then(|| "it reads another file through a quoted reference".to_owned())
}

/// Refuses a formula an agent may not write (docs/agents.md, "Dangerous
/// formulas"), read by GridCraft's parser as the kernel stores it, so text
/// in strings never counts. Refused: the functions of [`REFUSED_FUNCTIONS`],
/// called or passed as a value; sheet names that name a file; and what does
/// not parse, such as links to other workbooks (`[1]Sheet!A1`) and DDE.
pub fn check_formula(i: usize, formula: &str) -> Result<(), CommandError> {
    // The kernel closes up to 8 missing parentheses, as Excel does.
    let close = |n: usize| parse(&format!("{formula}{}", ")".repeat(n)));
    let found = (0..8).map(close).find(Result::is_ok);
    let parsed = found.unwrap_or_else(|| close(8));
    let expr = parsed.map_err(|e| {
        let e = escape(&e.to_string(), 100);
        CommandError::new(
            ErrorCode::InvalidArgument,
            format!("ops[{i}] has a formula Carrysheet cannot read"),
            format!("{e}; links to other workbooks and DDE are not allowed"),
            "Correct the formula (use , between arguments) and call again; nothing was saved.",
        )
    })?;
    let mut why = None;
    expr.walk(&mut |node| {
        let found = match node {
            Expr::Call(name, _) => refused_name(name),
            Expr::Name(name) => match name.rsplit_once('!') {
                Some((sheet, name)) => outside_file(&[sheet]).or_else(|| refused_name(name)),
                None => refused_name(name),
            },
            Expr::Struct(table) => refused_name(&table.table),
            Expr::Ref(reference) => match &reference.sheet {
                SheetSel::Named(sheet) => outside_file(&[sheet.as_str()]),
                SheetSel::Span(first, last) => outside_file(&[first.as_str(), last.as_str()]),
                SheetSel::Current => None,
            },
            _ => None,
        };
        if why.is_none() {
            why = found;
        }
    });
    why.map_or(Ok(()), |why| {
        Err(CommandError::new(
            ErrorCode::InvalidArgument,
            format!("ops[{i}] writes a formula Carrysheet does not let agents write"),
            why,
            "Write the value instead, or ask a person to add this formula in a spreadsheet app.",
        ))
    })
}

/// What one cell held before and after an edit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CellChange {
    pub sheet: String,
    pub cell: String,
    /// The value, or `{"formula", "value"}` for a formula; `null` when empty.
    pub before: Value,
    pub after: Value,
}

/// What one operation did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpResult {
    pub op: EditKind,
    pub target: String,
    /// A short note, like `3 rows inserted before row 5`.
    pub done: String,
}

/// What an edit did, for the change list and the review log.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct EditResult {
    pub ops: Vec<OpResult>,
    /// Cells the operations wrote, up to [`MAX_LISTED_CHANGES`].
    pub changes: Vec<CellChange>,
    /// All cells the operations wrote.
    pub changes_total: u64,
    /// Kept content the edit dropped, by feature, for the save report.
    #[serde(skip)]
    pub(crate) dropped: BTreeMap<&'static str, u64>,
}

/// Applies `ops` in order. On an error the book is put back as it was.
pub(crate) fn apply(book: &mut Book, ops: &[EditOp]) -> Result<EditResult, CommandError> {
    let start = book.doc()?.wb.clone();
    let active = start.active().map(|s| s.name.clone());
    let mut result = EditResult::default();
    let outcome =
        ops.iter().enumerate().try_for_each(|(i, op)| apply_one(book, i, op, &mut result));
    if let Err(e) = outcome {
        if let Some(d) = book.session.active_mut() {
            d.wb = start;
            d.calc.rebuild(&d.wb);
        }
        return Err(e);
    }
    // Filling a range moves the active sheet; the file keeps its own.
    if let Some(name) = active {
        let _ = book.session.execute_headless("sheet.activate", json!({ "sheet": name }));
    }
    // One-shot calls keep no undo history (record 0003, point 4).
    if let Some(d) = book.session.active_mut() {
        d.undo.clear();
        d.redo.clear();
        d.saved = d.wb.clone();
    }
    Ok(result)
}

/// Runs one GridCraft command for operation `i`.
fn execute(
    book: &mut Book,
    i: usize,
    op: &EditOp,
    id: &str,
    params: Value,
) -> Result<(), CommandError> {
    match book.session.execute_headless(id, params) {
        Ok(_) => Ok(()),
        Err(EngineError::Internal(_, detail)) => Err(internal("editing", &detail)),
        Err(e) => Err(CommandError::new(
            ErrorCode::InvalidArgument,
            format!("ops[{i}] ({}) could not be applied", op.kind.name()),
            escape(&e.to_string(), 200),
            "Change or remove this operation and call again; nothing was saved.",
        )),
    }
}

/// A sheet's index at this point of the batch; no name means the first sheet.
fn sheet(book: &Book, i: usize, name: Option<&str>) -> Result<usize, CommandError> {
    book.sheet_index(name).map_err(|mut e| {
        e.what = format!("ops[{i}]: {}", e.what);
        e
    })
}

/// What a cell holds, for the change list.
fn cell_state(sh: Option<&Sheet>, at: CellRef) -> Value {
    let Some(sh) = sh else { return Value::Null };
    let value = value_json(&sh.value(at));
    match sh.cell(at).and_then(|c| c.formula.as_ref()) {
        Some(f) => {
            json!({ "formula": format!("={}", f.text.trim_start_matches('=')), "value": value })
        }
        None => value,
    }
}

fn apply_one(
    book: &mut Book,
    i: usize,
    op: &EditOp,
    result: &mut EditResult,
) -> Result<(), CommandError> {
    let done = match (&op.target, op.kind) {
        (EditTarget::Sheet(name), EditKind::AddSheet) => {
            let n = book.wb()?.sheets.len();
            let params = json!({ "name": name, "before": n });
            execute(book, i, op, "home.insertSheet", params)?;
            format!("sheet added at the end ({} sheets now)", n + 1)
        }
        (EditTarget::Sheet(name), EditKind::RenameSheet) => {
            let si = sheet(book, i, Some(name))?;
            let new = op.text.clone().unwrap_or_default();
            let params = json!({ "sheet": si, "name": new });
            execute(book, i, op, "sheet.rename", params)?;
            format!("renamed to {new}")
        }
        (EditTarget::Sheet(name), EditKind::DeleteSheet) => {
            let si = sheet(book, i, Some(name))?;
            execute(book, i, op, "home.deleteSheet", json!({ "sheet": si }))?;
            "sheet deleted".to_owned()
        }
        (
            EditTarget::Range(target),
            EditKind::SetValue | EditKind::SetFormula | EditKind::Clear,
        ) => {
            let si = sheet(book, i, target.sheet.as_deref())?;
            write_cells(book, i, op, si, target, result)?
        }
        (
            EditTarget::Range(target),
            kind @ (EditKind::InsertRows
            | EditKind::DeleteRows
            | EditKind::InsertColumns
            | EditKind::DeleteColumns),
        ) => {
            let (si, r) = (sheet(book, i, target.sheet.as_deref())?, target.range);
            let (id, key, start, size) = match kind {
                EditKind::InsertRows => ("home.insertRows", "rows", r.start.row, r.height()),
                EditKind::DeleteRows => ("home.deleteRows", "rows", r.start.row, r.height()),
                EditKind::InsertColumns => ("home.insertColumns", "cols", r.start.col, r.width()),
                _ => ("home.deleteColumns", "cols", r.start.col, r.width()),
            };
            let is_rows = key == "rows";
            let word = if is_rows { "row" } else { "column" };
            // A whole-column target has no rows, and the reverse.
            if (r.is_full_rows(), r.is_full_cols()) == (!is_rows, is_rows) {
                return Err(CommandError::new(
                    ErrorCode::InvalidArgument,
                    format!("ops[{i}] ({}) needs {word}s", kind.name()),
                    format!("the target {} has no {word}s", op.target_text()),
                    "Use a target like 5:7 or B2:B9 for rows, and C:D or C1:D1 for columns.",
                ));
            }
            forget_kept(book, si, None, &mut result.dropped);
            let n = op.count.unwrap_or(size).max(1);
            let label = |at: u32| {
                if is_rows { at.saturating_add(1).to_string() } else { col_to_letters(at) }
            };
            let span = format!("{}:{}", label(start), label(start.saturating_add(n - 1)));
            execute(book, i, op, id, json!({ "sheet": si, key: span }))?;
            match kind {
                EditKind::InsertRows | EditKind::InsertColumns => {
                    format!("{n} {word}(s) inserted before {word} {}", label(start))
                }
                _ => format!("{word}(s) {span} deleted"),
            }
        }
        // parse_ops never makes these; an EditOp built by hand can.
        (_, kind) => {
            return Err(CommandError::invalid_argument(
                "ops",
                format!("ops[{i}]: {} does not fit its target", kind.name()),
                "Sheet operations take a sheet name as target; the others take a range.",
            ));
        }
    };
    result.ops.push(OpResult { op: op.kind, target: op.target_text(), done });
    Ok(())
}

/// `set_value`, `set_formula` and `clear`: writes the cells, lists what
/// changed, and says how many cells changed.
fn write_cells(
    book: &mut Book,
    i: usize,
    op: &EditOp,
    si: usize,
    target: &SheetRange,
    result: &mut EditResult,
) -> Result<String, CommandError> {
    let r = target.range;
    let before = book.doc()?.wb.clone();
    let before_sheet = before.sheets.get(si).map(Arc::as_ref);
    // A clear lists only the cells that hold something: a clear of A:A is
    // not a million changes.
    let cells: Vec<CellRef> = match (op.kind, before_sheet) {
        (_, None) => Vec::new(),
        (EditKind::Clear, Some(sh)) => sh.cells.iter_range(r).map(|(at, _)| at).collect(),
        _ => r.iter().collect(),
    };
    let (start, range) = (r.start.a1(), target.a1());
    match op.kind {
        EditKind::SetValue => {
            let values = values_of(op, &r, &before, si);
            let params = json!({ "sheet": si, "range": start, "values": values });
            execute(book, i, op, "range.setValues", params)?;
        }
        // One cell, or a range filled the way Excel fills it (relative
        // references move with each cell).
        EditKind::SetFormula => {
            let input = format!("={}", op.text.as_deref().unwrap_or(""));
            if r.is_single() {
                let params = json!({ "sheet": si, "cell": start, "input": input });
                execute(book, i, op, "cell.set", params)?;
            } else {
                let select = json!({ "sheet": si, "range": range, "active": start });
                execute(book, i, op, "selection.set", select)?;
                let params = json!({ "sheet": si, "range": range, "input": input });
                execute(book, i, op, "range.fill", params)?;
            }
        }
        _ => {
            let params = json!({ "sheet": si, "range": range });
            execute(book, i, op, "edit.clearContents", params)?;
        }
    }
    let after = book.doc()?.wb.clone();
    let after_sheet = after.sheets.get(si).map(Arc::as_ref);
    let name = after_sheet.map(|s| s.name.clone()).unwrap_or_default();
    let mut count = 0u64;
    for at in cells {
        let (b, a) = (cell_state(before_sheet, at), cell_state(after_sheet, at));
        if a == b {
            continue;
        }
        count += 1;
        result.changes_total += 1;
        if result.changes.len() < MAX_LISTED_CHANGES {
            result.changes.push(CellChange {
                sheet: name.clone(),
                cell: at.a1(),
                before: b,
                after: a,
            });
        }
    }
    // A formula typed into a cell formatted as Text stays text, as in
    // Excel. An agent asked for a formula, so that is an error.
    if op.kind == EditKind::SetFormula
        && let Some(sh) = after_sheet
        && let Some(at) = r.iter().find(|at| sh.cell(*at).is_none_or(|c| c.formula.is_none()))
    {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            format!("ops[{i}]: {} did not take the formula", at.a1()),
            "the cell is formatted as Text, so the formula would be stored as text",
            "Write the formula to a cell with another number format; nothing was saved.",
        ));
    }
    forget_kept(book, si, Some(&r), &mut result.dropped);
    Ok(format!("{count} cell(s) changed"))
}

/// The rows of values `set_value` writes, each with its JSON type. Text is
/// marked as text (a leading `'`), so `"3"` stays text and `"=A1"` is not
/// a formula; cells formatted as Text take it as is.
fn values_of(op: &EditOp, r: &RangeRef, wb: &Workbook, si: usize) -> Vec<Value> {
    let sh = wb.sheets.get(si);
    let is_text_format =
        |at: CellRef| sh.is_some_and(|sh| wb.styles.get(sh.style_id(at)).num_fmt.as_str() == "@");
    let value = op.value.clone().unwrap_or(Scalar::Text(String::new()));
    let cell = |at: CellRef| match &value {
        Scalar::Number(n) => json!(n),
        Scalar::Flag(b) => json!(b),
        Scalar::Text(t) if is_text_format(at) => json!(t),
        Scalar::Text(t) => json!(format!("'{t}")),
    };
    (r.start.row..=r.end.row)
        .map(|row| {
            let cells = r.start.col..=r.end.col;
            cells.map(|col| cell(CellRef::new(row, col))).collect()
        })
        .collect()
}

/// The feature of kept content that names cells GridCraft does not move (vendor README).
fn kept_feature(slot: &str) -> Option<&'static str> {
    match slot.split_once(':').map_or(slot, |(kind, _)| kind) {
        "scenarios" => Some("scenarios"),
        "ignoredErrors" => Some("ignored error flags"),
        "dataTable" => Some("what-if data tables"),
        "note" => Some("formatted note text"),
        _ => None,
    }
}

/// Drops kept content that would point at the wrong cells, counted in `dropped` for the
/// save report: all of it after rows or columns move (`within` is `None`), else the data
/// tables anchored in the range.
fn forget_kept(
    book: &mut Book,
    si: usize,
    within: Option<&RangeRef>,
    dropped: &mut BTreeMap<&'static str, u64>,
) {
    let Some(d) = book.session.active_mut() else {
        return;
    };
    let stale = |slot: &str| match within {
        None => kept_feature(slot),
        Some(r) => slot
            .strip_prefix("dataTable:")
            .and_then(CellRef::parse)
            .filter(|c| r.contains(*c))
            .and_then(|_| kept_feature(slot)),
    };
    let has_stale =
        d.wb.sheets
            .get(si)
            .and_then(|sh| sh.preserved.as_deref())
            .is_some_and(|p| p.elements.iter().any(|e| stale(&e.slot).is_some()));
    if !has_stale {
        return;
    }
    let wb = Arc::make_mut(&mut d.wb);
    if let Some(sh) = wb.sheets.get_mut(si)
        && let Some(p) = Arc::make_mut(sh).preserved.as_mut()
    {
        Arc::make_mut(p).elements.retain(|e| match stale(&e.slot) {
            Some(feature) => {
                *dropped.entry(feature).or_default() += 1;
                false
            }
            None => true,
        });
    }
}

/// Applies a batch of operations and saves the result.
/// The caller fills in `output` ([`crate::save::prepare_output`]).
pub(crate) fn edit(
    kernel: &Kernel,
    args: &Map<String, Value>,
    bytes: Vec<u8>,
) -> Result<Answer, CommandError> {
    let path = input(args)?;
    let ops = parse_ops(args.get("ops").unwrap_or(&Value::Null))?;
    let output = text(args, "output").map_or_else(|| path.clone(), PathBuf::from);
    let format = output_format(&output, None)?;
    let mut book = kernel.open_bytes(&path, bytes)?;
    let result = kernel.edit(&mut book, &ops)?;
    let mut encoded = kernel.encode(&book, &output, format, &EncodeOptions::default())?;
    let dropped = result.dropped.iter().map(|(f, n)| Loss::all(*f, *n));
    encoded.report.lost.extend(dropped);
    let mut data = Map::new();
    data.insert("file".into(), shown(&path));
    data.insert("output".into(), shown(&output));
    // ops, changes, changes_total.
    if let Value::Object(fields) = json!(result) {
        data.extend(fields);
    }
    finish_save(args, &output, encoded, data)
}
