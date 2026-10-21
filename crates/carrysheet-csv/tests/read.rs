//! Reading: encodings, delimiters, quoting, limits and errors.

use carrysheet_csv::{
    BIG5, Confidence, CsvError, EUC_KR, GB18030, GBK, Limits, LineEnding, Malformed, ReadOptions,
    SHIFT_JIS, UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252, detect_delimiter, detect_encoding,
    encoding_for_label, read, read_from,
};

fn auto(bytes: &[u8]) -> carrysheet_csv::ReadResult {
    match read(bytes, &ReadOptions::default()) {
        Ok(r) => r,
        Err(e) => panic!("read failed: {e}"),
    }
}

fn rows(lines: &[&[&str]]) -> Vec<Vec<String>> {
    lines
        .iter()
        .map(|r| r.iter().map(|s| (*s).to_owned()).collect())
        .collect()
}

#[test]
fn utf8_without_bom() {
    let r = auto(include_bytes!("data/unicode/utf8.csv"));
    assert_eq!(r.rows, rows(&[&["name", "city"], &["Ada", "Zürich"]]));
    assert_eq!(r.detected.encoding, UTF_8);
    assert_eq!(r.detected.encoding_confidence, Confidence::High);
    assert!(!r.detected.bom);
    assert_eq!(r.detected.delimiter, ',');
    assert_eq!(r.detected.line_ending, Some(LineEnding::Lf));
}

#[test]
fn utf8_with_bom_and_semicolons() {
    let r = auto(include_bytes!("data/unicode/utf8-bom-semicolon.csv"));
    assert_eq!(r.rows, rows(&[&["name", "city"], &["Ada", "Zürich"]]));
    assert_eq!(r.detected.encoding, UTF_8);
    assert_eq!(r.detected.encoding_confidence, Confidence::Certain);
    assert!(r.detected.bom);
    assert_eq!(r.detected.delimiter, ';');
    assert!(r.detected.delimiter_detected);
    assert_eq!(r.detected.line_ending, Some(LineEnding::CrLf));
}

#[test]
fn utf16_with_bom() {
    let le = auto(include_bytes!("data/unicode/utf16le-bom-tab.txt"));
    assert_eq!(le.rows, rows(&[&["name", "city"], &["Ada", "Zürich"]]));
    assert_eq!(le.detected.encoding, UTF_16LE);
    assert_eq!(le.detected.delimiter, '\t');
    assert!(le.detected.bom);

    let be = auto(include_bytes!("data/unicode/utf16be-bom.csv"));
    assert_eq!(be.rows, rows(&[&["name", "city"], &["Ada", "Zürich"]]));
    assert_eq!(be.detected.encoding, UTF_16BE);
}

#[test]
fn utf16_without_bom_is_a_low_guess() {
    let bytes: Vec<u8> = "a,b\r\n1,2\r\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let guess = detect_encoding(&bytes);
    assert_eq!(guess.encoding, UTF_16LE);
    assert_eq!(guess.confidence, Confidence::Low);
    assert_eq!(auto(&bytes).rows, rows(&[&["a", "b"], &["1", "2"]]));
}

