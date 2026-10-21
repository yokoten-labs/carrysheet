//! CSV: committed samples, a 100 MB file, a wide file and a legacy
//! encoding, each opened and saved, and two hostile shapes.

use carrysheet_csv::{ReadOptions, ReadResult, WriteOptions, read, write};

use super::{
    CSV_100MB, CSV_EMPTY, CSV_GBK, CSV_QUOTES, CSV_WIDE, Case, Input, Op, Operation, tally,
};

const SAMPLES: Input = Input::Files {
    dirs: &["crates/carrysheet-csv/tests/data"],
    exts: &["csv", "txt"],
};

pub fn cases() -> Vec<Case> {
    use Operation::{Open, Save};
    vec![
        Case {
            name: "csv-samples/open",
            op: Open,
            about: "read every committed CSV sample (legacy encodings, shapes, Unicode)",
            input: SAMPLES,
            inner: 1000,
            setup: read_each,
        },
        Case {
            name: "csv-samples/save",
            op: Save,
            about: "write every committed CSV sample back the way it came",
            input: SAMPLES,
            inner: 5000,
            setup: write_back,
        },
        Case {
            name: "csv-100mb/open",
            op: Open,
            about: "read a 100 MB, 880,000-row CSV",
            input: Input::Generated(&CSV_100MB),
            inner: 1,
            setup: read_each,
        },
        Case {
            name: "csv-100mb/save",
            op: Save,
            about: "write the rows of the 100 MB CSV back the way it came",
            input: Input::Generated(&CSV_100MB),
            inner: 1,
            setup: write_back,
        },
        Case {
            name: "csv-wide/open",
            op: Open,
            about: "read 256 rows of 16,384 columns",
            input: Input::Generated(&CSV_WIDE),
            inner: 1,
            setup: read_each,
        },
        Case {
            name: "csv-wide/save",
            op: Save,
            about: "write the 256 rows of 16,384 columns back",
            input: Input::Generated(&CSV_WIDE),
            inner: 3,
            setup: write_back,
        },
        Case {
            name: "csv-gbk-20mb/open",
            op: Open,
            about: "detect and decode 20 MB of GBK text",
            input: Input::Generated(&CSV_GBK),
            inner: 1,
            setup: read_each,
        },
        Case {
            name: "csv-gbk-20mb/save",
            op: Save,
            about: "encode the 20 MB of text back to GBK",
            input: Input::Generated(&CSV_GBK),
            inner: 1,
            setup: write_back,
        },
        Case {
            name: "csv-hostile-quotes/open",
            op: Open,
            about: "hostile: 200,000 rows of fields full of doubled quotes and line breaks",
            input: Input::Generated(&CSV_QUOTES),
            inner: 1,
            setup: read_each,
        },
        Case {
            name: "csv-hostile-empty-cells/open",
            op: Open,
            about: "hostile: 9 million empty cells in 10 MB",
            input: Input::Generated(&CSV_EMPTY),
            inner: 1,
            setup: read_each,
        },
    ]
}

/// Reads each input; the check is the cell count.
fn read_each(inputs: Vec<Vec<u8>>) -> Result<Op, String> {
    Ok(Box::new(move || {
        Ok(tally(
            inputs.iter().map(|b| read(b, &ReadOptions::default())),
            |r| r.rows.iter().map(|row| row.len() as u64).sum(),
        ))
    }))
}

/// Reads the inputs untimed (dropping their bytes), then times writing
/// each back with the encoding, delimiter and line ending it came with.
/// The check is the bytes written.
fn write_back(inputs: Vec<Vec<u8>>) -> Result<Op, String> {
    let files: Vec<ReadResult> = inputs
        .into_iter()
        .filter_map(|b| read(&b, &ReadOptions::default()).ok())
        .collect();
    if files.is_empty() {
        return Err("no input could be read, so there is nothing to save".into());
    }
    Ok(Box::new(move || {
        Ok(tally(
            files
                .iter()
                .map(|f| write(&f.rows, &WriteOptions::matching(&f.detected))),
            |out| out.len() as u64,
        ))
    }))
}
