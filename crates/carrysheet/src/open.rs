//! Opening a workbook: a file's bytes to GridCraft's workbook, recalculated,
//! so every value read is computed, not cached. Text goes through the csv
//! front end (record 0010, decision 3), which keeps what was written.

use std::path::Path;
use std::sync::Arc;

use encoding_rs::{Encoding, UTF_8};
use gridcraft_engine::core::Value;
use gridcraft_engine::model::{NumFmt, StyleId, StyleTable, Workbook};
use gridcraft_engine::{DocState, EngineError, Session};

use crate::error::{CommandError, ErrorCode};
use crate::internal;
use crate::text::escape;

/// The largest file opened. 32-bit programs get less (record 0003, point 4).
pub const MAX_FILE_BYTES: u64 = if cfg!(target_pointer_width = "64") { 1 << 30 } else { 256 << 20 };

/// Extensions [`check_opens`] accepts.
const OPENS: &[&str] = &["xlsx", "xlsm", "xltx", "xltm", "xls", "xlsb", "ods", "csv"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    /// An OOXML zip package: parts can be compared on save.
    Package,
    Csv,
    /// A format GridCraft imports with losses it does not list (xls, xlsb).
    Imported(&'static str),
}

/// A workbook opened by the kernel, already recalculated.
pub struct Book {
    pub(crate) session: Session,
    /// The file's bytes, to compare a save with.
    pub(crate) source: Arc<Vec<u8>>,
    pub(crate) kind: SourceKind,
    /// What GridCraft skipped or repaired while reading, as it said it.
    pub(crate) warnings: Vec<String>,
}

impl std::fmt::Debug for Book {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Book")
            .field("kind", &self.kind)
            .field("bytes", &self.source.len())
            .finish_non_exhaustive()
    }
}

/// The error for bytes over [`MAX_FILE_BYTES`].
pub(crate) fn too_large(path: &Path) -> CommandError {
    CommandError::new(
        ErrorCode::LimitExceeded,
        format!("{} is too large to open", path.display()),
        format!("Carrysheet opens files up to {} MB", MAX_FILE_BYTES >> 20),
        "Split the workbook, or save a copy with fewer sheets or rows.",
    )
}

/// Refuses a file whose extension Carrysheet does not open, before it is read.
pub fn check_opens(path: &Path) -> Result<(), CommandError> {
    if OPENS.contains(&extension(path).as_str()) {
        return Ok(());
    }
    Err(CommandError::new(
        ErrorCode::UnsupportedFormat,
        format!("{} is not a workbook Carrysheet opens", path.display()),
        "the extension is not one of .xlsx, .xlsm, .xltx, .xltm, .xls, .xlsb, .ods or .csv",
        "Give a spreadsheet file with one of those extensions.",
    ))
}

/// The file's extension in lower case, or `""`.
fn extension(path: &Path) -> String {
    let ext = path.extension().and_then(|e| e.to_str());
    ext.map(str::to_ascii_lowercase).unwrap_or_default()
}

/// GridCraft's open error (plain text, which can quote the file) with a code.
fn open_error(path: &Path, e: &EngineError) -> CommandError {
    let text = e.to_string();
    if let EngineError::Internal(_, detail) = e {
        return internal("opening the file", detail);
    }
    let (shown, why) = (path.display(), escape(&text, 200));
    if text.contains(gridcraft_engine::io::PASSWORD_REQUIRED) {
        return CommandError::new(
            ErrorCode::UnsupportedFormat,
            format!("{shown} is protected with a password"),
            why,
            "Save a copy without the password in the program that made it, then use that copy.",
        );
    }
    if text.contains("too large") {
        return CommandError::new(
            ErrorCode::LimitExceeded,
            format!("{shown} holds more than Carrysheet accepts"),
            why,
            "Split the workbook into smaller files.",
        );
    }
    damaged(path, why)
}

fn damaged(path: &Path, why: String) -> CommandError {
    CommandError::new(
        ErrorCode::DamagedFile,
        format!("{} could not be opened as a workbook", path.display()),
        why,
        "Check that this is the right file and that it was copied completely; a damaged file is not repaired.",
    )
}

