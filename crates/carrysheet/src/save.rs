//! Saving: `convert`, what a save keeps and loses (record 0003, point 3),
//! csv escaping, and writing files so a failure never leaves half a workbook
//! behind.

use std::borrow::Cow;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use gridcraft_engine::io::FileKind;
use gridcraft_engine::model::Workbook;
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::args::{flag, input, shown, text};
use crate::commands::paths::display_path;
use crate::error::{CommandError, ErrorCode};
use crate::kernel::package;
use crate::open::{Book, SourceKind};
use crate::{Kernel, internal};

/// File formats Carrysheet writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FileFormat {
    Xlsx,
    Xls,
    Csv,
}

impl FileFormat {
    /// The format named by an extension (any case), such as `xlsx`. The
    /// macro-enabled `xlsm` is the same package format as `xlsx`.
    pub fn from_extension(ext: &str) -> Option<FileFormat> {
        match ext.to_ascii_lowercase().as_str() {
            "xlsx" | "xlsm" => Some(FileFormat::Xlsx),
            "xls" => Some(FileFormat::Xls),
            "csv" => Some(FileFormat::Csv),
            _ => None,
        }
    }

    /// The format as written in arguments and results: `xlsx`, `xls`, `csv`.
    pub fn as_str(self) -> &'static str {
        match self {
            FileFormat::Xlsx => "xlsx",
            FileFormat::Xls => "xls",
            FileFormat::Csv => "csv",
        }
    }

    /// The format of a path, from its extension.
    pub fn from_path(path: &Path) -> Option<FileFormat> {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(FileFormat::from_extension)
    }
}

/// How [`Kernel::encode`] writes a file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncodeOptions<'a> {
    /// For csv, the sheet to write; `None` means the first.
    pub sheet: Option<&'a str>,
    /// For csv, write text starting with `=`, `+`, `-`, `@`, tab or return
    /// as it is, instead of with a `'` in front (formula injection).
    pub csv_raw: bool,
}

/// Most dropped package parts listed by name; the rest are counted.
pub const MAX_LISTED_PARTS: usize = 20;

/// One kind of content a save would not keep.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Loss {
    /// What, in plain words: `charts`, `VBA project`, `formulas`.
    pub feature: String,
    /// How many the opened file has.
    pub before: u64,
    /// How many the saved file will have.
    pub after: u64,
}

/// How the package parts of the opened file fare in the saved one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct PartCounts {
    /// Written back byte for byte.
    pub identical: u64,
    /// Written again by the kernel (content kept, bytes differ).
    pub rewritten: u64,
    pub dropped: u64,
    /// New in the saved file.
    pub added: u64,
}

/// What a save will keep and lose, found before writing anything.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SaveReport {
    pub format: FileFormat,
    /// What the saved file will not have that the opened one had. Empty
    /// means nothing known is lost.
    pub lost: Vec<Loss>,
    /// Package parts of the opened file that the saved one will not have,
    /// as quoted and shortened names, at most [`MAX_LISTED_PARTS`].
    pub dropped_parts: Vec<String>,
    /// For an xlsx opened and saved as xlsx: the parts compared.
    pub parts: Option<PartCounts>,
    /// Things worth knowing that are not losses, such as a cache Excel
    /// rebuilds itself.
    pub notes: Vec<String>,
}

impl SaveReport {
    pub fn new(format: FileFormat) -> SaveReport {
        SaveReport {
            format,
            lost: Vec::new(),
            dropped_parts: Vec::new(),
            parts: None,
            notes: Vec::new(),
        }
    }

    /// True when saving would lose something, so it needs `confirm`.
    pub fn loses_something(&self) -> bool {
        !self.lost.is_empty() || !self.dropped_parts.is_empty()
    }

