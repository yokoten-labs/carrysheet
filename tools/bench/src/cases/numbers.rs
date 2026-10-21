//! Apple Numbers: committed samples. There is no Numbers writer yet, so
//! no large generated file; add one when there is.

use carrysheet_numbers::Document;

use super::{Case, Input, Op, Operation, tally};

pub fn cases() -> Vec<Case> {
    vec![Case {
        name: "numbers-samples/open",
        op: Operation::Open,
        about: "read every committed .numbers sample",
        input: Input::Files {
            dirs: &["crates/carrysheet-numbers/tests/data"],
            exts: &["numbers", "zip"],
        },
        inner: 20,
        setup: read_each,
    }]
}

/// Reads each input; the check is the table count.
fn read_each(inputs: Vec<Vec<u8>>) -> Result<Op, String> {
    Ok(Box::new(move || {
        Ok(tally(inputs.iter().map(|b| Document::from_bytes(b)), |d| {
            d.sheets().iter().map(|s| s.tables().len() as u64).sum()
        }))
    }))
}