impl Book {
    /// Parses and recalculates a file's bytes; see [`crate::Kernel::open_bytes`].
    pub(crate) fn from_bytes(path: &Path, bytes: Vec<u8>) -> Result<Book, CommandError> {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "book.xlsx".to_owned());
        let sniffed = gridcraft_xlsx::sniff(&bytes);
        let ext = extension(path);
        // GridCraft reads text named .xlsx as csv. An .xlsx or .xlsb that is
        // not a zip package is damaged or not what it says, so refuse it.
        if matches!(ext.as_str(), "xlsx" | "xlsm" | "xltx" | "xltm" | "xlsb")
            && sniffed == gridcraft_xlsx::Format::Csv
        {
            let why = format!("an .{ext} file is a zip package, and this one is not");
            return Err(damaged(path, why));
        }
        let kind = match sniffed {
            gridcraft_xlsx::Format::Xlsx => SourceKind::Package,
            gridcraft_xlsx::Format::Csv => SourceKind::Csv,
            gridcraft_xlsx::Format::Xlsb => SourceKind::Imported("xlsb"),
            gridcraft_xlsx::Format::Ods => SourceKind::Imported("ods"),
            _ => SourceKind::Imported("xls"),
        };
        // Text goes through the csv front end, which keeps what was written.
        let (mut wb, warnings) = if kind == SourceKind::Csv {
            read_csv(path, &name, &bytes)?
        } else {
            gridcraft_engine::io::open_bytes(&name, &bytes).map_err(|e| open_error(path, &e))?
        };
        // So relative links to other workbooks follow a save to another folder.
        wb.source_dir =
            std::path::absolute(path).ok().and_then(|p| p.parent().map(Path::to_path_buf));
        // Recalculates everything, so values read are computed, not cached.
        let doc = DocState::new(wb, None, name);
        let mut session = Session::new();
        session.add_document(doc);
        Ok(Book { session, source: Arc::new(bytes), kind, warnings })
    }

    pub(crate) fn doc(&self) -> Result<&DocState, CommandError> {
        self.session.active().ok_or_else(|| internal("reading the workbook", "no document is open"))
    }

    pub(crate) fn wb(&self) -> Result<&Workbook, CommandError> {
        Ok(self.doc()?.wb.as_ref())
    }

    /// A sheet's index by name (any case), or the first sheet.
    pub(crate) fn sheet_index(&self, name: Option<&str>) -> Result<usize, CommandError> {
        let wb = self.wb()?;
        match name {
            Some(n) => wb.sheet_index(n).ok_or_else(|| no_such_sheet(n)),
            None if wb.sheets.is_empty() => Err(no_such_sheet("(first sheet)")),
            None => Ok(0),
        }
    }
}

pub(crate) fn no_such_sheet(name: &str) -> CommandError {
    CommandError::new(
        ErrorCode::NoSuchSheet,
        format!("there is no sheet named {}", crate::text::quote(name, 64)),
        "the workbook has no sheet with that name (names are compared without case)",
        "Run overview to see the sheet names.",
    )
}

/// The csv front end: decodes, lets GridCraft split the fields as text, and
/// makes a field a number only when it prints back the same, so `007`, `1E5`
/// and `SEPT1` stay (record 0006). One sheet, named after the file.
fn read_csv(path: &Path, name: &str, bytes: &[u8]) -> Result<Opened, CommandError> {
    let (text, warnings) = decode_text(path, bytes)?;
    let options =
        gridcraft_xlsx::CsvOptions { delimiter: 0, parse_values: false, ..Default::default() };
    let mut wb = gridcraft_xlsx::read_csv(text.as_bytes(), &options)
        .map_err(|e| internal("reading the csv file", &e.to_string()))?;
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
            if let Some((n, style)) = text.and_then(|t| csv_number(t, &mut styles, &mut made)) {
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

/// A workbook and what GridCraft skipped or repaired while reading it.
type Opened = (Workbook, Vec<String>);

/// Excel shows at most 30 decimals (and 15 significant digits); more stays text.
const MAX_DECIMALS: usize = 30;
type Found = Option<(f64, StyleId)>;

/// The number a field holds, only when it prints back as the same text:
/// `0.1` in General, `12.50` in the format `0.00`, one style per decimal
/// count. `-0` stays text, since an .xlsx file writes it as `0`. With at
/// most 15 significant digits, the format prints the shortest form of the
/// number padded with zeros, so comparing that form is enough.
fn csv_number(field: &str, styles: &mut StyleTable, made: &mut [Option<StyleId>]) -> Found {
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
fn decode_text(path: &Path, bytes: &[u8]) -> Result<(String, Vec<String>), CommandError> {
    let (encoding, body, how) = match Encoding::for_bom(bytes) {
        Some((encoding, bom)) => {
            (encoding, bytes.get(bom..).unwrap_or_default(), "its byte order mark names")
        }
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
            format!("{} could not be read as text", path.display()),
            format!("some bytes are not valid {name}, the encoding {how}"),
            "Save the file again as UTF-8 in the program that made it, then open that copy.",
        )
    })
}
