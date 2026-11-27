//! Every file the removed readers were tested with, opened the way the
//! program opens a path, then damaged in seeded ways and opened as bytes.
//! Formats Carrysheet does not open (Numbers, BIFF2 to BIFF7) must be
//! refused with an error that names the reason.

use std::path::Path;

use carrysheet_commands::ErrorCode;

use crate::support::{
    Depth, EXPIRY, ISSUE, Input, Report, Rng, Waiver, check_waivers, load_parts, mentions,
    mutate_binary, mutate_csv, sample_files, short_name,
};
use crate::{biff, cfb, xlsb};

/// Known failures of this file's cases. Each needs a reason, an issue and
/// the expiry `v0.1.0`; a waiver that no case needs fails the test.
const WAIVERS: &[Waiver] = &[
    Waiver {
        case: "biff/testEXCEL_3.xls",
        reason: "GridCraft reads a raw BIFF3 stream named .xls as csv text",
        issue: Some(ISSUE),
        expires: EXPIRY,
    },
    Waiver {
        case: "biff/testEXCEL_4.xls",
        reason: "GridCraft reads a raw BIFF4 stream named .xls as csv text",
        issue: Some(ISSUE),
        expires: EXPIRY,
    },
];

/// Damaged copies made of each input.
const MUTATIONS_PER_INPUT: usize = 6;

/// What a sample is, from its name and its first bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// xls (BIFF8), xlsx, csv: opened, or refused because it is damaged
    /// or protected.
    Opens,
    /// An Apple Numbers document or package.
    Numbers,
    /// An Excel file older than BIFF8.
    OldExcel(u16),
}

fn kind(path: &Path, bytes: &[u8]) -> Kind {
    let name = short_name(path);
    if name.starts_with("numbers/") {
        return Kind::Numbers;
    }
    match biff::version(bytes) {
        Some(v) if v < 8 => Kind::OldExcel(v),
        _ => Kind::Opens,
    }
}

#[test]
fn waivers_are_complete() {
    check_waivers(WAIVERS);
}

#[test]
fn every_sample_opens_or_is_refused_clearly() {
    let files = sample_files();
    assert!(files.len() >= 100, "{} sample files", files.len());
    let mut report = Report::default();
    for path in &files {
        let name = short_name(path);
        let bytes = std::fs::read(path).unwrap();
        let kind = kind(path, &bytes);
        let (case, outcome) = if name.ends_with(".parts") {
            // A package written as text: zipped, it is an xlsx.
            let case = format!("{name} as xlsx");
            let outcome = report.run(
                case.clone(),
                Input::Bytes("package.xlsx", load_parts(path)),
                Depth::Save,
            );
            (case, outcome)
        } else {
            let outcome = report.run(name.clone(), Input::File(path), Depth::Save);
            (name.clone(), outcome)
        };
        let Some(outcome) = outcome else { continue };
        let refused = outcome.refused();
        match kind {
            Kind::Opens => {}
            Kind::Numbers => match refused {
                Some(e) if e.code == ErrorCode::UnsupportedFormat => {}
                _ => report.fail(
                    &case,
                    format!("a Numbers file was not refused: {outcome:?}"),
                ),
            },
            Kind::OldExcel(v) => match refused {
                Some(e) if mentions(&e.why, &["BIFF", "Excel 5", "version"]) => {}
                _ => report.fail(
                    &case,
                    format!("a BIFF{v} file was not refused with its version: {outcome:?}"),
                ),
            },
        }
        println!("{case}: {kind:?}, {}", describe(&outcome));
    }
    println!("{}", report.summary("samples"));
    report.settle(WAIVERS);
}

fn describe(outcome: &crate::support::Outcome) -> String {
    match outcome.refused() {
        Some(e) => format!("refused ({}): {} / {}", e.code.as_str(), e.what, e.why),
        None => "opened".to_owned(),
    }
}

/// Wraps a mutated stream back into a file.
type Wrap = fn(&[u8]) -> Vec<u8>;

/// One input of the mutation loop: a name, the file name it is opened
/// under, and its bytes.
struct Seed {
    case: String,
    file_name: &'static str,
    bytes: Vec<u8>,
    /// Wraps a mutated stream back into a file; `None` opens it as is.
    wrap: Option<Wrap>,
}