    /// One line naming every loss, such as `VBA project, charts (2), tables (1 of 3)`.
    pub fn summary(&self) -> String {
        let mut items: Vec<String> = self
            .lost
            .iter()
            .map(|l| match (l.before, l.after) {
                (1, 0) => l.feature.clone(),
                (n, 0) => format!("{} ({n})", l.feature),
                (n, kept) => format!("{} ({} of {n})", l.feature, n.saturating_sub(kept)),
            })
            .collect();
        let dropped = self.dropped_parts.len();
        if dropped > 0 {
            let more = if dropped >= MAX_LISTED_PARTS { "+" } else { "" };
            items.push(format!("{dropped}{more} package part(s)"));
        }
        items.join(", ")
    }
}

/// A workbook encoded for saving, and what the save keeps and loses.
#[derive(Debug, Clone, PartialEq)]
pub struct Encoded {
    pub bytes: Vec<u8>,
    pub report: SaveReport,
}

/// Text from a file, made safe to show: control, bidi and invisible
/// characters escaped, cut at `max` characters.
pub fn clean_from_file(text: &str, max: usize) -> String {
    escape(text, max, false)
}

/// [`clean_from_file`] in double quotes, with quotes and backslashes escaped,
/// for naming something from a file inside a message.
pub fn quote_from_file(text: &str, max: usize) -> String {
    format!("\"{}\"", escape(text, max, true))
}

fn escape(text: &str, max: usize, quotes: bool) -> String {
    let mut out = String::new();
    for (shown, ch) in text.chars().enumerate() {
        if shown == max {
            out.push_str("...");
            break;
        }
        let invisible = ch.is_control()
            || matches!(ch, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}');
        if invisible {
            out.push_str(&format!("\\u{{{:x}}}", ch as u32));
        } else if quotes && (ch == '"' || ch == '\\') {
            out.push('\\');
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Where `edit` writes when no output is given: beside the original, as
/// `name-edited.xlsx`, or `name-edited-2.xlsx` and so on when taken.
pub fn default_output(input: &Path) -> Result<PathBuf, CommandError> {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "book".to_owned());
    let ext = input
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let dir = input.parent().unwrap_or(Path::new(""));
    for n in 1..=99 {
        let name = if n == 1 {
            format!("{stem}-edited{ext}")
        } else {
            format!("{stem}-edited-{n}{ext}")
        };
        let candidate = dir.join(name);
        if std::fs::symlink_metadata(&candidate).is_err() {
            return Ok(candidate);
        }
    }
    Err(CommandError::new(
        ErrorCode::FileExists,
        format!("no free name beside {}", display_path(input)),
        "the names name-edited.xlsx to name-edited-99.xlsx are all taken",
        "Pass output with the file to write.",
    ))
}

/// Refuses to replace an existing file unless `overwrite` is set.
pub fn check_output(path: &Path, overwrite: bool) -> Result<(), CommandError> {
    if overwrite || std::fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    Err(CommandError::new(
        ErrorCode::FileExists,
        format!("{} already exists", display_path(path)),
        "Carrysheet never replaces a file unless asked",
        "Pass a different output, or set overwrite and confirm to true to replace it.",
    ))
}

fn io_error(path: &Path, e: &std::io::Error) -> CommandError {
    let code = match e.kind() {
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        std::io::ErrorKind::NotFound => ErrorCode::FileNotFound,
        _ => ErrorCode::Io,
    };
    CommandError::new(
        code,
        format!("{} could not be written", display_path(path)),
        e.to_string(),
        "Check that the folder exists, you may write there, and no other program has the file open.",
    )
}

thread_local! {
    /// The folder [`write_atomic`] must stay inside on this thread (the MCP
    /// root, canonical), while a [`WriteRoot`] lives.
    static WRITE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Confines [`write_atomic`] on this thread to a folder until dropped.
#[must_use = "the confinement ends when this is dropped"]
pub struct WriteRoot(Option<PathBuf>);

/// Keeps every [`write_atomic`] on this thread inside `root` while the
/// returned guard lives (review round 2, L-a): a link changed after the
/// path was resolved cannot lead the write outside.
pub fn confine_writes(root: &Path) -> WriteRoot {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    WriteRoot(WRITE_ROOT.with(|r| r.replace(Some(root))))
}

impl Drop for WriteRoot {
    fn drop(&mut self) {
        let previous = self.0.take();
        WRITE_ROOT.with(|r| *r.borrow_mut() = previous);
    }
}

/// Refuses when a write confinement is set and `target`'s folder, with
/// every link followed, is not inside it, or `target` itself now leads
/// elsewhere than `expected`.
fn still_inside(target: &Path, expected: &Path, dir: &Path) -> Result<(), CommandError> {
    let Some(root) = WRITE_ROOT.with(|r| r.borrow().clone()) else {
        return Ok(());
    };
    let inside = link_target(target) == expected
        && std::fs::canonicalize(dir).is_ok_and(|d| d.starts_with(&root));
    if inside {
        return Ok(());
    }
    Err(CommandError::new(
        ErrorCode::OutsideRoot,
        format!("{} is outside the root folder", display_path(target)),
        "a link on the way to the file changed while it was being saved, and now leads outside the root folder",
        "Use a path inside the root folder that is not a link changed by another program; nothing was written.",
    ))
}

/// Writes `bytes` to `path` through a temporary file in the same folder,
/// then renames it into place: a failure leaves the old file as it was.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CommandError> {
    // A link is written through: the file it points to is replaced and the
    // link stays (review M2).
    let given = path;
    let real = link_target(path);
    let path = real.as_path();
    // An existing file keeps its permissions (a private 600 file must not
    // become readable by others).
    let keep = std::fs::metadata(path).ok().map(|m| m.permissions());
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.{}.carrysheet-tmp", std::process::id()));
    still_inside(given, path, dir)?;
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        if let Some(permissions) = keep {
            // Set before any byte is written.
            file.set_permissions(permissions)?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_error(path, &e));
    }
    // Checked again just before the rename: links may change meanwhile.
    if let Err(e) = still_inside(given, path, dir) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        io_error(path, &e)
    })
}

