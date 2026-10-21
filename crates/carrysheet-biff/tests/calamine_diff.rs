//! Cross-checks our reading against calamine (MIT) on every sample it can
//! read.
//!
//! Cell values must match exactly, apart from the files in
//! `CALAMINE_VALUE_BUGS`. Formula text is compared where calamine
//! renders the formula without its known problems (see `calamine_is_off`);
//! the rest is printed with `--nocapture` for review.

// The test reports what it compared with --nocapture; the workspace denies
// println! only so that library and MCP code never write to stdout.
#![allow(clippy::print_stdout)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::Path;

use calamine::{Data, Reader, Xls};
use carrysheet_biff::{BiffWorkbook, CellValue, cell_name};

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Num(f64),
    Text(String),
    Bool(bool),
    Err(String),
}

fn ours(v: &CellValue) -> Option<Val> {
    match v {
        CellValue::Empty => None,
        CellValue::Number(n) => Some(Val::Num(*n)),
        CellValue::Text(t) => Some(Val::Text(t.to_string())),
        CellValue::Bool(b) => Some(Val::Bool(*b)),
        CellValue::Error(e) => Some(Val::Err(e.as_str().to_string())),
    }
}

fn theirs(d: &Data) -> Option<Val> {
    match d {
        Data::Empty => None,
        Data::Int(i) => Some(Val::Num(*i as f64)),
        Data::Float(f) => Some(Val::Num(*f)),
        Data::String(s) => Some(Val::Text(s.clone())),
        Data::Bool(b) => Some(Val::Bool(*b)),
        Data::DateTime(dt) => Some(Val::Num(dt.as_f64())),
        Data::DateTimeIso(s) | Data::DurationIso(s) => Some(Val::Text(s.clone())),
        Data::Error(e) => Some(Val::Err(e.to_string())),
    }
}

/// calamine 0.36 formula text we do not compare against, with the reason:
///
/// - `$`: calamine swaps the row and column relative flags of BIFF8
///   PtgRef/PtgArea ([MS-XLS] ColRelU in RgceLoc: bit 14 is colRelative and
///   bit 15 rowRelative; Apache POI's RefPtgBase agrees with us), so every
///   mixed reference comes out wrong.
/// - `!`: calamine reads the column of PtgRef3d/PtgArea3d with the flag bits
///   still in it (columns such as BTRM) and ignores sheet ranges.
/// - spaces: calamine renders PtgAttrSpace; we drop it, as Apache POI does.
/// - `User(`, `[PtgNameX]`, `Unrecognised`: tokens calamine does not render
///   (add-in and user functions, external names, PtgTbl, PtgMemFunc,
///   PtgMemArea).
fn calamine_is_off(f: &str) -> bool {
    f.contains('$')
        || f.contains('!')
        || f.contains(' ')
        || f.contains("User(")
        || f.contains("[PtgNameX]")
        || f.starts_with("Unrecognised")
}

/// Files where calamine 0.36 reads cell text wrongly, with the reason. Each
/// must still differ (so the list cannot go stale), and nothing else may.
///
/// The first two are the same calamine bug: an SST string with fHighByte = 1 is
/// UTF-16LE whatever the CODEPAGE record says ([MS-XLS] 2.5.293
/// XLUnicodeRichExtendedString), but calamine runs it through the code page
/// (1252), giving "T\0i\0t\0e\0l\0" for "Titel" and "?" for U+25A0.
const CALAMINE_VALUE_BUGS: &[(&str, &str)] = &[
    (
        "sheet_name_parsing.xls",
        "UTF-16 SST strings decoded as code page 1252",
    ),
    (
        "biff5-rich-text-string.xls",
        "UTF-16 SST string decoded as code page 1252 (H3: U+25A0, U+2022, U+2019)",
    ),
    // calamine 0.36 reads a chart's cached series values (SIIndex followed
    // by NUMBER records inside an embedded chart substream, [MS-XLS] 2.4.236)
    // as cells of the sheet that holds the chart. 0.37 no longer does.
    ("WithChart.xls", "chart series cache read as sheet cells"),
    ("OOM_alloc.xls", "chart series cache read as sheet cells"),
];

/// Single formulas calamine 0.36 renders wrongly, as "file / sheet / cell",
/// with the reason. Each must still differ.
const CALAMINE_FORMULA_BUGS: &[(&str, &str)] = &[(
    "SharedFormulaTest.xls / 0 / DY1",
    "shared PtgRefN column offset applied past column 255 wrongly (DZ1 shown as Z1); \
     [MS-XLS] RgceLocRel adds the signed offset to the cell column",
)];

#[derive(Default)]
struct Tally {
    values: usize,
    value_diffs: Vec<String>,
    formulas: usize,
    formula_diffs: Vec<String>,
    skipped_formulas: Vec<String>,
}

