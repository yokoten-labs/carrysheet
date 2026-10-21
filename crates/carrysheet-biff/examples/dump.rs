//! Prints what the reader sees in a .xls file.
//!
//! cargo run -p carrysheet-biff --example dump -- book.xls [max cells per sheet]

// An example prints its results to stdout; the workspace denies println! only
// so that library and MCP code can never write there by accident.
#![allow(clippy::print_stdout)]

use carrysheet_biff::{CellValue, FormulaText, cell_name};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: dump <file.xls> [max cells per sheet]");
        std::process::exit(2);
    };
    let max: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(50);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    let book = match carrysheet_biff::read(&bytes) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "{} ({}), {:?}, code page {:?}, 1904 dates: {}",
        book.version.name(),
        book.version.excel(),
        book.container,
        book.codepage,
        book.date_1904
    );
    for n in &book.names {
        println!(
            "name {:?} sheet {:?} = {:?}",
            n.name, n.sheet, n.formula.text
        );
    }
    for (sheet, s) in book.sheets.iter().zip(book.summaries()) {
        println!(
            "sheet {:?} {:?} {:?}: used {:?}, {} cells, {} formulas ({} unparsed), {} merged, {} raw records",
            s.name,
            s.kind,
            s.visibility,
            s.used_range.map(|r| r.to_a1()),
            s.cells,
            s.formulas,
            s.unparsed_formulas,
            s.merged_ranges,
            s.raw_records
        );
        for c in sheet.cells.iter().take(max) {
            let v = match &c.value {
                CellValue::Empty => "(empty)".to_string(),
                CellValue::Number(n) => format!("{n}"),
                CellValue::Text(t) => format!("{t:?}"),
                CellValue::Bool(b) => format!("{b}"),
                CellValue::Error(e) => e.as_str().to_string(),
            };
            let f = match c.formula.as_deref().map(|f| &f.text) {
                Some(FormulaText::Parsed(t)) => format!("  ={t}"),
                Some(FormulaText::Unparsed { reason, at }) => format!("  =? ({reason} at {at})"),
                None => String::new(),
            };
            println!(
                "  {} [{}] {v}{f}",
                cell_name(c.row, c.col),
                book.number_format(c)
            );
        }
    }
    for w in &book.warnings {
        println!("warning at {}: {}", w.offset, w.message);
    }
}