/// The file `path` names, following symbolic links (at most 40, as the
/// system does). A link to a missing file gives the missing file's path.
fn link_target(path: &Path) -> std::path::PathBuf {
    let mut p = path.to_path_buf();
    for _ in 0..40 {
        let is_link = std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink());
        if !is_link {
            break;
        }
        let Ok(to) = std::fs::read_link(&p) else {
            break;
        };
        p = match p.parent() {
            Some(dir) if to.is_relative() => dir.join(to),
            _ => to,
        };
    }
    p
}

pub(crate) fn encode(
    book: &Book,
    path: &Path,
    format: FileFormat,
    options: &EncodeOptions<'_>,
    csv_bytes: usize,
) -> Result<Encoded, CommandError> {
    // GridCraft picks xlsx or xlsm (with the VBA project) by the name.
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "book.xlsx".to_owned());
    match format {
        FileFormat::Xls => Err(CommandError::new(
            ErrorCode::NotImplemented,
            "writing .xls is not built yet",
            "Carrysheet reads .xls but writes only .xlsx, .xlsm and .csv so far",
            "Write .xlsx (or .xlsm to keep macros) instead.",
        )),
        FileFormat::Xlsx => encode_xlsx(book, path, &name),
        FileFormat::Csv => encode_csv(book, options, csv_bytes),
    }
}

fn save_failed(e: &gridcraft_engine::EngineError) -> CommandError {
    if let gridcraft_engine::EngineError::Internal(_, detail) = e {
        return internal("saving", detail);
    }
    CommandError::new(
        ErrorCode::Io,
        "the workbook could not be encoded",
        clean_from_file(&e.to_string(), 200),
        "Try another output format; if it keeps failing, report it at https://github.com/yokoten-labs/carrysheet/issues.",
    )
}

