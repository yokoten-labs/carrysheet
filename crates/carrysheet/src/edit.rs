//! `edit`: operations parsed and checked once, then applied through
//! GridCraft's command layer, which checks protection, tables and array
//! formulas, and recalculates only what depends on the changed cells.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gridcraft_engine::core::{CellRef, RangeRef, col_to_letters};
use gridcraft_engine::formula::{Expr, SheetSel, parse};
use gridcraft_engine::model::{Sheet, Workbook};
use gridcraft_engine::{EngineError, Session};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub use crate::args::Scalar;
use crate::args::{OPERATION_EXAMPLE, Operation, Operations, input, shown, text};
pub use crate::commands::limits::{MAX_BATCH_CELLS, MAX_FILL_CELLS, MAX_OPS};
use crate::error::{CommandError, ErrorCode};
use crate::open::Book;
use crate::range::{Shape, SheetRange, validate_sheet_name};
use crate::save::{EncodeOptions, finish_save, output_format};
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

    /// The range of a cell, row or column operation.
    pub fn range(&self) -> Option<&SheetRange> {
        match &self.target {
            EditTarget::Range(r) => Some(r),
            EditTarget::Sheet(_) => None,
        }
    }
}

fn op_error(i: usize, why: impl Into<String>, fix: impl Into<String>) -> CommandError {
    CommandError::new(
        ErrorCode::InvalidArgument,
        format!("ops[{i}] is not valid"),
        why,
        fix,
    )
}

