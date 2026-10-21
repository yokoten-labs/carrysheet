//! Applying `edit` operations through GridCraft's command layer, which
//! checks protection, tables and array formulas, and recalculates only
//! what depends on the changed cells.

use std::collections::HashSet;
use std::sync::Arc;

use carrysheet_commands::engine::edit::MAX_LISTED_CHANGES;
use carrysheet_commands::engine::save::clean_from_file;
use carrysheet_commands::engine::{
    CellChange, EditKind, EditOp, EditResult, EditTarget, Scalar, value_json,
};
use carrysheet_commands::{CommandError, ErrorCode};
use carrysheet_model::{Range, RangeShape, column_to_letters};
use gridcraft_engine::core::CellRef as GCell;
use gridcraft_engine::model::{Sheet, Workbook};
use gridcraft_engine::{EngineError, Session};
use serde_json::{Value as Json, json};

use crate::book::{Book, cell_value};
use crate::internal;

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
    if !ops.iter().any(|op| op.kind.is_structural()) {
        let after = session
            .active()
            .map(|d| d.wb.clone())
            .ok_or_else(|| internal("editing", "no document is open"))?;
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

fn op_failed(i: usize, op: &EditOp, e: EngineError) -> CommandError {
    if let EngineError::Internal(_, detail) = &e {
        return internal("editing", detail);
    }
    CommandError::new(
        ErrorCode::InvalidArgument,
        format!("ops[{i}] ({}) could not be applied", op.kind.as_str()),
        clean_from_file(&e.to_string(), 200),
        "Change or remove this operation and call again; nothing was saved.",
    )
}

fn shape_error(i: usize, op: &EditOp, wanted: &str) -> CommandError {
    CommandError::new(
        ErrorCode::InvalidArgument,
        format!("ops[{i}] ({}) needs {wanted}", op.kind.as_str()),
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

/// A1 text without a sheet, as GridCraft reads it.
fn a1(r: &Range) -> String {
    r.sorted().to_string()
}

fn gcell(r: u32, c: u32) -> GCell {
    GCell::new(r, c)
}

/// What a cell holds, for the change list.
fn cell_state(sh: Option<&Sheet>, at: GCell) -> Json {
    let Some(sh) = sh else {
        return Json::Null;
    };
    let value = value_json(&cell_value(&sh.value(at)));
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
    let run = |s: &mut Session, id: &str, params: Json| {
        s.execute_headless(id, params)
            .map_err(|e| op_failed(i, op, e))
    };
    let done = match (&op.target, op.kind) {
        (EditTarget::Sheet(name), EditKind::AddSheet) => {
            let n = wb(session)?.sheets.len();
            run(
                session,
                "home.insertSheet",
                json!({ "name": name, "before": n }),
            )?;
            format!("sheet added at the end ({} sheets now)", n + 1)
        }
        (EditTarget::Sheet(name), EditKind::RenameSheet) => {
            let si = sheet_of(session, i, Some(name))?;
            let new = op.text.clone().unwrap_or_default();
            run(session, "sheet.rename", json!({ "sheet": si, "name": new }))?;
            format!("renamed to {new}")
        }
        (EditTarget::Sheet(name), EditKind::DeleteSheet) => {
            let si = sheet_of(session, i, Some(name))?;
            run(session, "home.deleteSheet", json!({ "sheet": si }))?;
            "sheet deleted".to_owned()
        }
        (EditTarget::Range(target), kind) => {
            let si = sheet_of(session, i, target.sheet.as_deref())?;
            let r = target.range.sorted();
            match kind {
                EditKind::SetValue | EditKind::SetFormula | EditKind::Clear => {
                    let before = wb(session)?;
                    let cells = cells_of(&before, si, &r, kind);
                    match kind {
                        EditKind::SetValue => set_values(session, i, op, si, &r, &before)?,
                        EditKind::SetFormula => set_formula(session, i, op, si, &r)?,
                        _ => {
                            run(
                                session,
                                "edit.clearContents",
                                json!({ "sheet": si, "range": a1(&r) }),
                            )?;
                        }
                    }
                    let after = wb(session)?;
                    let name = after
                        .sheets
                        .get(si)
                        .map(|s| s.name.clone())
                        .unwrap_or_default();
                    let mut count = 0u64;
                    for at in cells {
                        let b = cell_state(before.sheets.get(si).map(Arc::as_ref), at);
                        let a = cell_state(after.sheets.get(si).map(Arc::as_ref), at);
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
                    if kind == EditKind::SetFormula {
                        check_formulas_stuck(&after, si, &r, i)?;
                    }
                    forget_kept(session, si, Some(&r));
                    format!("{count} cell(s) changed")
                }
                EditKind::InsertRows | EditKind::DeleteRows => {
                    if r.shape == RangeShape::Columns {
                        return Err(shape_error(i, op, "rows"));
                    }
                    forget_kept(session, si, None);
                    let n = op.count.unwrap_or(r.row_count()).max(1);
                    let last = r.start.row.saturating_add(n);
                    let rows = format!("{}:{}", r.start.row + 1, last);
                    if kind == EditKind::InsertRows {
                        run(
                            session,
                            "home.insertRows",
                            json!({ "sheet": si, "rows": rows }),
                        )?;
                        format!("{n} row(s) inserted before row {}", r.start.row + 1)
                    } else {
                        run(
                            session,
                            "home.deleteRows",
                            json!({ "sheet": si, "rows": rows }),
                        )?;
                        format!("row(s) {rows} deleted")
                    }
                }
                EditKind::InsertColumns | EditKind::DeleteColumns => {
                    if r.shape == RangeShape::Rows {
                        return Err(shape_error(i, op, "columns"));
                    }
                    forget_kept(session, si, None);
                    let n = op.count.unwrap_or(r.col_count()).max(1);
                    let last = r.start.col.saturating_add(n - 1);
                    let cols = format!(
                        "{}:{}",
                        column_to_letters(r.start.col),
                        column_to_letters(last)
                    );
                    if kind == EditKind::InsertColumns {
                        run(
                            session,
                            "home.insertColumns",
                            json!({ "sheet": si, "cols": cols }),
                        )?;
                        format!(
                            "{n} column(s) inserted before column {}",
                            column_to_letters(r.start.col)
                        )
                    } else {
                        run(
                            session,
                            "home.deleteColumns",
                            json!({ "sheet": si, "cols": cols }),
                        )?;
                        format!("column(s) {cols} deleted")
                    }
                }
                _ => {
                    return Err(CommandError::invalid_argument(
                        "ops",
                        format!("ops[{i}]: {} needs a sheet name as target", kind.as_str()),
                        "Set target to the sheet name.",
                    ));
                }
            }
        }
        (EditTarget::Sheet(_), kind) => {
            return Err(CommandError::invalid_argument(
                "ops",
                format!("ops[{i}]: {} needs a range as target", kind.as_str()),
                "Set target to a range like Sheet1!B2:B9.",
            ));
        }
    };
    result.ops.push(carrysheet_commands::engine::OpResult {
        op: op.kind,
        target: op.target_text(),
        done,
    });
    Ok(())
}

/// Kept sheet content that names cells (scenarios, ignored-error ranges,
/// data table formulas, formatted note text; see the vendor README) and
/// that GridCraft does not move with them. After rows or columns move
/// (`within` is `None`) all of it goes; after a fill or clear, the data
/// tables anchored in the range. The save report then lists it as lost,
/// so nothing is written that points at the wrong cells.
fn forget_kept(session: &mut Session, si: usize, within: Option<&Range>) {
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
            .and_then(GCell::parse)
            .is_some_and(|c| r.contains(c.row, c.col)),
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

/// The cells an operation may change: every cell of a fill, and for a
/// clear only those that hold something (a clear of A:A is not a million
/// changes).
fn cells_of(wb: &Workbook, si: usize, r: &Range, kind: EditKind) -> Vec<GCell> {
    let Some(sh) = wb.sheets.get(si) else {
        return Vec::new();
    };
    if kind == EditKind::Clear {
        let g = gridcraft_engine::core::RangeRef::new(
            gcell(r.start.row, r.start.col),
            gcell(r.end.row, r.end.col),
        );
        return sh.cells.iter_range(g).map(|(at, _)| at).collect();
    }
    (r.start.row..=r.end.row)
        .flat_map(|row| (r.start.col..=r.end.col).map(move |col| gcell(row, col)))
        .collect()
}

/// `set_value`: each cell gets the value with its JSON type. Text is
/// marked as text (a leading `'`), so `"3"` stays text and `"=A1"` is not
/// a formula; cells formatted as Text take it as is.
fn set_values(
    session: &mut Session,
    i: usize,
    op: &EditOp,
    si: usize,
    r: &Range,
    wb: &Workbook,
) -> Result<(), CommandError> {
    let sh = wb.sheets.get(si);
    let value = op.value.clone().unwrap_or(Scalar::Text(String::new()));
    let rows: Vec<Json> = (r.start.row..=r.end.row)
        .map(|row| {
            Json::Array(
                (r.start.col..=r.end.col)
                    .map(|col| match &value {
                        Scalar::Number(n) => json!(n),
                        Scalar::Bool(b) => json!(b),
                        Scalar::Text(t) => {
                            let text_format = sh.is_some_and(|sh| {
                                wb.styles.get(sh.style_id(gcell(row, col))).num_fmt.as_str() == "@"
                            });
                            if text_format {
                                json!(t)
                            } else {
                                json!(format!("'{t}"))
                            }
                        }
                    })
                    .collect(),
            )
        })
        .collect();
    let start = gcell(r.start.row, r.start.col).a1();
    session
        .execute_headless(
            "range.setValues",
            json!({ "sheet": si, "range": start, "values": rows }),
        )
        .map(|_| ())
        .map_err(|e| op_failed(i, op, e))
}

/// `set_formula`: one cell, or a range filled the way Excel fills it
/// (relative references move with each cell).
fn set_formula(
    session: &mut Session,
    i: usize,
    op: &EditOp,
    si: usize,
    r: &Range,
) -> Result<(), CommandError> {
    let input = format!("={}", op.text.as_deref().unwrap_or(""));
    let start = gcell(r.start.row, r.start.col).a1();
    let run = |s: &mut Session, id: &str, params: Json| {
        s.execute_headless(id, params)
            .map(|_| ())
            .map_err(|e| op_failed(i, op, e))
    };
    if r.is_single_cell() {
        return run(
            session,
            "cell.set",
            json!({ "sheet": si, "cell": start, "input": input }),
        );
    }
    let range = a1(r);
    run(
        session,
        "selection.set",
        json!({ "sheet": si, "range": range, "active": start }),
    )?;
    run(
        session,
        "range.fill",
        json!({ "sheet": si, "range": range, "input": input }),
    )
}

/// A formula typed into a cell formatted as Text stays text, as in Excel.
/// An agent asked for a formula, so that is an error, not a silent change.
fn check_formulas_stuck(wb: &Workbook, si: usize, r: &Range, i: usize) -> Result<(), CommandError> {
    let Some(sh) = wb.sheets.get(si) else {
        return Ok(());
    };
    let stuck = (r.start.row..=r.end.row)
        .flat_map(|row| (r.start.col..=r.end.col).map(move |col| gcell(row, col)))
        .find(|at| sh.cell(*at).is_none_or(|c| c.formula.is_none()));
    match stuck {
        None => Ok(()),
        Some(at) => Err(CommandError::new(
            ErrorCode::InvalidArgument,
            format!("ops[{i}]: {} did not take the formula", at.a1()),
            "the cell is formatted as Text, so the formula would be stored as text",
            "Write the formula to a cell with another number format; nothing was saved.",
        )),
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
            let (old, new) = (cell_value(&b.value(at)), cell_value(&cell.value));
            let same = old == new
                || matches!((&old, &new), (carrysheet_model::CellValue::Number(x), carrysheet_model::CellValue::Number(y)) if x.is_nan() && y.is_nan());
            if same {
                continue;
            }
            result.recalculated_total += 1;
            if result.recalculated.len() < MAX_LISTED_CHANGES {
                result.recalculated.push(CellChange {
                    sheet: a.name.clone(),
                    cell: at.a1(),
                    before: value_json(&old),
                    after: value_json(&new),
                });
            }
        }
    }
}
