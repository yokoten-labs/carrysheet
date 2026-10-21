//! Writing: quoting, formula escaping, encodings, line endings, Cell.

use carrysheet_csv::{
    Cell, CsvError, EUC_KR, GB18030, LineEnding, ReadOptions, SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE,
    WINDOWS_1252, WriteOptions, read, write, write_to,
};

fn plain() -> WriteOptions {
    WriteOptions {
        bom: false,
        line_ending: LineEnding::Lf,
        ..WriteOptions::default()
    }
}

fn text(rows: &[&[&str]], options: &WriteOptions) -> String {
    String::from_utf8(write(rows, options).unwrap()).unwrap()
}

#[test]
fn defaults_are_excel_friendly() {
    let out = write(&[["a", "b"]], &WriteOptions::default()).unwrap();
    assert_eq!(out, b"\xEF\xBB\xBFa,b\r\n");
}

#[test]
fn quoting() {
    let rows: &[&[&str]] = &[
        &[
            "plain",
            "with,comma",
            "with \"quote\"",
            "two\nlines",
            "cr\ronly",
        ],
        &[""],
        &[],
        &["", ""],
    ];
    assert_eq!(
        text(rows, &plain()),
        "plain,\"with,comma\",\"with \"\"quote\"\"\",\"two\nlines\",\"cr\ronly\"\n\"\"\n\n,\n"
    );
}

#[test]
fn first_field_that_looks_like_bom_or_sep_is_quoted() {
    assert_eq!(text(&[&["\u{FEFF}x"]], &plain()), "\"\u{FEFF}x\"\n");
    assert_eq!(text(&[&["SEP=;"]], &plain()), "\"SEP=;\"\n");
    assert_eq!(text(&[&["a"], &["sep=;"]], &plain()), "a\nsep=;\n");
}

#[test]
fn formula_escaping() {
    let rows: &[&[&str]] = &[&["=1+2", "+cmd", "-2+3", "@SUM(A1)", "\t=x", "\rx", "a=b"]];
    assert_eq!(
        text(rows, &plain()),
        "\"'=1+2\",\"'+cmd\",\"'-2+3\",\"'@SUM(A1)\",\"'\t=x\",\"'\rx\",a=b\n"
    );
}

#[test]
fn plain_numbers_are_not_escaped() {
    let rows: &[&[&str]] = &[&["-12", "+3.5", "-1e5", "-.5", "-", "+", "-1-1"]];
    assert_eq!(
        text(rows, &plain()),
        "-12,+3.5,-1e5,-.5,\"'-\",\"'+\",\"'-1-1\"\n"
    );
}

#[test]
fn formula_escaping_can_be_turned_off() {
    let options = WriteOptions {
        escape_formulas: false,
        ..plain()
    };
    assert_eq!(text(&[&["=1+2", "@x"]], &options), "=1+2,@x\n");
}

#[test]
fn escaped_formula_reads_back_with_apostrophe() {
    let out = write(&[["=HYPERLINK(\"x\")"]], &plain()).unwrap();
    let back = read(&out, &ReadOptions::default()).unwrap();
    assert_eq!(back.rows, [["'=HYPERLINK(\"x\")"]]);
}

#[test]
fn formula_fixture_is_escaped_on_save() {
    let r = read(
        include_bytes!("data/shapes/formulas.csv"),
        &ReadOptions::default(),
    )
    .unwrap();
    let out = write(&r.rows, &WriteOptions::matching(&r.detected)).unwrap();
    assert_eq!(out, b"a,b\n\"'=1+2\",-5\n\"'@SUM(A1)\",\"'+cmd\"\n");
}

#[test]
fn line_endings_and_delimiters() {
    let rows: &[&[&str]] = &[&["a", "b"], &["c", "d"]];
    let crlf = WriteOptions {
        line_ending: LineEnding::CrLf,
        ..plain()
    };
    assert_eq!(text(rows, &crlf), "a,b\r\nc,d\r\n");
    let cr = WriteOptions {
        line_ending: LineEnding::Cr,
        ..plain()
    };
    assert_eq!(text(rows, &cr), "a,b\rc,d\r");
    let tab = WriteOptions {
        delimiter: '\t',
        ..plain()
    };
    assert_eq!(text(&[&["a", "b,c"]], &tab), "a\tb,c\n");
    let sep = WriteOptions {
        delimiter: ';',
        sep_line: true,
        ..plain()
    };
    assert_eq!(text(rows, &sep), "sep=;\na;b\nc;d\n");
}

#[test]
fn utf16_output() {
    let le = WriteOptions {
        encoding: UTF_16LE,
        bom: true,
        ..plain()
    };
    assert_eq!(write(&[["a"]], &le).unwrap(), b"\xFF\xFEa\x00\n\x00");
    let be = WriteOptions {
        encoding: UTF_16BE,
        bom: true,
        ..plain()
    };
    assert_eq!(write(&[["a"]], &be).unwrap(), b"\xFE\xFF\x00a\x00\n");
}