/// Reads the `ops` argument of `edit`. The argument layer checked its
/// shape; this checks what each operation needs, and the fill limits.
pub fn parse_ops(ops: &Value) -> Result<Vec<EditOp>, CommandError> {
    let items = Operations::deserialize(ops).map_err(|e| {
        let why = format!(
            "ops is not a list of operations: {}",
            escape(&e.to_string(), 200)
        );
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
    let filled: u64 = ops
        .iter()
        .filter(|op| matches!(op.kind, EditKind::SetValue | EditKind::SetFormula))
        .map(|op| op.range().map_or(0, |r| r.range.count()))
        .sum();
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
    let check_new_name = |name: &str| {
        validate_sheet_name(name).map_err(|e| {
            op_error(
                i,
                format!("the new sheet name is not allowed: {e}"),
                "Use 1 to 31 characters without : \\ / ? * [ ].",
            )
        })
    };
    let is_sheet_op = matches!(
        kind,
        EditKind::AddSheet | EditKind::RenameSheet | EditKind::DeleteSheet
    );
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
                return Err(op_error(
                    i,
                    "the formula is empty",
                    "Add \"value\": \"SUM(B2:B9)\".",
                ));
            }
            match check_formula(formula) {
                None => {}
                Some(Refusal::Unreadable(why)) => {
                    return Err(CommandError::new(
                        ErrorCode::InvalidArgument,
                        format!("ops[{i}] has a formula Carrysheet cannot read"),
                        why,
                        "Correct the formula (use , between arguments) and call again; nothing was saved.",
                    ));
                }
                Some(Refusal::Refused(why)) => {
                    return Err(CommandError::new(
                        ErrorCode::InvalidArgument,
                        format!("ops[{i}] writes a formula Carrysheet does not let agents write"),
                        why,
                        "Write the value instead, or ask a person to add this formula in a spreadsheet app.",
                    ));
                }
            }
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
    let op = EditOp {
        kind,
        target,
        value,
        text,
        count: item.count,
    };
    let cells = op.range().map_or(0, |r| r.range.count());
    if matches!(kind, EditKind::SetValue | EditKind::SetFormula) && cells > MAX_FILL_CELLS {
        return Err(CommandError::new(
            ErrorCode::LimitExceeded,
            format!("ops[{i}] fills {cells} cells"),
            format!(
                "{} fills at most {MAX_FILL_CELLS} cells per operation",
                kind.name()
            ),
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
IMPORTXML: it fetches a web page
IMPORTDATA: it fetches a file from the web
STOCKHISTORY: it fetches data from an online service
COPILOT: it sends cell data to an online AI service
PY: it runs Python code (in Microsoft's cloud)
CALL: it calls a function in a program library (DLL)
REGISTER: it loads a function from a program library (DLL)
REGISTER.ID: it loads a function from a program library (DLL)
EXEC: it starts another program (an Excel 4 macro function)
DDE: it talks to another program over DDE
RTD: it reads live data from another program
SQL.REQUEST: it queries an outside database
HYPERLINK: it makes a link that can open files or web addresses
IMAGE: it downloads a picture from a web address
TRANSLATE: it sends cell text to an online translation service
DETECTLANGUAGE: it sends cell text to an online language service
IMPORTRANGE: it reads data from another online spreadsheet
IMPORTHTML: it fetches a web page
IMPORTFEED: it fetches a web feed
GOOGLEFINANCE: it fetches data from an online service
CUBEVALUE: it queries an outside data cube (OLAP) server
CUBEMEMBER: it queries an outside data cube (OLAP) server
CUBEMEMBERPROPERTY: it queries an outside data cube (OLAP) server
CUBERANKEDMEMBER: it queries an outside data cube (OLAP) server
CUBESET: it queries an outside data cube (OLAP) server
CUBESETCOUNT: it queries an outside data cube (OLAP) server
CUBEKPIMEMBER: it queries an outside data cube (OLAP) server";

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
    let (name, why) = table.find(|(refused, _)| *refused == name)?;
    Some(format!("{name} is refused: {why}"))
}

/// A sheet name that names another file, as `'C:\x\[a.xlsx]S'!A1` does.
fn outside_file(sheets: &[&str]) -> Option<String> {
    let names_file = sheets.iter().any(|s| s.contains(['[', ']', '\\', '/']));
    names_file.then(|| "it reads another file through a quoted reference".to_owned())
}

/// Why a formula is refused when an agent writes it (docs/agents.md,
/// "Dangerous formulas"), or `None`. GridCraft's parser reads it as the
/// kernel stores it, so text in strings never counts. Refused: the
/// functions of [`REFUSED_FUNCTIONS`], called or passed as a value; sheet
/// names that name a file; and what does not parse, which includes links
/// to other workbooks (`[1]Sheet!A1`) and DDE (`app|topic!item`).
pub fn refused_formula(formula: &str) -> Option<String> {
    check_formula(formula).map(|r| match r {
        Refusal::Unreadable(why) | Refusal::Refused(why) => why,
    })
}

/// Why [`check_formula`] refused: a typo the agent can fix, or a policy refusal.
enum Refusal {
    Unreadable(String),
    Refused(String),
}

fn check_formula(formula: &str) -> Option<Refusal> {
    // The kernel closes up to 8 missing parentheses, as Excel does.
    let mut closed = formula.to_owned();
    let mut parsed = parse(&closed);
    for _ in 0..8 {
        if parsed.is_ok() {
            break;
        }
        closed.push(')');
        parsed = parse(&closed);
    }
    let expr = match parsed {
        Ok(expr) => expr,
        Err(e) => {
            let e = escape(&e.to_string(), 100);
            return Some(Refusal::Unreadable(format!(
                "{e}; links to other workbooks and DDE are not allowed"
            )));
        }
    };
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
    why.map(Refusal::Refused)
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
    /// Formula cells whose result changed because of the edit (not
    /// counted when rows, columns or sheets moved), up to
    /// [`MAX_LISTED_CHANGES`].
    pub recalculated: Vec<CellChange>,
    pub recalculated_total: u64,
}

/// Applies `ops` in order. On an error the book is put back as it was.
pub(crate) fn apply(book: &mut Book, ops: &[EditOp]) -> Result<EditResult, CommandError> {
    let start = book.doc()?.wb.clone();
    let active = start.active().map(|s| s.name.clone());
    let mut result = EditResult::default();
    let mut written: HashSet<(usize, u32, u32)> = HashSet::new();
    let outcome = ops
        .iter()
        .enumerate()
        .try_for_each(|(i, op)| apply_one(&mut book.session, i, op, &mut result, &mut written));
    let session = &mut book.session;
    if let Err(e) = outcome {
        if let Some(d) = session.active_mut() {
            d.wb = start;
            d.calc.rebuild(&d.wb);
        }
        return Err(e);
    }
    // Filling a range moves the active sheet; the file keeps its own.
    if let Some(name) = active {
        let _ = session.execute_headless("sheet.activate", json!({ "sheet": name }));
    }
    // After rows, columns or sheets move, cells cannot be compared by address.
    let fills = [EditKind::SetValue, EditKind::SetFormula, EditKind::Clear];
    if ops.iter().all(|op| fills.contains(&op.kind)) {
        let after = wb(session)?;
        recalculated(&start, &after, &written, &mut result);
    }
    // One-shot calls keep no undo history (record 0003, point 4).
    if let Some(d) = session.active_mut() {
        d.undo.clear();
        d.redo.clear();
        d.saved = d.wb.clone();
    }
    Ok(result)
}

/// Runs one GridCraft command for operation `i`.
fn execute(
    session: &mut Session,
    i: usize,
    op: &EditOp,
    id: &str,
    params: Value,
) -> Result<(), CommandError> {
    match session.execute_headless(id, params) {
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

fn shape_error(i: usize, op: &EditOp, wanted: &str) -> CommandError {
    CommandError::new(
        ErrorCode::InvalidArgument,
        format!("ops[{i}] ({}) needs {wanted}", op.kind.name()),
        format!("the target {} has no {wanted}", op.target_text()),
        "Use a target like 5:7 or B2:B9 for rows, and C:D or C1:D1 for columns.",
    )
}

fn no_such_sheet(i: usize, name: &str) -> CommandError {
    CommandError::new(
        ErrorCode::NoSuchSheet,
        format!("ops[{i}] names a sheet that does not exist: \"{name}\""),
        "the workbook has no sheet with that name at this point of the batch",
        "Run overview to see the sheet names.",
    )
}

fn wb(session: &Session) -> Result<Arc<Workbook>, CommandError> {
    session
        .active()
        .map(|d| d.wb.clone())
        .ok_or_else(|| internal("editing", "no document is open"))
}

/// A sheet's index; no name means the first sheet.
fn sheet_of(session: &Session, i: usize, name: Option<&str>) -> Result<usize, CommandError> {
    let wb = wb(session)?;
    match name {
        Some(n) => wb.sheet_index(n).ok_or_else(|| no_such_sheet(i, n)),
        None if wb.sheets.is_empty() => Err(no_such_sheet(i, "(first sheet)")),
        None => Ok(0),
    }
}

/// What a cell holds, for the change list.
fn cell_state(sh: Option<&Sheet>, at: CellRef) -> Value {
    let Some(sh) = sh else {
        return Value::Null;
    };
    let value = value_json(&sh.value(at));
    match sh.cell(at).and_then(|c| c.formula.as_ref()) {
        Some(f) => {
            json!({ "formula": format!("={}", f.text.trim_start_matches('=')), "value": value })
        }
        None => value,
    }
}

fn apply_one(
    session: &mut Session,
    i: usize,
    op: &EditOp,
    result: &mut EditResult,
    written: &mut HashSet<(usize, u32, u32)>,
) -> Result<(), CommandError> {
    let done = match (&op.target, op.kind) {
        (EditTarget::Sheet(name), EditKind::AddSheet) => {
            let n = wb(session)?.sheets.len();
            let params = json!({ "name": name, "before": n });
            execute(session, i, op, "home.insertSheet", params)?;
            format!("sheet added at the end ({} sheets now)", n + 1)
        }
        (EditTarget::Sheet(name), EditKind::RenameSheet) => {
            let si = sheet_of(session, i, Some(name))?;
            let new = op.text.clone().unwrap_or_default();
            let params = json!({ "sheet": si, "name": new });
            execute(session, i, op, "sheet.rename", params)?;
            format!("renamed to {new}")
        }
        (EditTarget::Sheet(name), EditKind::DeleteSheet) => {
            let si = sheet_of(session, i, Some(name))?;
            execute(session, i, op, "home.deleteSheet", json!({ "sheet": si }))?;
            "sheet deleted".to_owned()
        }
        (
            EditTarget::Range(target),
            EditKind::SetValue | EditKind::SetFormula | EditKind::Clear,
        ) => {
            let si = sheet_of(session, i, target.sheet.as_deref())?;
            write_cells(session, i, op, si, target, result, written)?
        }
        (EditTarget::Range(target), EditKind::InsertRows | EditKind::DeleteRows) => {
            let (si, r) = (sheet_of(session, i, target.sheet.as_deref())?, target.range);
            if target.shape == Shape::Columns {
                return Err(shape_error(i, op, "rows"));
            }
            forget_kept(session, si, None);
            let n = op.count.unwrap_or(r.height()).max(1);
            let rows = format!("{}:{}", r.start.row + 1, r.start.row.saturating_add(n));
            let params = json!({ "sheet": si, "rows": rows });
            if op.kind == EditKind::InsertRows {
                execute(session, i, op, "home.insertRows", params)?;
                format!("{n} row(s) inserted before row {}", r.start.row + 1)
            } else {
                execute(session, i, op, "home.deleteRows", params)?;
                format!("row(s) {rows} deleted")
            }
        }
        (EditTarget::Range(target), EditKind::InsertColumns | EditKind::DeleteColumns) => {
            let (si, r) = (sheet_of(session, i, target.sheet.as_deref())?, target.range);
            if target.shape == Shape::Rows {
                return Err(shape_error(i, op, "columns"));
            }
            forget_kept(session, si, None);
            let n = op.count.unwrap_or(r.width()).max(1);
            let first = col_to_letters(r.start.col);
            let cols = format!(
                "{first}:{}",
                col_to_letters(r.start.col.saturating_add(n - 1))
            );
            let params = json!({ "sheet": si, "cols": cols });
            if op.kind == EditKind::InsertColumns {
                execute(session, i, op, "home.insertColumns", params)?;
                format!("{n} column(s) inserted before column {first}")
            } else {
                execute(session, i, op, "home.deleteColumns", params)?;
                format!("column(s) {cols} deleted")
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
    result.ops.push(OpResult {
        op: op.kind,
        target: op.target_text(),
        done,
    });
    Ok(())
}

/// `set_value`, `set_formula` and `clear`: writes the cells, lists what
/// changed, and says how many cells changed.
fn write_cells(
    session: &mut Session,
    i: usize,
    op: &EditOp,
    si: usize,
    target: &SheetRange,
    result: &mut EditResult,
    written: &mut HashSet<(usize, u32, u32)>,
) -> Result<String, CommandError> {
    let r = target.range;
    let before = wb(session)?;
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
            execute(session, i, op, "range.setValues", params)?;
        }
        // One cell, or a range filled the way Excel fills it (relative
        // references move with each cell).
        EditKind::SetFormula => {
            let input = format!("={}", op.text.as_deref().unwrap_or(""));
            if r.is_single() {
                let params = json!({ "sheet": si, "cell": start, "input": input });
                execute(session, i, op, "cell.set", params)?;
            } else {
                let select = json!({ "sheet": si, "range": range, "active": start });
                execute(session, i, op, "selection.set", select)?;
                let params = json!({ "sheet": si, "range": range, "input": input });
                execute(session, i, op, "range.fill", params)?;
            }
        }
        _ => {
            let params = json!({ "sheet": si, "range": range });
            execute(session, i, op, "edit.clearContents", params)?;
        }
    }
    let after = wb(session)?;
    let after_sheet = after.sheets.get(si).map(Arc::as_ref);
    let name = after_sheet.map(|s| s.name.clone()).unwrap_or_default();
    let mut count = 0u64;
    for at in cells {
        let (b, a) = (cell_state(before_sheet, at), cell_state(after_sheet, at));
        if a == b {
            continue;
        }
        count += 1;
        written.insert((si, at.row, at.col));
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
        && let Some(at) = r
            .iter()
            .find(|at| sh.cell(*at).is_none_or(|c| c.formula.is_none()))
    {
        return Err(CommandError::new(
            ErrorCode::InvalidArgument,
            format!("ops[{i}]: {} did not take the formula", at.a1()),
            "the cell is formatted as Text, so the formula would be stored as text",
            "Write the formula to a cell with another number format; nothing was saved.",
        ));
    }
    forget_kept(session, si, Some(&r));
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

/// Kept sheet content that names cells (scenarios, ignored-error ranges,
/// data table formulas, formatted note text; see the vendor README) and
/// that GridCraft does not move with them. After rows or columns move
/// (`within` is `None`) all of it goes; after a fill or clear, the data
/// tables anchored in the range. The save report then lists it as lost,
/// so nothing is written that points at the wrong cells.
fn forget_kept(session: &mut Session, si: usize, within: Option<&RangeRef>) {
    let Some(d) = session.active_mut() else {
        return;
    };
    let stale = |slot: &str| match within {
        None => {
            matches!(slot, "scenarios" | "ignoredErrors")
                || slot.starts_with("dataTable:")
                || slot.starts_with("note:")
        }
        Some(r) => slot
            .strip_prefix("dataTable:")
            .and_then(CellRef::parse)
            .is_some_and(|c| r.contains(c)),
    };
    let has_stale =
        d.wb.sheets
            .get(si)
            .and_then(|sh| sh.preserved.as_deref())
            .is_some_and(|p| p.elements.iter().any(|e| stale(&e.slot)));
    if !has_stale {
        return;
    }
    let wb = Arc::make_mut(&mut d.wb);
    if let Some(sh) = wb.sheets.get_mut(si) {
        let sh = Arc::make_mut(sh);
        if let Some(p) = sh.preserved.as_mut() {
            Arc::make_mut(p).elements.retain(|e| !stale(&e.slot));
        }
    }
}

/// Formula cells whose result changed, outside the cells written.
fn recalculated(
    before: &Workbook,
    after: &Workbook,
    written: &HashSet<(usize, u32, u32)>,
    result: &mut EditResult,
) {
    for (si, (b, a)) in before.sheets.iter().zip(after.sheets.iter()).enumerate() {
        if Arc::ptr_eq(b, a) {
            continue;
        }
        for (at, cell) in a.cells.iter() {
            if cell.formula.is_none() || written.contains(&(si, at.row, at.col)) {
                continue;
            }
            // Both sides as shown: NaN and NaN are the same, as text.
            let (old, new) = (value_json(&b.value(at)), value_json(&cell.value));
            if old == new {
                continue;
            }
            result.recalculated_total += 1;
            if result.recalculated.len() < MAX_LISTED_CHANGES {
                result.recalculated.push(CellChange {
                    sheet: a.name.clone(),
                    cell: at.a1(),
                    before: old,
                    after: new,
                });
            }
        }
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
    let encoded = kernel.encode(&book, &output, format, &EncodeOptions::default())?;
    let mut data = Map::new();
    data.insert("file".into(), shown(&path));
    data.insert("output".into(), shown(&output));
    // ops, changes, changes_total, recalculated, recalculated_total.
    if let Value::Object(fields) = json!(result) {
        data.extend(fields);
    }
    finish_save(args, &output, encoded, data)
}
