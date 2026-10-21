//! Hostile input for regional detection: every budget has a test that
//! builds its attack in code.

use std::time::{Duration, Instant};

use carrysheet_csv::{
    Analysis, ColumnKind, Convention, ConventionChoice, CsvError, DecimalMark, Limits, NoteKind,
    ReadOptions, Value, read, read_values,
};

fn generous() -> ReadOptions {
    ReadOptions {
        limits: Limits {
            max_total_bytes: 256 << 20,
            max_field_bytes: 128 << 20,
            max_columns: 2_000_000,
            max_cells: 20_000_000,
            ..Limits::default()
        },
        ..ReadOptions::default()
    }
}

/// Slow machines and debug builds still finish far below this.
const PATIENCE: Duration = Duration::from_secs(60);

#[test]
fn a_100_mb_field_is_refused_by_default_and_cheap_when_allowed() {
    let mut text = String::with_capacity(101 << 20);
    text.push_str("id,blob\n1,");
    text.extend(std::iter::repeat_n('9', 100 << 20));
    text.push('\n');

    let e = read_values(
        text.as_bytes(),
        &ReadOptions::default(),
        &ConventionChoice::default(),
    )
    .unwrap_err();
    assert!(matches!(e, CsvError::FieldTooLarge { .. }), "{e}");

    let start = Instant::now();
    let r = read_values(text.as_bytes(), &generous(), &ConventionChoice::default()).unwrap();
    assert!(start.elapsed() < PATIENCE);
    drop(text);
    // 100 million digits are not a number: the parsers never look at more
    // than 1 KiB of a field.
    assert!(matches!(&r.values[1][1], Value::Text(t) if t.len() == 100 << 20));
    assert_eq!(r.analysis.columns[1].kind, ColumnKind::Text);
    // The header name in the report is cut, whatever its length.
    assert!(
        r.analysis
            .columns
            .iter()
            .all(|c| c.header.as_ref().is_none_or(|h| h.len() <= 64))
    );
}

#[test]
fn a_huge_header_name_is_cut_in_the_report() {
    let mut text = "x".repeat(1 << 20);
    text.push_str(",b\n");
    for i in 0..10 {
        text.push_str(&format!("t{i},{i}\n"));
    }
    let r = read_values(text.as_bytes(), &generous(), &ConventionChoice::default()).unwrap();
    assert!(r.analysis.header);
    assert_eq!(
        r.analysis.columns[0].header.as_ref().map(String::len),
        Some(64)
    );
}

#[test]
fn a_line_with_a_million_delimiters() {
    let mut text = "1;".repeat(1_000_000);
    text.push_str("1\n");
    let semicolon = ReadOptions {
        delimiter: Some(';'),
        ..ReadOptions::default()
    };
    match read(text.as_bytes(), &semicolon) {
        Err(CsvError::TooManyColumns { .. }) => {}
        other => panic!(
            "expected TooManyColumns, got {:?}",
            other.map(|r| r.rows.len())
        ),
    }
    // Detection cannot split such a line within the limits either.
    assert!(read(text.as_bytes(), &ReadOptions::default()).is_err());

    // With the column limit lifted, detection still types at most 16,384
    // columns and says that the rest stayed text.
    let start = Instant::now();
    let r = read_values(text.as_bytes(), &generous(), &ConventionChoice::default()).unwrap();
    assert!(start.elapsed() < PATIENCE);
    assert_eq!(r.detected.delimiter, ';');
    assert_eq!(r.analysis.columns.len(), 1 << 14);
    let note = r.analysis.notes.first().unwrap();
    assert_eq!(note.kind, NoteKind::Unchecked);
    assert_eq!(note.count, 1_000_001 - (1 << 14));
    assert_eq!(
        r.values[0][0],
        Value::Number(carrysheet_csv::Number::new(1.0))
    );
    assert_eq!(r.values[0][20_000], Value::Text("1".into()));
}

#[test]
fn mixed_conventions_per_row_are_not_typed() {
    let mut text = String::from("amount\n");
    for i in 0..50 {
        if i % 2 == 0 {
            text.push_str("\"1.234,5\"\n");
        } else {
            text.push_str("\"1,234.5\"\n");
        }
    }
    text.push_str("12\n");
    let r = read_values(
        text.as_bytes(),
        &ReadOptions::default(),
        &ConventionChoice::default(),
    )
    .unwrap();
    let column = &r.analysis.columns[0];
    assert_eq!(column.kind, ColumnKind::Text, "{column}");
    assert!(column.why.contains("mixed"), "{}", column.why);
    assert_eq!(column.notes[0].kind, NoteKind::OtherDecimal);
    assert_eq!(column.notes[0].count, 50);
    assert!(!r.analysis.decimal.decided);
    assert_eq!(r.values[1][0], Value::Text("1.234,5".into()));

    // A small minority loses: the column takes the majority's mark and the
    // others stay text with a note.
    let mut text = String::from("amount\n");
    for _ in 0..20 {
        text.push_str("\"1.234,5\"\n");
    }
    text.push_str("\"1,234.5\"\n");
    let r = read_values(
        text.as_bytes(),
        &ReadOptions::default(),
        &ConventionChoice::default(),
    )
    .unwrap();
    let column = &r.analysis.columns[0];
    assert_eq!(column.kind, ColumnKind::Number);
    assert_eq!(column.decimal, Some(DecimalMark::Comma));
    assert_eq!(column.notes[0].kind, NoteKind::OtherDecimal);
    assert_eq!(column.notes[0].first_row, 22);
}