#[test]
fn legacy_output() {
    let sjis = WriteOptions {
        encoding: SHIFT_JIS,
        ..plain()
    };
    let out = write(&[["東京", "x"]], &sjis).unwrap();
    assert_eq!(out, b"\x93\x8c\x8b\x9e,x\n");
    let r = read(
        &out,
        &ReadOptions {
            encoding: Some(SHIFT_JIS),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.rows, [["東京", "x"]]);
}

#[test]
fn unencodable_character_names_its_cell() {
    let options = WriteOptions {
        encoding: WINDOWS_1252,
        ..plain()
    };
    let rows: &[&[&str]] = &[&["a", "b"], &["\"c\"", "ok", "中文"]];
    match write(rows, &options) {
        Err(CsvError::Unencodable {
            character,
            encoding,
            row,
            column,
        }) => {
            assert_eq!(
                (character, encoding, row, column),
                ('中', "windows-1252", 2, 3)
            );
        }
        other => panic!("expected Unencodable, got {other:?}"),
    }
    let korean = WriteOptions {
        encoding: EUC_KR,
        ..plain()
    };
    let err = write(&[["=x", "😀"]], &korean).unwrap_err();
    assert!(
        matches!(
            err,
            CsvError::Unencodable {
                row: 1,
                column: 2,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(err.to_string().contains("UTF-8"));
}

#[test]
fn gb18030_stores_everything() {
    let options = WriteOptions {
        encoding: GB18030,
        ..plain()
    };
    let out = write(&[["😀", "𠀀", "é"]], &options).unwrap();
    let r = read(
        &out,
        &ReadOptions {
            encoding: Some(GB18030),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r.rows, [["😀", "𠀀", "é"]]);
}

#[test]
fn bad_options() {
    let bom = WriteOptions {
        encoding: SHIFT_JIS,
        bom: true,
        ..plain()
    };
    assert!(matches!(
        write(&[["a"]], &bom),
        Err(CsvError::BomNotAvailable { .. })
    ));
    let replacement = carrysheet_csv::encoding_for_label("iso-2022-kr").unwrap();
    let options = WriteOptions {
        encoding: replacement,
        ..plain()
    };
    assert!(matches!(
        write(&[["a"]], &options),
        Err(CsvError::CannotWriteEncoding { .. })
    ));
    let quote = WriteOptions {
        delimiter: '"',
        ..plain()
    };
    assert!(matches!(
        write(&[["a"]], &quote),
        Err(CsvError::InvalidDelimiter { .. })
    ));
}

#[test]
fn write_to_a_stream() {
    let mut out = Vec::new();
    write_to(&mut out, &[vec!["a".to_owned()]], &plain()).unwrap();
    assert_eq!(out, b"a\n");
}

#[test]
fn matching_keeps_the_file_format() {
    let r = read(
        include_bytes!("data/unicode/utf16le-bom-tab.txt"),
        &ReadOptions::default(),
    )
    .unwrap();
    let out = write(&r.rows, &WriteOptions::matching(&r.detected)).unwrap();
    assert_eq!(out, include_bytes!("data/unicode/utf16le-bom-tab.txt"));

    let r = read(
        include_bytes!("data/legacy/euc-kr.csv"),
        &ReadOptions::default(),
    )
    .unwrap();
    let out = write(&r.rows, &WriteOptions::matching(&r.detected)).unwrap();
    assert_eq!(out, include_bytes!("data/legacy/euc-kr.csv"));

    let r = read(
        include_bytes!("data/shapes/pipe-sep-line.csv"),
        &ReadOptions::default(),
    )
    .unwrap();
    let out = write(&r.rows, &WriteOptions::matching(&r.detected)).unwrap();
    assert_eq!(out, include_bytes!("data/shapes/pipe-sep-line.csv"));
    assert_eq!(WriteOptions::matching(&r.detected).encoding, UTF_8);
}

#[test]
fn cell_inference() {
    assert_eq!(Cell::infer(""), Cell::Empty);
    assert_eq!(Cell::infer("TRUE"), Cell::Bool(true));
    assert_eq!(Cell::infer("false"), Cell::Bool(false));
    assert_eq!(Cell::infer("-12"), Cell::Number(-12.0));
    assert_eq!(Cell::infer("3.5"), Cell::Number(3.5));
    assert_eq!(Cell::infer(".5"), Cell::Number(0.5));
    assert_eq!(Cell::infer("0.5"), Cell::Number(0.5));
    assert_eq!(Cell::infer("1e3"), Cell::Number(1000.0));
    assert_eq!(Cell::infer("0"), Cell::Number(0.0));
    for text in [
        "007",
        "+5",
        " 5",
        "1,000",
        "inf",
        "NaN",
        "1e999",
        "4111111111111111",
        "1e",
        ".",
    ] {
        assert_eq!(Cell::infer(text), Cell::Text(text.to_owned()), "{text}");
    }
    assert_eq!(Cell::Number(1.5).to_string(), "1.5");
    assert_eq!(Cell::Bool(true).to_string(), "TRUE");
    assert_eq!(Cell::Empty.to_string(), "");
}
