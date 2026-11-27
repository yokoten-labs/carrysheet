//! The csv front end (record 0010, decision 3). The bytes are decoded by
//! their encoding, GridCraft splits the fields as text, and a field becomes
//! a number only when the number prints back as the same text. So `007`,
//! `1E5` and `SEPT1` stay as they were written (record 0006).

use std::path::Path;

use carrysheet_commands::paths::display_path;
use carrysheet_commands::{CommandError, ErrorCode};
use encoding_rs::{Encoding, UTF_8};
use gridcraft_engine::core::Value;
use gridcraft_engine::model::Workbook;

/// Reads csv bytes into a workbook with one sheet, named after the file.
pub(crate) fn read(path: &Path, name: &str, bytes: &[u8]) -> Result<Workbook, CommandError> {
    let text = decode(path, bytes)?;
    let options = gridcraft_xlsx::CsvOptions {
        delimiter: 0,
        parse_values: false,
        ..Default::default()
    };
    let mut wb = gridcraft_xlsx::read_csv(text.as_bytes(), &options)
        .map_err(|e| crate::internal("reading the csv file", &e.to_string()))?;
    let mut rows = Vec::new();
    if let Some(sheet) = wb.sheet_mut(0) {
        let numbers: Vec<_> = sheet
            .cells
            .iter()
            .filter_map(|(at, cell)| Some((at, number(cell.value.as_text()?)?)))
            .collect();
        for (at, n) in numbers {
            if let Some(cell) = sheet.cells.get_mut(at) {
                cell.value = Value::Number(n);
            }
        }
        rows = sheet.cells.iter().map(|(at, _)| at.row).collect();
        let stem = Path::new(name).file_stem().and_then(|s| s.to_str());
        let stem = stem.unwrap_or_default();
        let bad = |c: &char| ":\\/?*[]".contains(*c);
        let clean: String = stem.chars().filter(|c| !bad(c)).take(31).collect();
        let clean = clean.trim_matches('\'');
        if !clean.is_empty() && !clean.eq_ignore_ascii_case("History") {
            sheet.name = clean.to_owned();
        }
    }
    // As GridCraft's own open does: rows grow to fit text on several lines.
    rows.dedup();
    for row in rows {
        gridcraft_engine::cmd::auto_row_height(&mut wb, 0, row);
    }
    // Opening counts against the time limit, as in GridCraft's other readers.
    gridcraft_engine::core::limits::check_time();
    Ok(wb)
}

/// The number a field holds, only when it prints back as the same text.
fn number(field: &str) -> Option<f64> {
    let n: f64 = field.parse().ok()?;
    (n.is_finite() && n.to_string() == field).then_some(n)
}

/// A byte order mark decides; else valid UTF-8 is UTF-8; else `chardetng`
/// guesses. Bytes the encoding cannot hold are an error, never U+FFFD.
fn decode(path: &Path, bytes: &[u8]) -> Result<String, CommandError> {
    let (encoding, body, how) = match Encoding::for_bom(bytes) {
        Some((encoding, bom)) => (
            encoding,
            bytes.get(bom..).unwrap_or_default(),
            "its byte order mark names",
        ),
        None if std::str::from_utf8(bytes).is_ok() => (UTF_8, bytes, "Carrysheet found"),
        None => {
            // chardetng reads at most 1 MiB to guess.
            let sample = bytes.get(..1 << 20).unwrap_or(bytes);
            let mut detector = chardetng::EncodingDetector::new();
            detector.feed(sample, sample.len() == bytes.len());
            (detector.guess(None, true), bytes, "Carrysheet guessed")
        }
    };
    let text = encoding.decode_without_bom_handling_and_without_replacement(body);
    let name = encoding.name();
    text.map(|t| t.into_owned()).ok_or_else(|| {
        CommandError::new(
            ErrorCode::DamagedFile,
            format!("{} could not be read as text", display_path(path)),
            format!("some bytes are not valid {name}, the encoding {how}"),
            "Save the file again as UTF-8 in the program that made it, then open that copy.",
        )
    })
}