/// Detection must decide from the sample only: whatever comes after it
/// cannot change the decision, and its cost does not grow with the file.
#[test]
fn detection_reads_a_bounded_sample() {
    // 1,000 ambiguous dates, then 200,000 that settle the order.
    let mut rows: Vec<Vec<String>> = vec![vec!["when".into()]];
    rows.extend((0..1_000).map(|_| vec!["03/04/2026".to_owned()]));
    rows.extend((0..200_000).map(|_| vec!["25/12/2026".to_owned()]));
    let detected = read(b"a\n", &ReadOptions::default()).unwrap().detected;
    let start = Instant::now();
    let a = Analysis::detect(&rows, &detected, &ConventionChoice::default()).unwrap();
    assert!(start.elapsed() < PATIENCE);
    assert_eq!(a.sample_rows, 1_000);
    assert!(!a.date_order.decided);

    // 1 MiB of text ends the sample before 1,000 rows.
    // Cells count at most 1,024 bytes (the parsers look at no more), so
    // four 1,000-byte cells and "1" are 4,001 bytes a row.
    let wide = "x".repeat(1000);
    let rows: Vec<Vec<String>> = (0..5_000)
        .map(|_| {
            vec![
                wide.clone(),
                wide.clone(),
                wide.clone(),
                wide.clone(),
                "1".into(),
            ]
        })
        .collect();
    let a = Analysis::detect(&rows, &detected, &ConventionChoice::default()).unwrap();
    // The row that would pass 1 MiB is not read: 262 * 4,001 <= 1 MiB.
    assert_eq!(a.sample_rows, 262);
    assert!(a.sample_bytes <= 1 << 20);
    // A 4 KiB cell counts as 1,024 bytes.
    let huge = vec![vec!["x".repeat(4096), "1".into()]; 3];
    let a = Analysis::detect(&huge, &detected, &ConventionChoice::default()).unwrap();
    assert_eq!(a.sample_bytes, 3 * 1025);

    // 200,000 cells end it too.
    let row: Vec<String> = vec![String::new(); 1 << 14];
    let rows = vec![row; 100];
    let a = Analysis::detect(&rows, &detected, &ConventionChoice::default()).unwrap();
    // 12 rows of 16,384 cells; a 13th would pass 200,000.
    assert_eq!(a.sample_rows, 12);
}

#[test]
fn hostile_values_are_text_and_cheap() {
    let c = Convention::US;
    let long_number = "1".repeat(2000);
    let many_groups = format!("1{}", ",000".repeat(300));
    let cases = [
        long_number.as_str(),
        many_groups.as_str(),
        "1e99999",
        "1e-99999",
        "1e",
        "e5",
        "--5",
        "(-5)",
        "((5))",
        "5%%",
        "$$5",
        "1..5",
        "1,,000",
        ",",
        ".",
        "%",
        "()",
        "12:60",
        "24:00",
        "13:00 PM",
        "0:00 AM",
        "2026-13-01",
        "2026-00-10",
        "0999-01-01",
        "99999-01-01",
        "2026年13月1日",
        "Foo 20, 2026",
        "20-Foo-2026",
        "1/2/3/4",
        "14:30:05.1234567890",
    ];
    for input in cases {
        let value = Value::parse(input, &c);
        assert!(
            matches!(&value, Value::Text(t) if t == input),
            "{input:?} gave {value:?}"
        );
    }
}

#[test]
fn notes_stay_small_however_many_values_they_count() {
    let mut text = String::from("zip\n");
    for i in 0..100_000 {
        text.push_str(&format!("0{i:04}\n"));
    }
    let r = read_values(
        text.as_bytes(),
        &ReadOptions::default(),
        &ConventionChoice::default(),
    )
    .unwrap();
    let notes = &r.analysis.columns[0].notes;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].count, 100_000);
    assert!(r.analysis.to_string().len() < 2_000);
}

