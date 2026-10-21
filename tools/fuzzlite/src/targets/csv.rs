//! carrysheet-csv: every entry point that takes bytes.

use carrysheet_csv::{Malformed, ReadOptions, WriteOptions};

use super::{SeedDir, Target};
use crate::mutate::Shape;

pub const TARGETS: &[Target] = &[Target {
    name: "csv",
    about: "carrysheet-csv: detect_encoding, read (strict and replacing), read_from, detect_delimiter, write back",
    run,
    seeds: &[SeedDir {
        dir: "crates/carrysheet-csv/tests/data",
        extensions: &[],
    }],
    builtin_seeds: &[b"a,b\r\n1,\"x\"\"y\"\r\n", b"sep=;\na;b\n"],
    shapes: &[Shape::Csv],
    hidden: false,
}];

/// Reads `data` every way the crate offers, then writes what it read.
pub fn run(data: &[u8]) {
    let _ = carrysheet_csv::detect_encoding(data);
    let strict = ReadOptions::default();
    if let Ok(file) = carrysheet_csv::read(data, &strict) {
        let _ = carrysheet_csv::write(&file.rows, &WriteOptions::matching(&file.detected));
    }
    let replace = ReadOptions {
        malformed: Malformed::Replace,
        ..ReadOptions::default()
    };
    let _ = carrysheet_csv::read(data, &replace);
    let _ = carrysheet_csv::read_from(data, &strict);
    let _ = carrysheet_csv::detect_delimiter(&String::from_utf8_lossy(data));
}