fn encode_xlsx(book: &Book, path: &Path, name: &str) -> Result<Encoded, CommandError> {
    let wb = book.wb()?;
    // Relative links to other workbooks still name the same files from
    // the output's folder.
    let target_dir = std::path::absolute(path)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let moved = target_dir.is_some() && target_dir != wb.source_dir;
    let has_links = wb.preserved.as_deref().is_some_and(|p| {
        p.parts.iter().flat_map(|part| part.rels.iter()).any(|r| {
            r.external
                && r.rel_type
                    .to_ascii_lowercase()
                    .ends_with("externallinkpath")
        })
    });
    let wb: Cow<'_, Workbook> = match target_dir.and_then(|d| gridcraft_xlsx::rebase_links(wb, &d))
    {
        Some(rebased) => Cow::Owned(rebased),
        None => Cow::Borrowed(wb),
    };
    // rebase_links keeps a path it cannot rewrite safely (another drive, or
    // one that would name folders outside the source's).
    let links_kept_paths = moved && has_links && matches!(wb, Cow::Borrowed(_));
    let bytes = gridcraft_engine::io::save_bytes_as(&wb, name, FileKind::Xlsx)
        .map_err(|e| save_failed(&e))?;
    // The saved file must open again; never write one that does not.
    if let Err(e) = gridcraft_xlsx::read_xlsx(&bytes) {
        return Err(internal(
            "checking the saved file",
            &format!("the written workbook does not open again: {e}"),
        ));
    }
    let mut report = SaveReport::new(FileFormat::Xlsx);
    match book.kind {
        SourceKind::Package => package::compare(&book.source, &bytes, &mut report),
        SourceKind::Csv => {}
        SourceKind::Imported(format) => report.lost.push(Loss {
            feature: format!("anything the .{format} import skips (Carrysheet cannot list it yet)"),
            before: 1,
            after: 0,
        }),
    }
    open_warnings(book, &mut report);
    if links_kept_paths {
        report.notes.push(
            "links to other workbooks keep their paths as written; they may not find their files from the new folder".into(),
        );
    }
    Ok(Encoded { bytes, report })
}

/// What opening the file could not read or keep: a loss when content was
/// dropped or unreadable, a note otherwise.
fn open_warnings(book: &Book, report: &mut SaveReport) {
    const LOST: [&str; 6] = [
        "dropped",
        "skipped",
        "could not be read",
        "not kept",
        "not retained",
        "missing",
    ];
    for w in &book.warnings {
        let text = clean_from_file(w, 200);
        if LOST.iter().any(|l| w.contains(l)) {
            report.lost.push(Loss {
                feature: format!("content the opened file could not give: {text}"),
                before: 1,
                after: 0,
            });
        } else {
            report.notes.push(text);
        }
    }
}

/// Text a spreadsheet program may run as a formula when it opens a csv
/// file (formula injection): it starts with one of these.
const CSV_FORMULA_STARTS: [char; 6] = ['=', '+', '-', '@', '\t', '\r'];

