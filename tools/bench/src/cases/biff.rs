//! Legacy Excel (.xls): committed samples, a generated 1,048,576-cell
//! file and a range read from it, and a hostile record flood. There is no
//! .xls writer, so no save case; the kernel (issue 45) adds save and
//! recalculation for the files it opens.

use carrysheet_biff::{BiffWorkbook, CellValue, read};

use super::{Case, Input, Op, Operation, XLS_1M, XLS_RECORDS, one, tally};

pub fn cases() -> Vec<Case> {
    use Operation::{Open, ReadRange};
    vec![
        Case {
            name: "xls-samples/open",
            op: Open,
            about: "read every committed .xls sample",
            input: Input::Files {
                dirs: &["crates/carrysheet-biff/tests/data"],
                exts: &["xls"],
            },
            inner: 15,
            setup: read_each,
        },
        Case {
            name: "xls-1m-cells/open",
            op: Open,
            about: "open a 1,048,576-cell .xls with 262,144 formulas",
            input: Input::Generated(&XLS_1M),
            inner: 1,
            setup: read_each,
        },
        Case {
            name: "xls-1m-cells/read-range",
            op: ReadRange,
            about: "read A30001:P31000 of the opened 1,048,576-cell sheet",
            input: Input::Generated(&XLS_1M),
            inner: 200,
            setup: read_range,
        },
        Case {
            name: "xls-hostile-many-records/open",
            op: Open,
            about: "hostile: a sheet of 3,000,000 unknown empty records",
            input: Input::Generated(&XLS_RECORDS),
            inner: 1,
            setup: read_each,
        },
    ]
}

fn cells(book: &BiffWorkbook) -> u64 {
    book.sheets.iter().map(|s| s.cells.len() as u64).sum()
}

/// Reads each input; the check is the cell count.
fn read_each(inputs: Vec<Vec<u8>>) -> Result<Op, String> {
    Ok(Box::new(move || {
        Ok(tally(inputs.iter().map(|b| read(b)), cells))
    }))
}

/// Opens the file untimed, then times looking up a 1,000 x 16 range.
fn read_range(inputs: Vec<Vec<u8>>) -> Result<Op, String> {
    let book = read(&one(inputs)?).map_err(|e| e.to_string())?;
    Ok(Box::new(move || {
        let sheet = &book.sheets[0];
        let mut check = 0u64;
        for row in 30_000..31_000 {
            for col in 0..16 {
                match sheet.cell(row, col).map(|c| &c.value) {
                    Some(CellValue::Number(n)) => check = check.wrapping_add(n.to_bits()),
                    Some(_) => check += 1,
                    None => return Err(format!("no cell at row {row}, column {col}")),
                }
            }
        }
        Ok(check)
    }))
}
