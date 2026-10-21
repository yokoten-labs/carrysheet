//! The workbook commands, run on any [`Engine`]: the same steps for the
//! command line and the MCP server, so programs stay thin.
//!
//! Each function takes arguments checked by
//! [`validate_args`](crate::validate_args) (absent ones are `null`) and
//! returns the JSON fields of the result. Paths in results are shown with
//! `~` for the home folder.

use std::path::{Path, PathBuf};

use carrysheet_model::SheetRange;
use serde_json::{Map, Value, json};

use super::save::{check_output, clean_from_file, default_output, quote_from_file, write_atomic};
use super::{
    DEFAULT_PAGE_COLUMNS, DEFAULT_PAGE_ROWS, EncodeOptions, Encoded, Engine, FileFormat,
    ReadCursor, ReadMode, ReadRequest, parse_ops,
};
use crate::error::{CommandError, ErrorCode};
use crate::limits::{MAX_LISTED_SHEETS, MAX_RESPONSE_TEXT_CHARS};
use crate::paths::display_path;
use crate::table::Command;

/// Commands that work on a workbook through an engine.
pub const ENGINE_COMMANDS: &[&str] = &["overview", "read", "eval", "edit", "convert"];

/// Runs one of [`ENGINE_COMMANDS`].
pub fn run<E: Engine>(
    engine: &E,
    command: &Command,
    args: &Map<String, Value>,
) -> Result<Map<String, Value>, CommandError> {
    run_within(engine, command, args, None)
}

/// [`run`] with the command's time limit in seconds (`None`: the
/// engine's, [`crate::limits::MAX_CALL_SECONDS`] for the kernel). One
/// budget covers the whole command: opening, the work and the save.
pub fn run_within<E: Engine>(
    engine: &E,
    command: &Command,
    args: &Map<String, Value>,
    seconds: Option<f64>,
) -> Result<Map<String, Value>, CommandError> {
    engine.begin_call(seconds);
    let result = dispatch(engine, command, args);
    // A limit that ran out wins over what the calls returned.
    engine.end_call()?;
    result
}

fn dispatch<E: Engine>(
    engine: &E,
    command: &Command,
    args: &Map<String, Value>,
) -> Result<Map<String, Value>, CommandError> {
    match command.name {
        "overview" => overview(engine, args),
        "read" => read(engine, args),
        "eval" => eval(engine, args),
        "edit" => edit(engine, args),
        "convert" => convert(engine, args),
        _ => Err(CommandError::not_implemented(&command.cli_name())),
    }
}

fn text<'a>(args: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

fn flag(args: &Map<String, Value>, name: &str) -> bool {
    args.get(name).and_then(Value::as_bool) == Some(true)
}

