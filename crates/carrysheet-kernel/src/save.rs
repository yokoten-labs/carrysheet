//! Encoding a book for saving, with the report of what the save keeps.

use std::borrow::Cow;
use std::path::Path;

use carrysheet_commands::engine::save::clean_from_file;
use carrysheet_commands::engine::{EncodeOptions, Encoded, FileFormat, Loss, SaveReport};
use carrysheet_commands::{CommandError, ErrorCode};
use gridcraft_engine::io::FileKind;
use gridcraft_engine::model::Workbook;

use crate::book::{Book, SourceKind};
use crate::{internal, package};

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
        "Try another output format; if it keeps failing, report it with `carrysheet-cli feedback file`.",
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
            Some(features) => {
                for (feature, n) in features {
                    report.lost.push(Loss {
                        feature: feature.to_owned(),
                        before: n,
                        after: 0,
                    });
                }
            }
            None => report.lost.push(package::unchecked_loss()),
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
