//! The csv front end (record 0010, decision 3). The bytes are decoded by
//! their encoding, GridCraft splits the fields as text, and a field becomes
//! a number only when the number prints back as the same text. So `007`,
//! `1E5` and `SEPT1` stay as they were written (record 0006).

use std::path::Path;

use crate::commands::paths::display_path;
use crate::commands::{CommandError, ErrorCode};
use encoding_rs::{Encoding, UTF_8};
use gridcraft_engine::core::Value;
use gridcraft_engine::model::{NumFmt, StyleId, StyleTable, Workbook};

/// Reads csv bytes into a workbook with one sheet, named after the file,
/// and a warning when the encoding was guessed.
pub(crate) fn read(
    path: &Path,
    name: &str,
    bytes: &[u8],
) -> Result<(Workbook, Vec<String>), CommandError> {
    let (text, warnings) = decode(path, bytes)?;
    let options = gridcraft_xlsx::CsvOptions {
        delimiter: 0,
        parse_values: false,
        ..Default::default()
    };
    let mut wb = gridcraft_xlsx::read_csv(text.as_bytes(), &options)
        .map_err(|e| crate::kernel::internal("reading the csv file", &e.to_string()))?;
    let mut styles = std::mem::take(&mut wb.styles);
    let mut made = vec![None; MAX_DECIMALS + 1];
    let mut rows = Vec::new();
    if let Some(sheet) = wb.sheet_mut(0) {
        let all: Vec<_> = sheet.cells.iter().map(|(at, _)| at).collect();
        for at in all {
            let Some(cell) = sheet.cells.get_mut(at) else {
                continue;
            };
            let text = cell.value.as_text();
            if let Some((n, style)) = text.and_then(|t| number(t, &mut styles, &mut made)) {
                (cell.value, cell.style) = (Value::Number(n), style);
            }
            // Cells come sorted by row, then column.
            if rows.last() != Some(&at.row) {
                rows.push(at.row);
            }
        }
        let stem = Path::new(name).file_stem().and_then(|s| s.to_str());
        let stem = stem.unwrap_or_default();
        let bad = |c: &char| ":\\/?*[]".contains(*c);
        let clean: String = stem.chars().filter(|c| !bad(c)).take(31).collect();
        let clean = clean.trim_matches('\'');
        if !clean.is_empty() && !clean.eq_ignore_ascii_case("History") {
            sheet.name = clean.to_owned();
        }
    }
    wb.styles = styles;
    // As GridCraft's own open does: rows grow to fit text on several lines.
    for row in rows {
        gridcraft_engine::cmd::auto_row_height(&mut wb, 0, row);
    }
    // Opening counts against the time limit, as in GridCraft's other readers.
    gridcraft_engine::core::limits::check_time();
    Ok((wb, warnings))
}

/// Excel shows at most 30 decimals and 15 significant digits, so a field
/// with more stays text, before any format code is built.
const MAX_DECIMALS: usize = 30;
type Found = Option<(f64, StyleId)>;

/// The number a field holds, only when it prints back as the same text:
/// `0.1` in General, `12.50` in the format `0.00`, one style per decimal
/// count. `-0` stays text, since an .xlsx file writes it as `0`. With at
/// most 15 significant digits, the format prints the shortest form of the
/// number padded with zeros, so comparing that form is enough.
fn number(field: &str, styles: &mut StyleTable, made: &mut [Option<StyleId>]) -> Found {
    let unsigned = field.strip_prefix('-').unwrap_or(field);
    let (whole, decimals) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let core = unsigned.trim_matches(['0', '.']);
    let significant = core.len() - usize::from(core.contains('.'));
    let fits = decimals.len() <= MAX_DECIMALS && significant <= 15;
    if whole.is_empty() || !digits(whole) || !digits(decimals) || !fits {
        return None;
    }
    let n: f64 = field.parse().ok()?;
    if !n.is_finite() || (n == 0.0 && field.starts_with('-')) {
        return None;
    }
    let short = match decimals {
        "" => field,
        _ => field.trim_end_matches('0').trim_end_matches('.'),
    };
    if n.to_string() != short {
        return None;
    } else if short == field {
        return Some((n, StyleId::DEFAULT));
    }
    let code = || format!("0.{}", "0".repeat(decimals.len()));
    let derive = || styles.derive(StyleId::DEFAULT, |s| s.num_fmt = NumFmt::new(&code()));
    Some((n, *made.get_mut(decimals.len())?.get_or_insert_with(derive)))
}

const GUESSED: &str = "Carrysheet guessed";

/// A byte order mark decides; else valid UTF-8 is UTF-8; else `chardetng`
/// guesses. Bytes the encoding cannot hold are an error, never U+FFFD.
fn decode(path: &Path, bytes: &[u8]) -> Result<(String, Vec<String>), CommandError> {
    let (encoding, body, how) = match Encoding::for_bom(bytes) {
        Some((encoding, bom)) => (
            encoding,
            bytes.get(bom..).unwrap_or_default(),
            "its byte order mark names",
        ),
        None if std::str::from_utf8(bytes).is_ok() => (UTF_8, bytes, "Carrysheet found"),
        None => {
            // We give chardetng at most 1 MiB, from the first byte that is not ASCII.
            let start = bytes.iter().position(|b| *b >= 0x80).unwrap_or(0);
            let rest = bytes.get(start..).unwrap_or_default();
            let sample = rest.get(..1 << 20).unwrap_or(rest);
            let mut detector = chardetng::EncodingDetector::new();
            detector.feed(sample, sample.len() == rest.len());
            (detector.guess(None, true), bytes, GUESSED)
        }
    };
    let text = encoding.decode_without_bom_handling_and_without_replacement(body);
    let name = encoding.name();
    let w = format!("not UTF-8, read as {name} by a guess; save as UTF-8 if the text looks wrong");
    let warnings = (how == GUESSED).then_some(w).into_iter().collect();
    text.map(|t| (t.into_owned(), warnings)).ok_or_else(|| {
        CommandError::new(
            ErrorCode::DamagedFile,
            format!("{} could not be read as text", display_path(path)),
            format!("some bytes are not valid {name}, the encoding {how}"),
            "Save the file again as UTF-8 in the program that made it, then open that copy.",
        )
    })
}