/// One sheet's values as csv. Text that a spreadsheet could take for a
/// formula gets a `'` in front unless `options.csv_raw` is set; numbers are
/// never changed.
fn csv_bytes_of(
    wb: &Workbook,
    si: usize,
    options: &EncodeOptions<'_>,
    limit: usize,
) -> Result<(Vec<u8>, u64), CommandError> {
    let Some(sh) = wb.sheets.get(si) else {
        return Err(internal("saving", "sheet index out of range"));
    };
    let mut escaped = 0u64;
    let bytes =
        gridcraft_xlsx::write_csv_limited(sh, wb.csv_delimiter.unwrap_or(b','), limit, |c| {
            let value = sh.value(c);
            // General's grid-width formatting rounds long numbers; a csv
            // file has no column width, so the whole number is written.
            let text = if wb
                .styles
                .get(sh.style_id(c))
                .num_fmt
                .as_str()
                .eq_ignore_ascii_case("General")
                && let gridcraft_engine::core::Value::Number(n) = value
                && n.is_finite()
            {
                n.to_string()
            } else {
                gridcraft_engine::display::value_text(wb, sh, c)
            };
            let is_number = matches!(value, gridcraft_engine::core::Value::Number(_));
            if !options.csv_raw && !is_number && text.starts_with(CSV_FORMULA_STARTS) {
                escaped += 1;
                format!("'{text}")
            } else {
                text
            }
        })
        .map_err(|e| {
            CommandError::new(
                ErrorCode::LimitExceeded,
                "the csv file would be too large",
                clean_from_file(&e.to_string(), 200),
                "Save as .xlsx, or copy the part you need to a new sheet and save that.",
            )
        })?;
    Ok((bytes, escaped))
}

fn encode_csv(
    book: &Book,
    options: &EncodeOptions<'_>,
    limit: usize,
) -> Result<Encoded, CommandError> {
    let si = book.sheet_index(options.sheet)?;
    let wb = book.wb()?;
    let (bytes, escaped) = csv_bytes_of(wb, si, options, limit)?;
    let mut report = SaveReport::new(FileFormat::Csv);
    report
        .notes
        .push("csv keeps the values of one sheet as they are shown; no formatting".into());
    if escaped > 0 {
        report.notes.push(format!(
            "{escaped} cells start with = + - @ or a tab and were written with a ' in front, so a spreadsheet does not run them as formulas; use --csv-raw to write them as they are"
        ));
    }
    let has_content = |sh: &gridcraft_engine::model::Sheet| {
        sh.cells
            .iter()
            .any(|(_, c)| c.formula.is_some() || !c.value.is_empty())
    };
    let others = wb
        .sheets
        .iter()
        .enumerate()
        .filter(|(i, sh)| *i != si && has_content(sh))
        .count() as u64;
    if others > 0 {
        report.lost.push(Loss {
            feature: "other sheets".into(),
            before: others,
            after: 0,
        });
    }
    let formulas = wb.sheets.get(si).map_or(0, |sh| {
        sh.cells.iter().filter(|(_, c)| c.formula.is_some()).count()
    }) as u64;
    if formulas > 0 {
        report.lost.push(Loss {
            feature: "formulas (their values are kept)".into(),
            before: formulas,
            after: 0,
        });
    }
    match book.kind {
        SourceKind::Package => match package::source_features(&book.source) {
            Ok(features) => {
                for (feature, n) in features {
                    report.lost.push(Loss {
                        feature: feature.to_owned(),
                        before: n,
                        after: 0,
                    });
                }
            }
            Err(why) => report.lost.push(package::unchecked_loss(&why)),
        },
        SourceKind::Csv => {}
        SourceKind::Imported(format) => report.lost.push(Loss {
            feature: format!("anything else the .{format} file holds"),
            before: 1,
            after: 0,
        }),
    }
    Ok(Encoded { bytes, report })
}

/// Writes the workbook in another format.
pub(crate) fn convert(
    kernel: &Kernel,
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
    let book = kernel.open(&path)?;
    let options = EncodeOptions {
        sheet: text(args, "sheet"),
        csv_raw: flag(args, "csv_raw"),
    };
    let encoded = kernel.encode(&book, &output, format, &options)?;
    let mut data = Map::new();
    data.insert("file".into(), shown(&path));
    data.insert("output".into(), shown(&output));
    finish_save(args, &output, encoded, data)
}

/// The format to write: `format` when given, else from the extension.
pub(crate) fn output_format(
    output: &Path,
    format: Option<&str>,
) -> Result<FileFormat, CommandError> {
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
pub(crate) fn finish_save(
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

fn loss_error(output: &Path, report: &SaveReport) -> CommandError {
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
