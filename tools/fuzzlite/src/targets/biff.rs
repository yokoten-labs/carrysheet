//! carrysheet-biff: `.xls` and `.xlw` files of every BIFF version.

use super::{SeedDir, Target};
use crate::mutate::Shape;

pub const TARGETS: &[Target] = &[Target {
    name: "biff",
    about: "carrysheet-biff: read (BIFF2 to BIFF8, OLE2 or raw), summaries, workbook_stream",
    run,
    seeds: &[
        SeedDir {
            dir: "crates/carrysheet-biff/tests/data",
            extensions: &["xls", "xlw"],
        },
        SeedDir {
            dir: "crates/carrysheet-vba/tests/data",
            extensions: &["xls"],
        },
    ],
    builtin_seeds: &[],
    shapes: &[Shape::Biff, Shape::Cfb],
    hidden: false,
}];

/// Reads the workbook and walks what came back.
pub fn run(data: &[u8]) {
    if let Ok(book) = carrysheet_biff::read(data) {
        let _ = book.summaries();
        for sheet in &book.sheets {
            for cell in &sheet.cells {
                let _ = book.is_date(cell);
            }
        }
    }
    let _ = carrysheet_biff::workbook_stream(data);
}