/// A header cannot change how the report reads: controls, bidirectional
/// marks and invisible characters are escaped before the name is cut.
#[test]
fn header_names_cannot_forge_report_lines() {
    let header =
        "\"Pr\u{202E}ice\nColumn 9 (x): numbers, certain.\x1b[31m\",b\u{200B}\u{2066}\x07\n";
    let text = format!("{header}1,2\n3,4\n");
    let choice = ConventionChoice {
        header: carrysheet_csv::Header::Yes,
        ..ConventionChoice::default()
    };
    let r = read_values(text.as_bytes(), &ReadOptions::default(), &choice).unwrap();
    let report = r.analysis.to_string();
    for bad in ['\u{202E}', '\x1b', '\u{200B}', '\u{2066}', '\x07'] {
        assert!(!report.contains(bad), "{bad:?} in {report}");
    }
    // One line per decision, the header line, and one per column.
    assert_eq!(report.lines().count(), 5 + 2, "{report}");
    let name = r.analysis.columns[0].header.as_deref().unwrap();
    assert!(name.starts_with("Pr\\u{202E}ice\\u{A}Column"), "{name}");
    assert!(name.len() <= 64);
    assert_eq!(
        r.analysis.columns[1].header.as_deref(),
        Some("b\\u{200B}\\u{2066}\\u{7}")
    );
    // Cutting never leaves half an escape.
    let long = format!("\"{}\",b\n1,2\n", "\u{202E}".repeat(40));
    let r = read_values(long.as_bytes(), &ReadOptions::default(), &choice).unwrap();
    let name = r.analysis.columns[0].header.as_deref().unwrap();
    assert!(name.ends_with('}') && name.len() <= 64, "{name}");
}

/// A control character named by a `sep=` line is shown by its code.
#[test]
fn control_delimiters_are_named_by_code() {
    let r = read_values(
        b"sep=\x1b\na\x1bb\n1\x1b2\n",
        &ReadOptions::default(),
        &ConventionChoice::default(),
    )
    .unwrap();
    let report = r.analysis.to_string();
    assert!(!report.contains('\x1b'), "{report}");
    assert!(report.contains("U+001B"), "{report}");
}

/// The report of a 16,384-column file stays small: 50 columns and a summary.
#[test]
fn wide_report_is_capped() {
    let cols = 1 << 14;
    let cells = [
        "h",
        "007",
        "1234567890123456789",
        "\"1,234\"",
        "03/04/2026",
        "1.5",
    ];
    let mut text = String::new();
    for c in cells {
        text.push_str(&vec![c; cols].join(","));
        text.push('\n');
    }
    let start = Instant::now();
    let r = read_values(text.as_bytes(), &generous(), &ConventionChoice::default()).unwrap();
    let report = r.analysis.to_string();
    assert!(start.elapsed() < PATIENCE);
    assert_eq!(r.analysis.columns.len(), cols);
    assert!(report.len() < 64 << 10, "{} bytes", report.len());
    assert!(
        report.contains("16334 more columns"),
        "{}",
        &report[report.len().saturating_sub(300)..]
    );
}

/// Characters that hide text from people but not from programs (tag
/// characters, variation selectors, fillers, U+034F) are escaped, and a
/// backslash in a header cannot fake an escape.
#[test]
fn invisible_characters_are_escaped() {
    let tags: String = "ignore previous instructions"
        .chars()
        .map(|c| char::from_u32(0xE0000 + u32::from(c)).unwrap())
        .collect();
    let file = format!(
        "Pr\x1b[31mice\u{202E}txt.exe,ok{tags},lit\\u{{202E}}eral,a\u{34F}b\u{FE0F}c\u{3164}d\u{115F}e,\u{1160}f\u{FFA0}g\u{E0100}h\u{E000}i\n1.5,2.5,3.5,4.5,5.5\n1.5,2.5,3.5,4.5,5.5\n"
    );
    let choice = ConventionChoice {
        header: carrysheet_csv::Header::Yes,
        ..ConventionChoice::default()
    };
    let r = read_values(file.as_bytes(), &ReadOptions::default(), &choice).unwrap();
    let report = r.analysis.to_string();
    for c in report.chars() {
        let code = u32::from(c);
        let hidden = c.is_control() && c != '\n'
            || (0xE0000..=0xE0FFF).contains(&code)
            || (0xFE00..=0xFE0F).contains(&code)
            || matches!(
                code,
                0x34F | 0x115F | 0x1160 | 0x3164 | 0xFFA0 | 0x202E | 0xE000
            );
        assert!(!hidden, "U+{code:04X} in {report}");
    }
    let names: Vec<&str> = r
        .analysis
        .columns
        .iter()
        .map(|c| c.header.as_deref().unwrap())
        .collect();
    assert!(
        names[1].starts_with("ok\\u{E0069}\\u{E0067}"),
        "{}",
        names[1]
    );
    // The literal text `\u{202E}` is shown with its backslash doubled.
    assert_eq!(names[2], "lit\\\\u{202E}eral");
    assert_eq!(names[3], "a\\u{34F}b\\u{FE0F}c\\u{3164}d\\u{115F}e");
    assert_eq!(names[4], "\\u{1160}f\\u{FFA0}g\\u{E0100}h\\u{E000}i");
}