fn seeds(extension: &str) -> Vec<Seed> {
    let mut out = Vec::new();
    for path in sample_files() {
        let name = short_name(&path);
        let bytes = std::fs::read(&path).unwrap();
        let matches = |ext: &str| {
            path.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case(ext))
        };
        match extension {
            "xls" if matches("xls") => {
                // Damage to the BIFF stream reaches the records, not only
                // the container around them.
                if let Some(stream) = cfb::read_stream(&bytes, "Workbook") {
                    out.push(Seed {
                        case: format!("{name} (stream)"),
                        file_name: "book.xls",
                        bytes: stream,
                        wrap: Some(cfb::workbook),
                    });
                }
                out.push(Seed {
                    case: name,
                    file_name: "book.xls",
                    bytes,
                    wrap: None,
                });
            }
            "csv" if matches("csv") || matches("txt") => out.push(Seed {
                case: name,
                file_name: "data.csv",
                bytes,
                wrap: None,
            }),
            "xlsx" if matches("xlsx") || matches("parts") => {
                let bytes = if matches("parts") {
                    load_parts(&path)
                } else {
                    bytes
                };
                out.push(Seed {
                    case: name,
                    file_name: "book.xlsx",
                    bytes,
                    wrap: None,
                });
            }
            _ => {}
        }
    }
    match extension {
        "xls" => {
            let built: [(&str, Vec<u8>); 4] = [
                ("biff8 split SST (built)", biff::biff8_split_sst()),
                ("biff2 (built)", biff::biff2_sheet()),
                (
                    "biff5 cp936 (built)",
                    biff::biff5_book(936, b"\xB1\xED", b"\xD6\xD0\xCE\xC4"),
                ),
                ("biff4 workbook (built)", biff::biff4_workbook()),
            ];
            for (case, stream) in built {
                out.push(Seed {
                    case: case.to_owned(),
                    file_name: "book.xls",
                    bytes: stream,
                    wrap: Some(cfb::workbook),
                });
            }
        }
        "xlsb" => out.push(Seed {
            case: "xlsb (built)".to_owned(),
            file_name: "book.xlsb",
            bytes: xlsb::small_book(),
            wrap: None,
        }),
        _ => {}
    }
    out
}

/// Damages every seed of one format and opens each copy.
fn mutate_all(extension: &str, seed: u64, minimum: usize) {
    let seeds = seeds(extension);
    let mut rng = Rng(seed);
    let mut report = Report::default();
    for s in &seeds {
        for case in 0..MUTATIONS_PER_INPUT {
            let (damaged, what) = if extension == "csv" {
                mutate_csv(&mut rng, &s.bytes)
            } else {
                mutate_binary(&mut rng, &s.bytes, case)
            };
            let bytes = match s.wrap {
                Some(wrap) => wrap(&damaged),
                None => damaged,
            };
            report.run(
                format!("{} mutation {case} ({what})", s.case),
                Input::Bytes(s.file_name, bytes),
                Depth::Read,
            );
        }
    }
    println!("{}", report.summary(&format!("{extension} mutations")));
    assert!(
        report.cases.len() >= minimum,
        "{} cases",
        report.cases.len()
    );
    report.settle(WAIVERS);
}

#[test]
fn mutated_xls_never_panics_or_hangs() {
    mutate_all("xls", 0x00C0_FFEE_D00D_F00D, 500);
}

#[test]
fn mutated_csv_never_panics_or_hangs() {
    mutate_all("csv", 0x9E37_79B9_7F4A_7C15, 60);
}

#[test]
fn mutated_packages_never_panic_or_hang() {
    mutate_all("xlsx", 0x5EED_0010, 30);
    mutate_all("xlsb", 0x5EED_0011, 6);
}

/// The built samples open: if the test's compound file writer stopped
/// matching GridCraft's reader, the mutation loop would test nothing.
#[test]
fn built_samples_open() {
    let mut report = Report::default();
    for (case, name, bytes) in [
        (
            "biff8 split SST",
            "book.xls",
            cfb::workbook(&biff::biff8_split_sst()),
        ),
        ("xlsb", "book.xlsb", xlsb::small_book()),
    ] {
        let outcome = report.run(case.to_owned(), Input::Bytes(name, bytes), Depth::Save);
        assert!(
            matches!(outcome, Some(crate::support::Outcome::Opened)),
            "{case}: {outcome:?}"
        );
    }
    report.settle(&[]);
}