#[test]
fn legacy_encodings_are_detected() {
    let cases: [(&[u8], &str, &[&'static carrysheet_csv::Encoding]); 6] = [
        (
            include_bytes!("data/legacy/gbk.csv"),
            "张伟",
            &[GBK, GB18030],
        ),
        (
            include_bytes!("data/legacy/gb18030.csv"),
            "𠀀",
            &[GBK, GB18030],
        ),
        (include_bytes!("data/legacy/big5.csv"), "陳大文", &[BIG5]),
        (
            include_bytes!("data/legacy/shift_jis.csv"),
            "山田太郎",
            &[SHIFT_JIS],
        ),
        (
            include_bytes!("data/legacy/euc-kr.csv"),
            "김철수",
            &[EUC_KR],
        ),
        (
            include_bytes!("data/legacy/windows-1252.csv"),
            "René",
            &[WINDOWS_1252],
        ),
    ];
    for (bytes, first_cell, expected) in cases {
        let r = auto(bytes);
        assert!(
            expected.contains(&r.detected.encoding),
            "{first_cell}: detected {}",
            r.detected.encoding.name()
        );
        assert!(r.detected.encoding_detected);
        assert!(r.detected.encoding_confidence <= Confidence::Medium);
        assert_eq!(r.rows.len(), 3, "{first_cell}");
        assert!(r.rows.iter().all(|row| row.len() == 3), "{first_cell}");
        assert!(
            r.rows.iter().flatten().any(|f| f == first_cell),
            "{first_cell}"
        );
    }
}

#[test]
fn windows_1252_euro_sign() {
    let r = auto(include_bytes!("data/legacy/windows-1252.csv"));
    assert_eq!(r.rows[1], ["René", "€5", "café crème"]);
}

#[test]
fn encoding_override_wins() {
    let bytes = include_bytes!("data/legacy/shift_jis.csv");
    let options = ReadOptions {
        encoding: Some(SHIFT_JIS),
        ..ReadOptions::default()
    };
    let r = read(bytes, &options).unwrap();
    assert_eq!(r.rows[0], ["名前", "都市", "メモ"]);
    assert!(!r.detected.encoding_detected);
    assert_eq!(r.detected.encoding_confidence, Confidence::Certain);
}

#[test]
fn override_strips_only_its_own_bom() {
    let bytes = b"\xEF\xBB\xBFa,b\n";
    let utf8 = read(
        bytes,
        &ReadOptions {
            encoding: Some(UTF_8),
            ..ReadOptions::default()
        },
    );
    assert_eq!(utf8.unwrap().rows, rows(&[&["a", "b"]]));
    let latin = read(
        bytes,
        &ReadOptions {
            encoding: Some(WINDOWS_1252),
            ..ReadOptions::default()
        },
    );
    assert_eq!(latin.unwrap().rows, rows(&[&["ï»¿a", "b"]]));
}

#[test]
fn labels() {
    assert_eq!(encoding_for_label("GBK"), Some(GBK));
    assert_eq!(encoding_for_label(" shift_jis "), Some(SHIFT_JIS));
    assert_eq!(encoding_for_label("cp1252"), Some(WINDOWS_1252));
    assert_eq!(encoding_for_label("utf-16le"), Some(UTF_16LE));
    assert_eq!(encoding_for_label("klingon"), None);
}

#[test]
fn quoted_fields_and_no_final_newline() {
    let r = auto(include_bytes!("data/shapes/quoted.csv"));
    assert_eq!(
        r.rows,
        rows(&[
            &["id", "text"],
            &["1", "line one\r\nline two"],
            &["2", "say \"hi\", then go"],
            &["3", "last line without newline"],
        ])
    );
}

#[test]
fn sep_line_names_the_delimiter() {
    let r = auto(include_bytes!("data/shapes/pipe-sep-line.csv"));
    assert_eq!(r.rows, rows(&[&["a", "b", "c"], &["1", "2", "3"]]));
    assert_eq!(r.detected.delimiter, '|');
    assert!(r.detected.sep_line);
    assert!(!r.detected.delimiter_detected);
}

#[test]
fn line_endings_mixed_and_blank_lines() {
    let r = auto(b"a,b\r\n\nc,d\re,f\n\n");
    assert_eq!(
        r.rows,
        rows(&[&["a", "b"], &[], &["c", "d"], &["e", "f"], &[]])
    );
    assert_eq!(r.detected.line_ending, Some(LineEnding::CrLf));
}

#[test]
fn empty_and_trailing_fields() {
    let r = read(
        b"a,,\n,\n\"\"\n",
        &ReadOptions {
            delimiter: Some(','),
            ..Default::default()
        },
    );
    assert_eq!(r.unwrap().rows, rows(&[&["a", "", ""], &["", ""], &[""]]));
    assert!(auto(b"").rows.is_empty());
}

#[test]
fn quote_handling_is_lenient_outside_quoted_fields() {
    let options = ReadOptions {
        delimiter: Some(','),
        ..Default::default()
    };
    let r = read(b"a\"b,\"x\"y, \"z\"\n", &options).unwrap();
    assert_eq!(r.rows, rows(&[&["a\"b", "xy", " \"z\""]]));
}

#[test]
fn delimiter_detection() {
    assert_eq!(detect_delimiter("a,b,c\n1,2,3\n"), Some(','));
    assert_eq!(detect_delimiter("a;b\n1,5;2\n3;4,5\n"), Some(';'));
    assert_eq!(detect_delimiter("a\tb\n\"x,y,z\"\t2\n"), Some('\t'));
    assert_eq!(detect_delimiter("a|b|c\n1|2|3\n"), Some('|'));
    assert_eq!(detect_delimiter("one column\nonly\n"), None);
    // A file with one column reads with a comma.
    assert_eq!(auto(b"x\ny\n").detected.delimiter, ',');
}

#[test]
fn malformed_bytes_error_by_default() {
    let bytes = b"a,b\n\xFF\xFE\xFDc,d\n";
    let options = ReadOptions {
        encoding: Some(UTF_8),
        ..Default::default()
    };
    match read(bytes, &options) {
        Err(CsvError::MalformedText { encoding, offset }) => {
            assert_eq!(encoding, "UTF-8");
            assert_eq!(offset, 4);
        }
        other => panic!("expected MalformedText, got {other:?}"),
    }
}

#[test]
fn malformed_bytes_can_be_replaced() {
    let options = ReadOptions {
        encoding: Some(UTF_8),
        malformed: Malformed::Replace,
        ..Default::default()
    };
    let r = read(b"a,\xFFb\n", &options).unwrap();
    assert_eq!(r.rows, rows(&[&["a", "\u{FFFD}b"]]));
    assert_eq!(r.detected.replaced, 1);
}

#[test]
fn odd_utf16_length_is_malformed() {
    let options = ReadOptions {
        encoding: Some(UTF_16LE),
        ..Default::default()
    };
    assert!(matches!(
        read(b"\xFF\xFEa\x00b", &options),
        Err(CsvError::MalformedText { .. })
    ));
}

#[test]
fn unclosed_quote() {
    let err = read(b"a,b\n1,\"open\n2,3\n", &ReadOptions::default()).unwrap_err();
    assert!(
        matches!(err, CsvError::UnclosedQuote { row: 2, line: 2 }),
        "{err:?}"
    );
    let text = err.to_string();
    assert!(text.contains("row 2"), "{text}");
    assert!(text.contains("closing double quote"), "{text}");
}

fn small(limits: Limits) -> ReadOptions {
    ReadOptions {
        delimiter: Some(','),
        limits,
        ..Default::default()
    }
}

#[test]
fn limits() {
    let base = Limits::default();
    let too_big = read(
        b"abcdef",
        &small(Limits {
            max_total_bytes: 5,
            ..base
        }),
    );
    assert!(matches!(too_big, Err(CsvError::InputTooLarge { limit: 5 })));

    let rows_err = read(
        b"a\nb\nc\n",
        &small(Limits {
            max_rows: 2,
            ..base
        }),
    );
    assert!(
        matches!(rows_err, Err(CsvError::TooManyRows { limit: 2, line: 3 })),
        "{rows_err:?}"
    );

    let cols = read(
        b"a\nb,c,d\n",
        &small(Limits {
            max_columns: 2,
            ..base
        }),
    );
    assert!(
        matches!(
            cols,
            Err(CsvError::TooManyColumns {
                limit: 2,
                row: 2,
                ..
            })
        ),
        "{cols:?}"
    );

    let cells = read(
        b"a,b\nc,d\n",
        &small(Limits {
            max_cells: 3,
            ..base
        }),
    );
    assert!(
        matches!(
            cells,
            Err(CsvError::TooManyCells {
                limit: 3,
                row: 2,
                ..
            })
        ),
        "{cells:?}"
    );

    let field = read(
        b"a,bbbbb\n",
        &small(Limits {
            max_field_bytes: 4,
            ..base
        }),
    );
    assert!(
        matches!(
            field,
            Err(CsvError::FieldTooLarge {
                limit: 4,
                row: 1,
                column: 2,
                ..
            })
        ),
        "{field:?}"
    );

    let quoted = read(
        b"\"aaaaaaaaaaaaaaaaaaaa",
        &small(Limits {
            max_field_bytes: 4,
            ..base
        }),
    );
    assert!(
        matches!(quoted, Err(CsvError::FieldTooLarge { column: 1, .. })),
        "{quoted:?}"
    );
}

#[test]
fn read_from_stops_at_the_limit() {
    let options = small(Limits {
        max_total_bytes: 8,
        ..Limits::default()
    });
    let endless = std::io::repeat(b'a');
    assert!(matches!(
        read_from(endless, &options),
        Err(CsvError::InputTooLarge { limit: 8 })
    ));
    let r = read_from(&b"a,b\n"[..], &options).unwrap();
    assert_eq!(r.rows, rows(&[&["a", "b"]]));
}

#[test]
fn invalid_delimiter() {
    for d in ['"', '\n', '\r', '€'] {
        let err = read(
            b"a",
            &ReadOptions {
                delimiter: Some(d),
                ..Default::default()
            },
        );
        assert!(
            matches!(err, Err(CsvError::InvalidDelimiter { .. })),
            "{d:?}"
        );
    }
}

#[test]
fn errors_have_three_parts() {
    let err = CsvError::TooManyColumns {
        limit: 3,
        row: 7,
        line: 9,
    };
    assert!(err.what().contains("Row 7 (line 9)"));
    assert!(!err.why().is_empty());
    assert!(err.fix().contains("max_columns"));
    assert_eq!(
        err.to_string(),
        format!("{} {} {}", err.what(), err.why(), err.fix())
    );
}
