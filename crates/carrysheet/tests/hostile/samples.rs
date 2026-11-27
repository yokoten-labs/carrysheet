//! Every file the removed readers were tested with, opened the way the
//! program opens a path, then damaged in seeded ways and opened as bytes.
//! Formats Carrysheet does not open (Numbers, BIFF2 to BIFF7) must be
//! refused with an error that names the reason.

use std::path::Path;

use carrysheet::commands::ErrorCode;

use crate::support::{
    Depth, EXPIRY, ISSUE, Input, Outcome, Report, Rng, Waiver, XML_BYTES, check_waivers,
    load_parts, mentions, mutate_binary, mutate_csv, mutate_text, pack, sample_files, short_name,
    unpack,
};
use crate::{biff, cfb, xlsb};

/// Known failures of this file's cases. Each needs a reason, an issue and
/// the expiry `v0.1.0`; a waiver that no case needs fails the test.
const WAIVERS: &[Waiver] = &[
    Waiver {
        case: "biff/testEXCEL_3.xls",
        problem: "not refused with its version",
        reason: "GridCraft reads a raw BIFF3 stream named .xls as csv text",
        issue: Some(ISSUE),
        expires: EXPIRY,
    },
    Waiver {
        case: "biff/testEXCEL_4.xls",
        problem: "not refused with its version",
        reason: "GridCraft reads a raw BIFF4 stream named .xls as csv text",
        issue: Some(ISSUE),
        expires: EXPIRY,
    },
];

/// How many samples are older than BIFF8: a miss of [`kind`] would let a
/// file skip the old-version check.
const OLD_EXCEL_SAMPLES: usize = 13;

/// Words of the error that refuses an old Excel file.
const OLD_VERSION: &[&str] = &["BIFF", "Excel 5", "version"];

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
    let mut old = 0;
    for path in &files {
        let name = short_name(path);
        let bytes = std::fs::read(path).unwrap();
        let kind = kind(path, &bytes);
        if matches!(kind, Kind::OldExcel(_)) {
            old += 1;
        }
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
            Kind::Opens => {
                if refused.is_some_and(|e| mentions(&e.why, OLD_VERSION)) {
                    report.fail(
                        &case,
                        format!("refused as an old version the test did not see: {outcome:?}"),
                    );
                }
            }
            Kind::Numbers => match refused {
                Some(e) if e.code == ErrorCode::UnsupportedFormat => {}
                _ => report.fail(
                    &case,
                    format!("a Numbers file was not refused: {outcome:?}"),
                ),
            },
            Kind::OldExcel(v) => match refused {
                Some(e) if mentions(&e.why, OLD_VERSION) => {}
                _ => report.fail(
                    &case,
                    format!("a BIFF{v} file was not refused with its version: {outcome:?}"),
                ),
            },
        }
        println!("{case}: {kind:?}, {}", describe(&outcome));
    }
    println!("{}", report.summary("samples"));
    println!("{old} samples older than BIFF8");
    assert_eq!(old, OLD_EXCEL_SAMPLES, "samples older than BIFF8");
    report.settle(WAIVERS);
}

fn describe(outcome: &Outcome) -> String {
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
    /// A package's parts: most copies damage one part and build the
    /// package again, so the CRC-32 matches and the damage reaches the
    /// xlsx or xlsb parser instead of stopping at the zip reader.
    parts: Option<Vec<(String, Vec<u8>)>>,
}

impl Seed {
    fn whole(case: String, file_name: &'static str, bytes: Vec<u8>) -> Seed {
        Seed {
            case,
            file_name,
            bytes,
            wrap: None,
            parts: None,
        }
    }

    /// A package: its parts when it is a zip (an encrypted one is not).
    fn package(case: String, file_name: &'static str, bytes: Vec<u8>) -> Seed {
        let parts = unpack(&bytes);
        Seed {
            parts,
            ..Seed::whole(case, file_name, bytes)
        }
    }
}

/// A small xlsx built in code: a number, a shared string, a formula and
/// an inline string.
fn built_xlsx() -> Vec<u8> {
    let mut book = crate::common::Book::new(
        r#"<row r="1"><c r="A1"><v>1.5</v></c><c r="B1" t="s"><v>0</v></c><c r="C1"><f>A1*2</f><v>3</v></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>inline</t></is></c></row>"#,
    );
    book.strings = Some(format!(
        r#"<?xml version="1.0"?><sst {}><si><t>shared</t></si></sst>"#,
        crate::common::NS
    ));
    book.bytes()
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
                        wrap: Some(cfb::workbook),
                        ..Seed::whole(format!("{name} (stream)"), "book.xls", stream)
                    });
                }
                out.push(Seed::whole(name, "book.xls", bytes));
            }
            "csv" if matches("csv") || matches("txt") => {
                out.push(Seed::whole(name, "data.csv", bytes));
            }
            "xlsx" if matches("xlsx") || matches("parts") => {
                let bytes = if matches("parts") {
                    load_parts(&path)
                } else {
                    bytes
                };
                out.push(Seed::package(name, "book.xlsx", bytes));
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
                    wrap: Some(cfb::workbook),
                    ..Seed::whole(case.to_owned(), "book.xls", stream)
                });
            }
        }
        "xlsx" => {
            // The kernel's own xlsx samples, and one built here.
            for name in ["WithTwoCharts.xlsx", "SimpleMacro.xlsm"] {
                let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/data")
                    .join(name);
                out.push(Seed::package(
                    name.to_owned(),
                    "book.xlsx",
                    std::fs::read(path).unwrap(),
                ));
            }
            out.push(Seed::package(
                "xlsx (built)".to_owned(),
                "book.xlsx",
                built_xlsx(),
            ));
        }
        "xlsb" => out.push(Seed::package(
            "xlsb (built)".to_owned(),
            "book.xlsb",
            xlsb::small_book(),
        )),
        _ => {}
    }
    out
}