fn number(args: &Map<String, Value>, name: &str) -> Option<u32> {
    args.get(name)
        .and_then(Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

fn detailed(args: &Map<String, Value>) -> bool {
    text(args, "response_format") == Some("detailed")
}

/// The workbook path; validation makes sure it is there.
fn input(args: &Map<String, Value>) -> Result<PathBuf, CommandError> {
    text(args, "path").map(PathBuf::from).ok_or_else(|| {
        CommandError::invalid_argument(
            "path",
            "no workbook was given",
            "Name the workbook file, like book.xlsx.",
        )
    })
}

fn shown(path: &Path) -> Value {
    json!(display_path(path))
}

/// Header cells shown per sheet in `overview`.
const HEADER_CELLS: u32 = 20;
const HEADER_CELLS_DETAILED: u32 = DEFAULT_PAGE_COLUMNS;

/// Sheets, used ranges, header rows and counts.
pub fn overview<E: Engine>(
    engine: &E,
    args: &Map<String, Value>,
) -> Result<Map<String, Value>, CommandError> {
    let path = input(args)?;
    let book = engine.open(&path)?;
    let overview = engine.overview(&book)?;
    let width = if detailed(args) {
        HEADER_CELLS_DETAILED
    } else {
        HEADER_CELLS
    };
    let mut sheets = Vec::new();
    // Header rows stop when the answer holds this much text.
    let mut text_left = MAX_RESPONSE_TEXT_CHARS;
    let mut headers_cut = 0usize;
    for s in overview.sheets.iter().take(MAX_LISTED_SHEETS) {
        let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
        v["name"] = json!(clean_from_file(&s.name, 64));
        // The first row of the used range: usually the headers.
        let header = match &s.used_range {
            Some(_) if text_left == 0 => {
                headers_cut += 1;
                Value::Null
            }
            Some(used) => {
                let first_row = SheetRange::parse(used)
                    .ok()
                    .map(|r| r.range.sorted().start.row + 1)
                    .unwrap_or(1);
                let range = SheetRange::parse(&format!(
                    "{}!{first_row}:{first_row}",
                    carrysheet_model::format_sheet_name(&s.name)
                ))?;
                let request = ReadRequest {
                    range: Some(range),
                    max_rows: 1,
                    max_columns: width,
                    ..ReadRequest::default()
                };
                let page = engine.read_range(&book, &request)?;
                let mut header = json!({
                    "row": first_row,
                    "columns": page.columns,
                    "cells": page.rows.first().map(|r| r.cells.clone()).unwrap_or_default(),
                });
                if let Some(cut) = &page.cut {
                    header["cut"] = json!(cut);
                }
                text_left = text_left.saturating_sub(header.to_string().len());
                header
            }
            None => Value::Null,
        };
        v["header"] = header;
        sheets.push(v);
    }
    let mut data = Map::new();
    data.insert("file".into(), shown(&path));
    data.insert("sheets".into(), Value::Array(sheets));
    data.insert("date_system".into(), json!(overview.date_system));
    data.insert("defined_names".into(), json!(overview.defined_names));
    if !overview.warnings.is_empty() {
        data.insert("warnings".into(), json!(overview.warnings));
    }
    let total = overview.sheets.len();
    if total > MAX_LISTED_SHEETS || headers_cut > 0 {
        data.insert("sheets_total".into(), json!(total));
        data.insert(
            "hint".into(),
            json!(format!(
                "The answer was cut to stay small: {} of {total} sheet(s) listed, {headers_cut} header row(s) left out. Read a sheet's first row with read and a range like 'Name'!1:1.",
                total.min(MAX_LISTED_SHEETS)
            )),
        );
    }
    Ok(data)
}

/// One page of a range.
pub fn read<E: Engine>(
    engine: &E,
    args: &Map<String, Value>,
) -> Result<Map<String, Value>, CommandError> {
    let path = input(args)?;
    let mut request = ReadRequest {
        range: text(args, "range").map(SheetRange::parse).transpose()?,
        cursor: text(args, "cursor").map(ReadCursor::parse).transpose()?,
        detailed: detailed(args),
        ..ReadRequest::default()
    };
    if let Some(mode) = text(args, "mode") {
        request.mode = ReadMode::parse(mode).ok_or_else(|| {
            CommandError::invalid_argument(
                "mode",
                "mode is not values, formulas or both",
                "Use values, formulas or both.",
            )
        })?;
    }
    request.max_rows = number(args, "max_rows").unwrap_or(DEFAULT_PAGE_ROWS);
    request.max_columns = number(args, "max_columns").unwrap_or(DEFAULT_PAGE_COLUMNS);
    let book = engine.open(&path)?;
    let page = engine.read_range(&book, &request)?;
    let more = page.next_cursor.is_some();
    let mut data = match serde_json::to_value(&page) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    data.insert("file".into(), shown(&path));
    if more {
        data.insert(
            "hint".into(),
            json!("More cells remain: call read again with cursor set to next_cursor and the same max_rows, or ask for a smaller range."),
        );
    }
    Ok(data)
}

/// Computes a formula without changing the file.
pub fn eval<E: Engine>(
    engine: &E,
    args: &Map<String, Value>,
) -> Result<Map<String, Value>, CommandError> {
    let path = input(args)?;
    let formula = text(args, "formula").unwrap_or("");
    if formula.trim().trim_start_matches('=').trim().is_empty() {
        return Err(CommandError::invalid_argument(
            "formula",
            "the formula is empty",
            "Give a formula like SUM(Sheet1!B2:B9).",
        ));
    }
    let book = engine.open(&path)?;
    let result = engine.evaluate(&book, text(args, "sheet"), formula)?;
    let mut data = match serde_json::to_value(&result) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    data.insert("file".into(), shown(&path));
    Ok(data)
}

/// Applies a batch of operations and saves the result.
pub fn edit<E: Engine>(
    engine: &E,
    args: &Map<String, Value>,
) -> Result<Map<String, Value>, CommandError> {
    let path = input(args)?;
    let ops = parse_ops(args.get("ops").unwrap_or(&Value::Null))?;
    let overwrite = flag(args, "overwrite");
    let output = match text(args, "output") {
        Some(o) => PathBuf::from(o),
        None if overwrite => path.clone(),
        None => default_output(&path)?,
    };
    let format = output_format(&output, None)?;
    check_output(&output, overwrite)?;
    let mut book = engine.open(&path)?;
    let result = engine.edit(&mut book, &ops)?;
    let encoded = engine.encode_with(&book, &output, format, &EncodeOptions::default())?;
    let mut data = Map::new();
    data.insert("file".into(), shown(&path));
    data.insert("output".into(), shown(&output));
    data.insert("ops".into(), json!(result.ops));
    data.insert("changes".into(), json!(result.changes));
    data.insert("changes_total".into(), json!(result.changes_total));
    data.insert("recalculated".into(), json!(result.recalculated));
    data.insert(
        "recalculated_total".into(),
        json!(result.recalculated_total),
    );
    finish_save(args, &output, encoded, data)
}

/// Writes the workbook in another format.
pub fn convert<E: Engine>(
    engine: &E,
    args: &Map<String, Value>,
) -> Result<Map<String, Value>, CommandError> {
    let path = input(args)?;
    let output = text(args, "output").map(PathBuf::from).ok_or_else(|| {
        CommandError::invalid_argument(
            "output",
            "no output file was given",
            "Name the file to write, like book.csv.",
        )
    })?;
    let format = output_format(&output, text(args, "format"))?;
    check_output(&output, flag(args, "overwrite"))?;
    let book = engine.open(&path)?;
    let options = EncodeOptions {
        sheet: text(args, "sheet"),
        csv_raw: flag(args, "csv_raw"),
    };
    let encoded = engine.encode_with(&book, &output, format, &options)?;
    let mut data = Map::new();
    data.insert("file".into(), shown(&path));
    data.insert("output".into(), shown(&output));
    finish_save(args, &output, encoded, data)
}

/// The format to write: `format` when given, else from the extension.
fn output_format(output: &Path, format: Option<&str>) -> Result<FileFormat, CommandError> {
    let by_name = match format {
        Some(f) => FileFormat::from_extension(f),
        None => FileFormat::from_path(output),
    };
    by_name.ok_or_else(|| {
        CommandError::new(
            ErrorCode::UnsupportedFormat,
            format!("{} has no format Carrysheet writes", display_path(output)),
            "the format comes from the output's extension: .xlsx, .xlsm or .csv",
            "Name the output with one of those extensions, or pass format.",
        )
    })
}

/// Shows the save report; with `dry_run` stops there, else refuses a lossy
/// save without `confirm_loss`, then writes. `confirm` (deleting rows,
/// overwriting) never stands in for it: dropping macros is agreed to on its
/// own.
fn finish_save(
    args: &Map<String, Value>,
    output: &Path,
    encoded: Encoded,
    mut data: Map<String, Value>,
) -> Result<Map<String, Value>, CommandError> {
    let Encoded { bytes, report } = encoded;
    let dry_run = flag(args, "dry_run");
    let loses = report.loses_something();
    data.insert("format".into(), json!(report.format));
    data.insert("save".into(), json!(report));
    data.insert("dry_run".into(), json!(dry_run));
    if dry_run {
        data.insert("written".into(), json!(false));
        if loses {
            data.insert(
                "hint".into(),
                json!("Saving would lose what save.lost lists. Call again with confirm_loss set to true to save anyway."),
            );
        }
        return Ok(data);
    }
    if loses && !flag(args, "confirm_loss") {
        return Err(loss_error(output, &report));
    }
    write_atomic(output, &bytes)?;
    data.insert("written".into(), json!(true));
    data.insert("bytes".into(), json!(bytes.len()));
    Ok(data)
}

fn loss_error(output: &Path, report: &super::SaveReport) -> CommandError {
    let mut fix = String::from(
        "Call again with confirm_loss set to true to save without them (--confirm-loss on the command line), or with dry_run to see the report.",
    );
    let keeps_macros = output
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("xlsm"));
    if report.lost.iter().any(|l| l.feature == "VBA project") && !keeps_macros {
        fix.push_str(" To keep macros, write an .xlsm file instead.");
    }
    let mut why = String::from(
        "Carrysheet never drops content silently, and this save cannot keep everything the file has",
    );
    if !report.dropped_parts.is_empty() {
        why.push_str("; parts dropped: ");
        why.push_str(&report.dropped_parts.join(", "));
    }
    CommandError::new(
        ErrorCode::ConfirmationRequired,
        format!("saving would lose {}", report.summary()),
        why,
        fix,
    )
}

/// A part name from a file, as shown in reports.
pub fn part_name(name: &str) -> String {
    quote_from_file(name, 64)
}