fn compare(file: &str, book: &BiffWorkbook, bytes: &[u8], t: &mut Tally) -> Result<(), String> {
    let mut cal = Xls::new(Cursor::new(bytes.to_vec())).map_err(|e| e.to_string())?;
    let names = cal.sheet_names().to_vec();
    for name in names {
        let Some(sheet) = book.sheets.iter().find(|s| s.name == name) else {
            t.value_diffs
                .push(format!("{file}: sheet {name:?} is missing"));
            continue;
        };
        let range = cal
            .worksheet_range(&name)
            .map_err(|e| format!("calamine failed on sheet {name}: {e}"))?;
        let (r0, c0) = range.start().unwrap_or((0, 0));
        let theirs_map: BTreeMap<(u32, u32), Val> = range
            .used_cells()
            .filter_map(|(r, c, d)| theirs(d).map(|v| ((r as u32 + r0, c as u32 + c0), v)))
            .collect();
        let ours_map: BTreeMap<(u32, u32), Val> = sheet
            .cells
            .iter()
            .filter_map(|c| ours(&c.value).map(|v| ((c.row, c.col as u32), v)))
            .collect();
        let keys: BTreeSet<_> = theirs_map.keys().chain(ours_map.keys()).copied().collect();
        for k in keys {
            let (a, b) = (ours_map.get(&k), theirs_map.get(&k));
            t.values += 1;
            let same = match (a, b) {
                (Some(Val::Num(x)), Some(Val::Num(y))) => x == y || (x.is_nan() && y.is_nan()),
                _ => a == b,
            };
            if !same {
                t.value_diffs.push(format!(
                    "{file} / {name} / {}: ours {a:?}, calamine {b:?}",
                    cell_name(k.0, k.1 as u16)
                ));
            }
        }
        let Ok(frange) = cal.worksheet_formula(&name) else {
            continue;
        };
        let (fr0, fc0) = frange.start().unwrap_or((0, 0));
        for (r, c, f) in frange.used_cells() {
            if f.is_empty() {
                continue;
            }
            let (row, col) = (r as u32 + fr0, (c as u32 + fc0) as u16);
            let mine = sheet
                .cell(row, col)
                .and_then(|c| c.formula.as_ref())
                .and_then(|f| f.text.as_str())
                .unwrap_or("<none>");
            let line = format!(
                "{file} / {name} / {}: ours {mine:?}, calamine {f:?}",
                cell_name(row, col)
            );
            if calamine_is_off(f) {
                if mine != f {
                    t.skipped_formulas.push(line);
                }
                continue;
            }
            t.formulas += 1;
            if mine != f {
                t.formula_diffs.push(line);
            }
        }
    }
    Ok(())
}

#[test]
fn values_and_formulas_match_calamine() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut files: Vec<_> = [dir.clone(), dir.join("calamine")]
        .iter()
        .flat_map(|d| std::fs::read_dir(d).unwrap())
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "xls"))
        .collect();
    files.sort();
    let mut t = Tally::default();
    let mut compared_files = 0;
    for path in files {
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&path).unwrap();
        let Ok(book) = carrysheet_biff::read(&bytes) else {
            println!("{file}: we refuse it (see samples.rs)");
            continue;
        };
        // calamine 0.36 panics on some damaged files; that is calamine's
        // problem, not a difference to report.
        let mut part = Tally::default();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compare(&file, &book, &bytes, &mut part)
        }));
        match outcome {
            Ok(Ok(())) => {
                compared_files += 1;
                t.values += part.values;
                t.formulas += part.formulas;
                t.value_diffs.extend(part.value_diffs);
                t.formula_diffs.extend(part.formula_diffs);
                t.skipped_formulas.extend(part.skipped_formulas);
            }
            Ok(Err(e)) => println!("{file}: calamine cannot read it ({e})"),
            Err(_) => println!("{file}: calamine panicked on it"),
        }
    }
    for line in &t.skipped_formulas {
        println!("not compared (calamine's rendering is off): {line}");
    }
    println!(
        "{compared_files} files, {} cell values and {} formulas compared",
        t.values, t.formulas
    );
    assert!(compared_files >= 40, "only {compared_files} files compared");
    for (file, reason) in CALAMINE_VALUE_BUGS {
        let prefix = format!("{file} /");
        let n = t
            .value_diffs
            .iter()
            .filter(|d| d.starts_with(&prefix))
            .count();
        assert!(
            n > 0,
            "{file} no longer differs; drop it from CALAMINE_VALUE_BUGS"
        );
        println!("{n} known calamine differences in {file}: {reason}");
        t.value_diffs.retain(|d| !d.starts_with(&prefix));
    }
    assert!(
        t.value_diffs.is_empty(),
        "value differences:\n{}",
        t.value_diffs.join("\n")
    );
    for (cell, reason) in CALAMINE_FORMULA_BUGS {
        let prefix = format!("{cell}:");
        let n = t
            .formula_diffs
            .iter()
            .filter(|d| d.starts_with(&prefix))
            .count();
        assert!(
            n > 0,
            "{cell} no longer differs; drop it from CALAMINE_FORMULA_BUGS"
        );
        println!("known calamine formula difference at {cell}: {reason}");
        t.formula_diffs.retain(|d| !d.starts_with(&prefix));
    }
    assert!(
        t.formula_diffs.is_empty(),
        "formula differences:\n{}",
        t.formula_diffs.join("\n")
    );
}