/// Whether a mutation may pick this part: the binary records of an xlsb,
/// the XML of an xlsx.
fn target(extension: &str, part: &str) -> bool {
    if extension == "xlsb" {
        part.ends_with(".bin")
    } else {
        part.ends_with(".xml") || part.ends_with(".rels")
    }
}

/// Damages every seed of one format and opens each copy: `per_seed`
/// copies of each, and of a package, one in four is damaged as a whole
/// file and the rest in one part.
fn mutate_all(extension: &str, seed: u64, per_seed: usize, minimum: usize) -> Report {
    let seeds = seeds(extension);
    let mut rng = Rng(seed);
    let mut report = Report::default();
    let (mut in_parts, mut parts_opened) = (0, 0);
    let mut refusals = std::collections::BTreeMap::<String, usize>::new();
    for s in &seeds {
        let targets: Vec<usize> = s
            .parts
            .iter()
            .flatten()
            .enumerate()
            .filter(|(_, (name, _))| target(extension, name))
            .map(|(i, _)| i)
            .collect();
        let copies = if s.parts.is_some() {
            per_seed
        } else {
            MUTATIONS_PER_INPUT
        };
        for case in 0..copies {
            let parts = s.parts.as_ref().filter(|_| case % 4 != 0);
            if let (Some(parts), false) = (parts, targets.is_empty()) {
                let mut parts = parts.clone();
                let at = targets[rng.below(targets.len())];
                let (name, body) = &mut parts[at];
                let (damaged, what) = if extension == "xlsx" && case % 2 == 1 {
                    mutate_text(&mut rng, body, XML_BYTES)
                } else {
                    mutate_binary(&mut rng, body, case)
                };
                *body = damaged;
                let case = format!("{} mutation {case} ({name}: {what})", s.case);
                let outcome =
                    report.run(case, Input::Bytes(s.file_name, pack(&parts)), Depth::Read);
                in_parts += 1;
                match outcome {
                    Some(Outcome::Opened) => parts_opened += 1,
                    Some(Outcome::Refused(e)) => {
                        let why: String = e.why.chars().take(90).collect();
                        *refusals.entry(why).or_insert(0) += 1;
                    }
                    None => {}
                }
                continue;
            }
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
    if matches!(extension, "xlsx" | "xlsb") {
        println!("{extension}: {parts_opened} of {in_parts} copies damaged in one part opened");
        for (why, n) in &refusals {
            println!("  {n} refused: {why}");
        }
        // A copy damaged in one part reached the parser when it opened or
        // when the parser, not the zip reader, refused it.
        let parser = if extension == "xlsb" { "XLSB" } else { "XML" };
        let reached = parts_opened
            + refusals
                .iter()
                .filter(|(why, _)| why.contains(parser))
                .map(|(_, n)| n)
                .sum::<usize>();
        assert!(
            reached * 2 >= in_parts,
            "{extension}: {reached} of {in_parts} copies damaged in one part reached the parser"
        );
    }
    report
}

#[test]
fn mutated_xls_never_panics_or_hangs() {
    mutate_all("xls", 0x00C0_FFEE_D00D_F00D, MUTATIONS_PER_INPUT, 500).settle(WAIVERS);
}

#[test]
fn mutated_csv_never_panics_or_hangs() {
    mutate_all("csv", 0x9E37_79B9_7F4A_7C15, MUTATIONS_PER_INPUT, 60).settle(WAIVERS);
}

#[test]
fn mutated_xlsx_never_panics_or_hangs() {
    mutate_all("xlsx", 0x5EED_0010, 24, 100).settle(WAIVERS);
}

#[test]
fn mutated_xlsb_never_panics_or_hangs() {
    mutate_all("xlsb", 0x5EED_0011, 60, 60).settle(WAIVERS);
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
        ("xlsx", "book.xlsx", built_xlsx()),
    ] {
        let outcome = report.run(case.to_owned(), Input::Bytes(name, bytes), Depth::Save);
        assert!(
            matches!(outcome, Some(Outcome::Opened)),
            "{case}: {outcome:?}"
        );
    }
    report.settle(&[]);
}
