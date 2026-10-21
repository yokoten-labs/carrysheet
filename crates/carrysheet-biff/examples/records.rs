//! Lists the physical records of a .xls workbook stream (a debugging aid).
//!
//! cargo run -p carrysheet-biff --example records -- book.xls [type in hex]
//!
//! With a type, whole record bodies are printed; without one, the first 48
//! bytes.

// An example prints its results to stdout; the workspace denies println! only
// so that library and MCP code can never write there by accident.
#![allow(clippy::print_stdout)]

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: records <file.xls> [record type in hex]");
        std::process::exit(2);
    };
    let only = args
        .next()
        .and_then(|t| u16::from_str_radix(t.trim_start_matches("0x"), 16).ok());
    let bytes = std::fs::read(&path).unwrap_or_default();
    let stream = match carrysheet_biff::workbook_stream(&bytes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(1);
        }
    };
    let mut pos = 0usize;
    while pos + 4 <= stream.len() {
        let typ = u16::from_le_bytes([stream[pos], stream[pos + 1]]);
        let len = u16::from_le_bytes([stream[pos + 2], stream[pos + 3]]) as usize;
        let end = (pos + 4 + len).min(stream.len());
        if only.is_none_or(|t| t == typ) {
            let body: String = stream[pos + 4..end]
                .iter()
                .take(if only.is_some() { usize::MAX } else { 48 })
                .map(|b| format!("{b:02x}"))
                .collect();
            println!("{pos:8} {typ:04x} {len:5} {body}");
        }
        pos += 4 + len;
    }
}
