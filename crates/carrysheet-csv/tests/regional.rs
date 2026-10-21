//! Regional conventions: a table of values per convention, detection per
//! column with its reasons, and write-then-read round trips.

use carrysheet_csv::{
    Analysis, ColumnKind, Confidence, Convention, ConventionChoice, Currency, Date, DateOrder,
    DecimalMark, Grouping, Header, NegativeStyle, NoteKind, Number, NumberFormat, ReadOptions,
    Time, Value, WriteOptions, read_values, write_values,
};
use proptest::prelude::*;

const US: Convention = Convention::US;
const UK: Convention = Convention::UK;
const DE: Convention = Convention::GERMANY;
const FR: Convention = Convention::FRANCE;
const CH: Convention = Convention::SWITZERLAND;
const EA: Convention = Convention::EAST_ASIA;
const ISO: Convention = Convention::ISO;

/// What a table row expects.
#[derive(Debug)]
enum Want {
    Num(f64),
    Fmt(f64, NumberFormat),
    Text,
    Empty,
    Bool(bool),
    Day(u16, u8, u8),
    At(u16, u8, u8, u8, u8, u8, u32),
    Clock(u8, u8, u8, u32),
}
use Want::*;

fn fmt() -> NumberFormat {
    NumberFormat::default()
}

fn grouped(decimals: u8) -> NumberFormat {
    NumberFormat {
        decimals,
        grouped: true,
        ..fmt()
    }
}

fn money(symbol: &str, after: bool, space: bool, decimals: u8, grouped: bool) -> NumberFormat {
    NumberFormat {
        decimals,
        grouped,
        currency: Currency::new(symbol, after, space),
        ..fmt()
    }
}

fn negative(style: NegativeStyle, decimals: u8, grouped: bool) -> NumberFormat {
    NumberFormat {
        decimals,
        grouped,
        negative: style,
        ..fmt()
    }
}

fn date(y: u16, m: u8, d: u8) -> Date {
    Date::new(y, m, d).unwrap()
}

fn time(h: u8, m: u8, s: u8, ns: u32) -> Time {
    Time::new(h, m, s, ns).unwrap()
}

#[test]
fn value_table() {
    let cases: Vec<(&str, Convention, Want)> = vec![
        // United States: '.' decimal, ',' grouping, month first.
        ("1234.5", US, Num(1234.5)),
        ("1,234.5", US, Fmt(1234.5, grouped(1))),
        ("-1,234.50", US, Fmt(-1234.5, grouped(2))),
        ("+42", US, Num(42.0)),
        (
            "42-",
            US,
            Fmt(-42.0, negative(NegativeStyle::Trailing, 0, false)),
        ),
        (
            "(1,234.50)",
            US,
            Fmt(-1234.5, negative(NegativeStyle::Parentheses, 2, true)),
        ),
        (
            "12.5%",
            US,
            Fmt(
                0.125,
                NumberFormat {
                    decimals: 1,
                    percent: true,
                    ..fmt()
                },
            ),
        ),
        (
            "$1,234.56",
            US,
            Fmt(1234.56, money("$", false, false, 2, true)),
        ),
        ("-$5", US, Fmt(-5.0, money("$", false, false, 0, false))),
        ("US$ 10", US, Fmt(10.0, money("US$", false, true, 0, false))),
        (
            "1.5E+10",
            US,
            Fmt(
                1.5e10,
                NumberFormat {
                    decimals: 1,
                    scientific: true,
                    ..fmt()
                },
            ),
        ),
        ("1e-3", US, Num(0.001)),
        (".5", US, Num(0.5)),
        ("1,23,456.78", US, Fmt(123_456.78, grouped(2))),
        ("007", US, Text),
        ("02134", US, Text),
        ("1234567890123456", US, Text),
        ("4111111111111111", US, Text),
        ("123456789012345", US, Num(123_456_789_012_345.0)),
        // 16 or 17 digits only when they are an f64's own shortest form.
        ("0.1234567890123456", US, Num(0.123_456_789_012_345_6)),
        ("0.30000000000000004", US, Num(0.1 + 0.2)),
        ("0.12345678901234567890", US, Text),
        ("0.30000000000000001", US, Text),
        ("1e-400", US, Text),
        ("1e400", US, Text),
        // A plus sign before eight or more digits is a phone number in
        // every convention, whatever the grouping.
        ("+4915112345678", US, Text),
        ("+12 345 678", FR, Text),
        ("+48 612 345 678", FR, Text),
        ("+48,612,345,678", US, Text),
        ("+48.612.345.678", DE, Text),
        (
            "+1 234 567",
            FR,
            Fmt(
                1_234_567.0,
                NumberFormat {
                    plus: true,
                    grouped: true,
                    ..fmt()
                },
            ),
        ),
        ("012 345 678", FR, Text),
        ("+49 151 12345678", US, Text),
        ("640 480", US, Text),
        ("640 480", DE, Text),
        ("+49 151 123 451", DE, Text),
        (
            "5e-324",
            US,
            Fmt(
                5e-324,
                NumberFormat {
                    scientific: true,
                    ..fmt()
                },
            ),
        ),
        ("1.2345e-320", US, Text),
        ("1 234,5", FR, Fmt(1234.5, grouped(1))),
        ("1 234", FR, Fmt(1234.0, grouped(0))),
        (
            "-EUR12",
            DE,
            Fmt(-12.0, money("EUR", false, false, 0, false)),
        ),
        (
            "EUR-12",
            DE,
            Fmt(-12.0, money("EUR", false, false, 0, false)),
        ),
        ("US$-5", US, Fmt(-5.0, money("US$", false, false, 0, false))),
        ("100000000000000000000", US, Num(1e20)),
        ("10/20/2026", US, Day(2026, 10, 20)),
        ("10/20/26", US, Day(2026, 10, 20)),
        ("1/2/30", US, Day(1930, 1, 2)),
        ("2026-10-20", US, Day(2026, 10, 20)),
        ("2026-10-20T14:30:05", US, At(2026, 10, 20, 14, 30, 5, 0)),
        ("2026-10-20 14:30", US, At(2026, 10, 20, 14, 30, 0, 0)),
        ("14:30", US, Clock(14, 30, 0, 0)),
        ("2:30 PM", US, Clock(14, 30, 0, 0)),
        ("12:00 AM", US, Clock(0, 0, 0, 0)),
        ("14:30:05.25", US, Clock(14, 30, 5, 250_000_000)),
        ("Oct 20, 2026", US, Day(2026, 10, 20)),
        ("20-Oct-2026", US, Day(2026, 10, 20)),
        ("3/4", US, Text),
        ("1-2", US, Text),
        ("SEPT2", US, Text),
        ("MARCH1", US, Text),
        ("TRUE", US, Bool(true)),
        ("false", US, Bool(false)),
        ("1,2", US, Text),
        ("abc", US, Text),
        ("", US, Empty),
        ("  ", US, Text),
        ("inf", US, Text),
        ("NaN", US, Text),
        ("1e400", US, Text),
        ("2026-02-29", US, Text),
        ("2028-02-29", US, Day(2028, 2, 29)),
        ("13/13/2026", US, Text),
        ("1 234", US, Text),
        ("2026-10-20T14:30:00Z", US, Text),
        // Full-width digits and signs (Japanese, Chinese input methods).
        ("２０２６年１０月２０日", EA, Day(2026, 10, 20)),
        ("１２３４．５", EA, Num(1234.5)),
        ("－５", EA, Num(-5.0)),
        ("１，２３４", EA, Fmt(1234.0, grouped(0))),
        (
            "（１２）",
            EA,
            Fmt(-12.0, negative(NegativeStyle::Parentheses, 0, false)),
        ),
        // Germany: ',' decimal, '.' grouping, day first with dots.
        ("1.234,5", DE, Fmt(1234.5, grouped(1))),
        ("1234,5", DE, Num(1234.5)),
        (
            "-1.234,50 €",
            DE,
            Fmt(-1234.5, money("€", true, true, 2, true)),
        ),
        (
            "1.234,50-",
            DE,
            Fmt(-1234.5, negative(NegativeStyle::Trailing, 2, true)),
        ),
        ("20.10.2026", DE, Day(2026, 10, 20)),
        ("20.10.26", DE, Day(2026, 10, 20)),
        ("20.10.2026 14:30", DE, At(2026, 10, 20, 14, 30, 0, 0)),
        ("1,234.5", DE, Text),
        (
            "12,5 %",
            DE,
            Fmt(
                0.125,
                NumberFormat {
                    decimals: 1,
                    percent: true,
                    ..fmt()
                },
            ),
        ),
        ("1.234.567", DE, Num(1_234_567.0)),
        ("0,5", DE, Num(0.5)),
        (
            "EUR 12,00",
            DE,
            Fmt(12.0, money("EUR", false, true, 2, false)),
        ),
        // France: ',' decimal, narrow no-break space grouping; any space works.
        ("1\u{202F}234,5", FR, Fmt(1234.5, grouped(1))),
        ("1\u{A0}234,5", FR, Num(1234.5)),
        ("1 234,5 €", FR, Num(1234.5)),
        ("20/10/2026", FR, Day(2026, 10, 20)),
        // Switzerland: '.' decimal, apostrophe grouping.
        ("1'234.50", CH, Fmt(1234.5, grouped(2))),
        (
            "CHF 1'234.50",
            CH,
            Fmt(1234.5, money("CHF", false, true, 2, true)),
        ),
        ("1\u{2019}234.5", CH, Num(1234.5)),
        // East Asia: year first.
        ("2026/10/20", EA, Day(2026, 10, 20)),
        ("2026年10月20日", EA, Day(2026, 10, 20)),
        ("2026년 10월 20일", EA, Day(2026, 10, 20)),
        ("¥1,000", EA, Fmt(1000.0, money("¥", false, false, 0, true))),
        (
            "1,000円",
            EA,
            Fmt(1000.0, money("円", true, false, 0, true)),
        ),
        ("₩1,000", EA, Fmt(1000.0, money("₩", false, false, 0, true))),
        (
            "₹1,23,456",
            EA,
            Fmt(123_456.0, money("₹", false, false, 0, true)),
        ),
        ("26/10/20", EA, Day(2026, 10, 20)),
        // United Kingdom: day first.
        ("20/10/2026", UK, Day(2026, 10, 20)),
        ("10/20/2026", UK, Text),
        ("05/05/2026", UK, Day(2026, 5, 5)),
        // ISO: no grouping accepted.
        ("1,234", ISO, Text),
        ("1234.5", ISO, Num(1234.5)),
        ("2026-10-20", ISO, Day(2026, 10, 20)),
    ];
    assert!(cases.len() >= 60, "the table has {} rows", cases.len());
    for (input, convention, want) in &cases {
        let got = Value::parse(input, convention);
        let ok = match (want, &got) {
            (Num(v), Value::Number(n)) => n.value == *v,
            (Fmt(v, f), Value::Number(n)) => n.value == *v && n.format == *f,
            (Text, Value::Text(t)) => t == input,
            (Empty, Value::Empty) => true,
            (Bool(b), Value::Bool(g)) => b == g,
            (Day(y, m, d), Value::Date(g)) => *g == date(*y, *m, *d),
            (At(y, mo, d, h, mi, s, ns), Value::DateTime(gd, gt)) => {
                *gd == date(*y, *mo, *d) && *gt == time(*h, *mi, *s, *ns)
            }
            (Clock(h, m, s, ns), Value::Time(t)) => *t == time(*h, *m, *s, *ns),
            _ => false,
        };
        assert!(
            ok,
            "{input:?} in {convention:?}: want {want:?}, got {got:?}"
        );
    }
}

fn values(text: &str) -> carrysheet_csv::ValuesResult {
    match read_values(
        text.as_bytes(),
        &ReadOptions::default(),
        &ConventionChoice::default(),
    ) {
        Ok(r) => r,
        Err(e) => panic!("read_values failed: {e}"),
    }
}

fn number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => n.value,
        other => panic!("not a number: {other:?}"),
    }
}

#[test]
fn german_file_is_detected() {
    let r = values(
        "Artikel;Preis;Datum;Menge\r\nTee;1.234,50;20.10.2026;3\r\nKaffee;3,99;31.12.2026;12\r\nZucker;0,89;01.02.2026;1.500\r\n",
    );
    let a = &r.analysis;
    assert!(a.header);
    assert_eq!(a.convention.delimiter, ';');
    assert_eq!(a.convention.decimal, DecimalMark::Comma);
    assert_eq!(a.decimal.confidence, Confidence::High);
    assert_eq!(a.convention.grouping, Grouping::Point);
    assert_eq!(a.convention.date_order, DateOrder::Dmy);
    assert_eq!(a.convention.date_separator, '.');
    let kinds: Vec<ColumnKind> = a.columns.iter().map(|c| c.kind).collect();
    assert_eq!(
        kinds,
        [
            ColumnKind::Text,
            ColumnKind::Number,
            ColumnKind::Date,
            ColumnKind::Number
        ]
    );
    assert_eq!(a.columns[1].decimal, Some(DecimalMark::Comma));
    assert_eq!(a.columns[1].header.as_deref(), Some("Preis"));
    assert_eq!(number(&r.values[1][1]), 1234.5);
    assert_eq!(r.values[3][2], Value::Date(date(2026, 2, 1)));
    // "1.500" could be 1500 or 1.5, and three rows of other columns are too
    // few to lend their decimal comma: it stays text, with a note.
    assert_eq!(r.values[3][3], Value::Text("1.500".into()));
    assert_eq!(a.columns[3].notes[0].kind, NoteKind::AmbiguousDecimal);
    assert_eq!(r.values[0][0], Value::Text("Artikel".into()));
    let report = a.to_string();
    assert!(report.contains("Decimal mark ','"), "{report}");
    assert!(report.contains("Column 2 (Preis): numbers"), "{report}");
}

#[test]
fn us_file_is_detected() {
    let r = values("item,price,when\nTea,\"1,234.50\",10/20/2026\nCoffee,3.99,12/31/2026\n");
    let a = &r.analysis;
    assert_eq!(a.convention.decimal, DecimalMark::Point);
    assert_eq!(a.convention.grouping, Grouping::Comma);
    assert_eq!(a.convention.date_order, DateOrder::Mdy);
    assert_eq!(a.columns[2].date_order, Some(DateOrder::Mdy));
    assert_eq!(r.values[2][2], Value::Date(date(2026, 12, 31)));
}

#[test]
fn ambiguous_dates_stay_text_and_say_so() {
    let r = values("when\n03/04/2026\n05/06/2026\n07/08/2026\n");
    let column = &r.analysis.columns[0];
    assert_eq!(column.kind, ColumnKind::Text);
    assert!(column.why.contains("unclear"), "{}", column.why);
    assert_eq!(column.notes[0].kind, NoteKind::AmbiguousDate);
    assert_eq!(column.notes[0].count, 3);
    assert_eq!(column.notes[0].first_row, 2);
    assert_eq!(r.values[1][0], Value::Text("03/04/2026".into()));
    assert!(!r.analysis.date_order.decided);
    assert!(
        r.analysis
            .to_string()
            .contains("03/04/2026 could be day first")
    );
}

#[test]
fn caller_settles_ambiguous_dates() {
    let choice = ConventionChoice {
        date_order: Some(DateOrder::Dmy),
        ..ConventionChoice::default()
    };
    let r = read_values(
        b"when\n03/04/2026\n05/06/2026\n",
        &ReadOptions::default(),
        &choice,
    )
    .unwrap();
    assert_eq!(r.values[1][0], Value::Date(date(2026, 4, 3)));
    assert_eq!(r.analysis.columns[0].confidence, Confidence::Certain);
    assert_eq!(r.analysis.date_order.confidence, Confidence::Certain);
}

#[test]
fn other_columns_settle_an_ambiguous_one() {
    // Ten dates in column a read only day first; column b's read both ways.
    let mut text = String::from("a,b\n");
    for day in 20..30 {
        text.push_str(&format!("{day}/12/2026,03/04/2026\n"));
    }
    let r = values(&text);
    let b = &r.analysis.columns[1];
    assert_eq!(b.kind, ColumnKind::Date);
    assert_eq!(b.date_order, Some(DateOrder::Dmy));
    assert_eq!(b.confidence, Confidence::Medium);
    assert_eq!(r.values[1][1], Value::Date(date(2026, 4, 3)));
    // Every value read that way is counted.
    assert_eq!(b.notes[0].kind, NoteKind::BorrowedDateOrder);
    assert_eq!(b.notes[0].count, 10);

    // Nine votes are not enough to lend.
    let text: String = text.lines().take(10).map(|l| format!("{l}\n")).collect();
    let r = values(&text);
    assert_eq!(r.analysis.columns[1].kind, ColumnKind::Text);
    assert_eq!(r.values[1][1], Value::Text("03/04/2026".into()));
}

#[test]
fn ambiguous_numbers_stay_text_and_say_so() {
    let r = values("amount\n\"1,234\"\n\"5,678\"\n\"9,012\"\n");
    let column = &r.analysis.columns[0];
    assert_eq!(column.kind, ColumnKind::Text);
    assert_eq!(column.notes[0].kind, NoteKind::AmbiguousDecimal);
    assert!(!r.analysis.decimal.decided);
    let choice = ConventionChoice {
        decimal: Some(DecimalMark::Point),
        ..ConventionChoice::default()
    };
    let r = read_values(
        b"amount\n\"1,234\"\n\"5,678\"\n",
        &ReadOptions::default(),
        &choice,
    )
    .unwrap();
    assert_eq!(number(&r.values[1][0]), 1234.0);
}

#[test]
fn values_that_must_stay_text() {
    let r = values(
        "zip,card,frac,gene,phone\n02134,4111111111111111,3/4,SEPT2,0612345678\n90210,5500000000000004,1-2,MARCH1,0791234567\n",
    );
    let a = &r.analysis;
    for column in &a.columns {
        assert_eq!(column.kind, ColumnKind::Text, "{column}");
    }
    let note = |c: usize| a.columns[c].notes[0].kind;
    assert_eq!(note(0), NoteKind::LeadingZeros);
    assert_eq!(note(1), NoteKind::TooManyDigits);
    assert_eq!(note(2), NoteKind::LooksLikeDate);
    assert_eq!(note(3), NoteKind::GeneName);
    assert_eq!(note(4), NoteKind::LeadingZeros);
    // 90210 has no leading zero but its column has: it stays text too.
    assert_eq!(r.values[2][0], Value::Text("90210".into()));
    let text = a.to_string();
    assert!(text.contains("ZIP codes"), "{text}");
    assert!(text.contains("gene names"), "{text}");
}

#[test]
fn two_digit_years_get_a_note() {
    let r = values("d\n31/12/29\n01/01/30\n25/12/99\n");
    let column = &r.analysis.columns[0];
    assert_eq!(column.kind, ColumnKind::Date);
    assert_eq!(r.values[1][0], Value::Date(date(2029, 12, 31)));
    assert_eq!(r.values[2][0], Value::Date(date(1930, 1, 1)));
    assert_eq!(
        column.notes[0].kind,
        NoteKind::TwoDigitYears { window_start: 1930 }
    );
    let choice = ConventionChoice {
        two_digit_pivot: 1950,
        ..ConventionChoice::default()
    };
    let r = read_values(
        b"d\n31/12/29\n01/01/30\n25/12/99\n",
        &ReadOptions::default(),
        &choice,
    )
    .unwrap();
    assert_eq!(r.values[1][0], Value::Date(date(2029, 12, 31)));
    assert_eq!(r.values[2][0], Value::Date(date(2030, 1, 1)));
}

#[test]
fn mixed_kinds_keep_the_odd_ones_as_text() {
    let r = values("n\n1\n2\nN/A\n4\n");
    let column = &r.analysis.columns[0];
    assert_eq!(column.kind, ColumnKind::Number);
    assert_eq!(r.values[3][0], Value::Text("N/A".into()));
    assert_eq!(
        column.notes[0].kind,
        NoteKind::OtherKind(ColumnKind::Number)
    );
    assert_eq!(column.notes[0].first_row, 4);
}

#[test]
fn header_choice() {
    let text = "1,2\n3,4\n";
    let r = values(text);
    assert!(!r.analysis.header);
    assert_eq!(number(&r.values[0][0]), 1.0);
    let yes = ConventionChoice {
        header: Header::Yes,
        ..ConventionChoice::default()
    };
    let r = read_values(text.as_bytes(), &ReadOptions::default(), &yes).unwrap();
    assert!(r.analysis.header);
    assert_eq!(r.values[0][0], Value::Text("1".into()));
}

#[test]
fn semicolon_wins_a_tie_with_comma() {
    let r = values("a;1,5\nb;2,25\n");
    assert_eq!(r.detected.delimiter, ';');
    assert_eq!(number(&r.values[0][1]), 1.5);
    assert_eq!(r.analysis.convention.decimal, DecimalMark::Comma);

    // Headerless decimal commas: `,` gives three cells a row, `;` two, but
    // the `;` cells are decimal-comma numbers and the `,` cells hold `;`.
    let r = values("1,5;2,5\n3,5;4,5\n");
    assert_eq!(r.detected.delimiter, ';');
    assert_eq!(number(&r.values[1][1]), 4.5);
    // The tie rule fired: the report says so and is less sure.
    assert!(r.detected.semicolon_for_decimal_commas);
    assert_eq!(r.analysis.delimiter.confidence, Confidence::Medium);
    assert!(r.analysis.delimiter.why.contains("decimal-comma"));

    // Without decimal commas a tie stays with `,`.
    let r = values("id,name;alias\n1,bob;b\n");
    assert_eq!(r.detected.delimiter, ',');
    assert!(!r.detected.semicolon_for_decimal_commas);
    assert_eq!(r.values[1][1], Value::Text("bob;b".into()));
}

#[test]
fn invalid_conventions_are_errors() {
    let bad = ConventionChoice {
        decimal: Some(DecimalMark::Comma),
        grouping: Some(Grouping::Comma),
        ..ConventionChoice::default()
    };
    let e = read_values(b"a\n", &ReadOptions::default(), &bad).unwrap_err();
    assert!(e.to_string().contains("same character"), "{e}");
    let c = Convention {
        date_separator: 'x',
        ..Convention::ISO
    };
    assert!(write_values(&[[Value::Empty]], &c, &WriteOptions::default()).is_err());
    let c = Convention {
        two_digit_pivot: 99,
        ..Convention::ISO
    };
    assert!(c.validate().is_err());
}

#[test]
fn export_in_regional_conventions() {
    let rows = vec![vec![
        Value::Text("=1+2".into()),
        Value::Number(Number {
            value: -1234.5,
            format: grouped(2),
        }),
        Value::Date(date(2026, 10, 20)),
        Value::Number(Number::new(1e20)),
        Value::Number(Number::new(1e-6)),
    ]];
    let options = WriteOptions {
        bom: false,
        ..WriteOptions::default()
    };
    let out =
        |c: &Convention| String::from_utf8(write_values(&rows, c, &options).unwrap()).unwrap();
    assert_eq!(
        out(&DE),
        "\"'=1+2\";-1.234,50;20.10.2026;100000000000000000000;0,000001\r\n"
    );
    assert_eq!(
        out(&US),
        "\"'=1+2\",\"-1,234.50\",10/20/2026,100000000000000000000,0.000001\r\n"
    );
    assert_eq!(
        out(&FR),
        "\"'=1+2\";-1\u{202F}234,50;20/10/2026;100000000000000000000;0,000001\r\n"
    );
    assert_eq!(
        out(&CH),
        "\"'=1+2\";-1'234.50;20.10.2026;100000000000000000000;0.000001\r\n"
    );
    assert_eq!(
        out(&ISO),
        "\"'=1+2\",-1234.50,2026-10-20,100000000000000000000,0.000001\r\n"
    );
}

#[test]
fn existing_encodings_work_with_conventions() {
    let rows = vec![vec![
        Value::Text("Größe".into()),
        Value::Number(Number {
            value: 1234.5,
            format: money("€", true, true, 2, true),
        }),
    ]];
    let options = WriteOptions {
        bom: false,
        encoding: carrysheet_csv::WINDOWS_1252,
        ..WriteOptions::for_convention(&DE)
    };
    let bytes = write_values(&rows, &DE, &options).unwrap();
    assert_eq!(bytes, b"Gr\xf6\xdfe;1.234,50 \x80\r\n");
    let back = read_values(
        &bytes,
        &ReadOptions {
            encoding: Some(carrysheet_csv::WINDOWS_1252),
            ..DE.read_options()
        },
        &ConventionChoice::from(DE),
    )
    .unwrap();
    assert_eq!(back.values, rows);
}

/// Rows that use every feature of a convention.
fn sample_rows() -> Vec<Vec<Value>> {
    let n = |value, format| Value::Number(Number { value, format });
    vec![
        vec![
            Value::Text("name".into()),
            Value::Text("amount".into()),
            Value::Text("day".into()),
            Value::Text("at".into()),
        ],
        vec![
            Value::Text("plain".into()),
            n(1234.5, grouped(2)),
            Value::Date(date(2026, 10, 20)),
            Value::DateTime(date(2026, 1, 2), time(3, 4, 5, 0)),
        ],
        vec![
            Value::Text("negative".into()),
            n(-0.25, negative(NegativeStyle::Parentheses, 2, false)),
            Value::Date(date(1999, 12, 31)),
            Value::DateTime(date(2026, 12, 25), time(23, 59, 59, 500_000_000)),
        ],
        vec![
            Value::Text("percent".into()),
            n(
                0.125,
                NumberFormat {
                    decimals: 1,
                    percent: true,
                    ..fmt()
                },
            ),
            Value::Date(date(2000, 2, 29)),
            Value::Empty,
        ],
        vec![
            Value::Text("money".into()),
            n(
                -1_000_000.0,
                NumberFormat {
                    currency: Currency::new("€", true, true),
                    negative: NegativeStyle::Trailing,
                    grouped: true,
                    ..fmt()
                },
            ),
            Value::Empty,
            Value::DateTime(date(2026, 6, 15), time(12, 0, 0, 0)),
        ],
        vec![
            Value::Text("big".into()),
            n(
                1.5e300,
                NumberFormat {
                    decimals: 1,
                    scientific: true,
                    ..fmt()
                },
            ),
            Value::Date(date(2026, 3, 4)),
            Value::Empty,
        ],
        vec![
            Value::Text("007".into()),
            n(7.0, fmt()),
            Value::Date(date(2026, 4, 3)),
            Value::Empty,
        ],
    ]
}

#[test]
fn round_trip_each_convention() {
    for convention in [US, UK, DE, FR, CH, EA, ISO] {
        let mut rows = sample_rows();
        if convention.grouping == Grouping::None {
            // Written without grouping, so read back without it.
            for value in rows.iter_mut().flatten() {
                if let Value::Number(n) = value {
                    n.format.grouped = false;
                }
            }
        }
        let options = WriteOptions::for_convention(&convention);
        let bytes = write_values(&rows, &convention, &options).unwrap();
        // Read with everything chosen...
        let back = read_values(&bytes, &ReadOptions::default(), &convention.into()).unwrap();
        assert_eq!(back.values, rows, "{convention:?} chosen");
        // ...and with everything detected.
        let found = read_values(
            &bytes,
            &ReadOptions::default(),
            &ConventionChoice::default(),
        )
        .unwrap();
        assert_eq!(
            found.values, rows,
            "{convention:?} detected:\n{}",
            found.analysis
        );
        assert_eq!(found.analysis.convention.delimiter, convention.delimiter);
        assert_eq!(found.analysis.convention.decimal, convention.decimal);
        // Write again: the same bytes.
        let again = write_values(&found.values, &convention, &options).unwrap();
        assert_eq!(again, bytes, "{convention:?}");
    }
}

fn convention() -> impl Strategy<Value = Convention> {
    prop::sample::select(vec![US, UK, DE, FR, CH, EA, ISO])
}

/// Numbers with at most 15 significant digits, as a spreadsheet keeps them.
fn spreadsheet_number() -> impl Strategy<Value = f64> {
    (-999_999_999_999_999i64..=999_999_999_999_999, -30i32..30)
        .prop_map(|(m, e)| format!("{m}e{e}").parse::<f64>().unwrap_or(0.0))
}

fn number_format() -> impl Strategy<Value = NumberFormat> {
    (
        0u8..4,
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        prop::sample::select(vec![None, Some("€"), Some("$"), Some("CHF"), Some("¥")]),
        any::<bool>(),
        any::<bool>(),
        prop::sample::select(vec![
            NegativeStyle::Minus,
            NegativeStyle::Parentheses,
            NegativeStyle::Trailing,
        ]),
        any::<bool>(),
    )
        .prop_map(
            |(decimals, grouped, percent, scientific, symbol, after, space, negative, plus)| {
                NumberFormat {
                    decimals,
                    plus,
                    grouped,
                    percent,
                    scientific,
                    currency: symbol.and_then(|s| Currency::new(s, after, space)),
                    negative,
                }
            },
        )
}

proptest! {
    #[test]
    fn numbers_round_trip(value in spreadsheet_number(), format in number_format(), convention in convention()) {
        let mut format = format;
        if convention.grouping == Grouping::None {
            format.grouped = false;
        }
        if format.percent || format.scientific {
            format.currency = None;
        }
        if format.scientific {
            format.grouped = false;
        }
        let rows = vec![vec![Value::Number(Number { value, format })]];
        let bytes = write_values(&rows, &convention, &WriteOptions::for_convention(&convention)).unwrap();
        let back = read_values(&bytes, &ReadOptions::default(), &convention.into()).unwrap();
        let Some(Value::Number(n)) = back.values.first().and_then(|r| r.first()) else {
            panic!("not a number: {:?} from {:?}", back.values, String::from_utf8_lossy(&bytes));
        };
        prop_assert_eq!(n.value, value, "{}", String::from_utf8_lossy(&bytes));
        prop_assert_eq!(n.format.percent, format.percent);
        // Written in scientific notation when it was, and when plain digits
        // would not read back.
        prop_assert!(n.format.scientific || !format.scientific);
        if value > 0.0 {
            prop_assert_eq!(n.format.plus, format.plus);
        }
        prop_assert_eq!(n.format.currency.map(|c| c.symbol()), format.currency.map(|c| c.symbol()));
        if value < 0.0 {
            prop_assert_eq!(n.format.negative, format.negative);
        }
    }

    #[test]
    fn parsing_never_panics(text in "\\PC{0,40}", convention in convention()) {
        let _ = Value::parse(&text, &convention);
    }

    #[test]
    fn detection_never_panics(rows in prop::collection::vec(prop::collection::vec("[0-9.,/ :%€$()-]{0,12}", 0..5), 0..12)) {
        let mut text = String::new();
        for row in &rows {
            text.push_str(&row.iter().map(|f| format!("\"{f}\"")).collect::<Vec<_>>().join(";"));
            text.push('\n');
        }
        // Typing never turns a readable file into an error.
        let plain = carrysheet_csv::read(text.as_bytes(), &ReadOptions::default());
        let typed = read_values(text.as_bytes(), &ReadOptions::default(), &ConventionChoice::default());
        prop_assert_eq!(plain.is_ok(), typed.is_ok());
    }

    /// Computed values (any finite f64, also subnormal) are written so that
    /// they read back exactly, in every convention.
    #[test]
    fn computed_values_round_trip(
        value in prop::num::f64::POSITIVE
            | prop::num::f64::NEGATIVE
            | prop::num::f64::NORMAL
            | prop::num::f64::SUBNORMAL
            | prop::num::f64::ZERO,
        convention in convention(),
    ) {
        let rows = vec![vec![Value::Number(Number::new(value))]];
        let bytes = write_values(&rows, &convention, &WriteOptions::for_convention(&convention)).unwrap();
        let back = read_values(&bytes, &ReadOptions::default(), &convention.into()).unwrap();
        let Some(Value::Number(n)) = back.values.first().and_then(|r| r.first()) else {
            panic!("not a number: {:?} from {:?}", back.values, String::from_utf8_lossy(&bytes));
        };
        prop_assert_eq!(n.value, value, "{}", String::from_utf8_lossy(&bytes));
    }
}

// Regressions from review round 2: each case below once gave a wrong value
// or a report that misled.

fn kinds(r: &carrysheet_csv::ValuesResult) -> Vec<ColumnKind> {
    r.analysis.columns.iter().map(|c| c.kind).collect()
}

fn has_note(r: &carrysheet_csv::ValuesResult, column: usize, kind: NoteKind) -> bool {
    r.analysis.columns[column]
        .notes
        .iter()
        .any(|n| n.kind == kind)
}

#[test]
fn a_text_column_does_not_lend_its_decimal_mark() {
    // "3,5" in a text column must not make "1,234" read as 1.234.
    let r = values(
        "Name,Amount,Tags\na,\"1,234\",x\nb,\"2,500\",\"3,5\"\nc,\"7,000\",y\nd,\"9,000\",z\n",
    );
    for row in &r.values[1..] {
        assert!(matches!(row[1], Value::Text(_)), "{row:?}");
    }
    let note = r.analysis.columns[1].notes[0];
    assert_eq!((note.kind, note.count), (NoteKind::AmbiguousDecimal, 4));
}

#[test]
fn few_votes_do_not_lend_a_decimal_mark() {
    // Three latitudes with a decimal point are too few to read 1.234 as
    // 1.234 in a quantity column.
    let r = values(
        "Ort;Breite;Menge\nBerlin;52.5200;1.234\nHamburg;53.5511;2.500\nKoeln;50.9375;12.000\n",
    );
    assert_eq!(number(&r.values[1][1]), 52.52);
    assert_eq!(r.values[1][2], Value::Text("1.234".into()));
    assert_eq!(r.values[3][2], Value::Text("12.000".into()));
    assert!(has_note(&r, 2, NoteKind::AmbiguousDecimal));
}

#[test]
fn free_text_does_not_decide_the_date_order() {
    let r = values(
        "When,Comment\n03/04/2026,ok\n05/06/2026,\"see 25/12/2026\"\n07/08/2026,25/12/2026\n",
    );
    assert_eq!(r.values[1][0], Value::Text("03/04/2026".into()));
    assert!(has_note(&r, 0, NoteKind::AmbiguousDate));
    assert!(!r.analysis.date_order.decided);
}

#[test]
fn borrowed_decimal_mark_is_noted_per_value() {
    let mut text = String::from("Price,Amount\n");
    for i in 0..10 {
        text.push_str(&format!("3.{i}5,\"1,234\"\n"));
    }
    let r = values(&text);
    assert_eq!(number(&r.values[1][1]), 1234.0);
    let column = &r.analysis.columns[1];
    assert_eq!(column.confidence, Confidence::Medium);
    let note = column.notes[0];
    assert_eq!(
        (note.kind, note.count, note.first_row),
        (NoteKind::BorrowedDecimal, 10, 2)
    );
    assert!(
        r.analysis
            .to_string()
            .contains("decimal mark of the other number columns")
    );
}

#[test]
fn a_column_that_disagrees_with_the_file_is_flagged() {
    // Two columns, two marks: neither is high confidence.
    let r = values("Item,Price,Pages\nA,3.25,\"1,5\"\nB,4.50,\"2,7\"\n");
    let pages = &r.analysis.columns[2];
    assert_eq!(pages.kind, ColumnKind::Number);
    assert!(pages.confidence < Confidence::High, "{pages}");
    assert!(has_note(
        &r,
        2,
        NoteKind::DecimalConflict(DecimalMark::Comma)
    ));

    // Three columns with '.', one with ',': the odd one is low confidence.
    let mut text = String::from("a,b,c,d\n");
    for i in 0..12 {
        text.push_str(&format!("1.{i}5,2.{i}5,3.{i}5,\"4,{i}5\"\n"));
    }
    let r = values(&text);
    assert_eq!(r.analysis.convention.decimal, DecimalMark::Point);
    let d = &r.analysis.columns[3];
    assert_eq!(d.confidence, Confidence::Low);
    assert_eq!(
        d.notes[0].kind,
        NoteKind::DecimalConflict(DecimalMark::Comma)
    );
    // `4,105` and `4,115` read both ways; ten read only with ','.
    assert_eq!(d.notes[0].count, 10);
    // Its values still read its own way.
    assert_eq!(number(&r.values[1][3]), 4.05);
}

#[test]
fn computed_values_read_and_write_exactly() {
    for value in [
        0.1 + 0.2,
        1e-300,
        1.0 / 3.0,
        2f64.powi(60),
        5e-324,
        f64::MAX,
    ] {
        let rows = vec![vec![Value::Number(Number::new(value))]];
        for c in [US, DE, FR, CH, ISO] {
            let bytes = write_values(&rows, &c, &WriteOptions::for_convention(&c)).unwrap();
            let back = read_values(&bytes, &ReadOptions::default(), &c.into()).unwrap();
            assert_eq!(
                number(&back.values[0][0]),
                value,
                "{}",
                String::from_utf8_lossy(&bytes)
            );
        }
    }
    // Plain digits would not read back as a number, so these are scientific.
    assert_eq!(Value::Number(Number::new(1e21)).to_text(&US), "1E+21");
    assert_eq!(Value::Number(Number::new(1e-7)).to_text(&DE), "1E-07");
    assert_eq!(
        Value::Number(Number::new(2f64.powi(60))).to_text(&ISO),
        "1.152921504606847E+18"
    );
    // 300 decimals do not overflow the format.
    let long = format!("0.{}1", "0".repeat(299));
    let Value::Number(n) = Value::parse(&long, &US) else {
        panic!()
    };
    assert_eq!(n.format.decimals, u8::MAX);
}

#[test]
fn currency_signs_never_read_as_formulas() {
    let euro = Number {
        value: -12.0,
        format: money("EUR", false, false, 0, false),
    };
    let dollar = Number {
        value: -5.0,
        format: money("US$", false, false, 0, false),
    };
    let rows = vec![vec![Value::Number(euro), Value::Number(dollar)]];
    let options = WriteOptions {
        bom: false,
        ..WriteOptions::for_convention(&DE)
    };
    let out = String::from_utf8(write_values(&rows, &DE, &options).unwrap()).unwrap();
    assert_eq!(out, "EUR-12;US$-5\r\n");
    let back = read_values(out.as_bytes(), &ReadOptions::default(), &DE.into()).unwrap();
    assert_eq!(number(&back.values[0][0]), -12.0);
    // `-EUR12` reads as the same number and is written back as `EUR-12`,
    // never in the formula shape (a minus and a cell reference).
    assert_eq!(Value::parse("-EUR12", &DE).to_text(&DE), "EUR-12");
    // An explicit plus is kept.
    let Value::Number(n) = Value::parse("+42", &US) else {
        panic!()
    };
    assert!(n.format.plus);
    assert_eq!(Value::Number(n).to_text(&US), "+42");
}

#[test]
fn phone_numbers_sizes_and_versions_stay_text() {
    let r = values(
        "Phone,Size,Version,Tiny\n+4915112345678,640 480,10.20.30,1e-400\n+4930123456,800 600,1.2.3,1e-400\n",
    );
    assert_eq!(kinds(&r), [ColumnKind::Text; 4]);
    assert_eq!(r.values[1][0], Value::Text("+4915112345678".into()));
    assert!(has_note(&r, 0, NoteKind::PhoneNumbers));
    assert_eq!(r.values[1][1], Value::Text("640 480".into()));
    assert_ne!(r.analysis.convention.grouping, Grouping::Space);
    assert_eq!(r.values[1][2], Value::Text("10.20.30".into()));
    assert_eq!(r.values[1][3], Value::Text("1e-400".into()));

    // Space-grouped whole numbers are numbers next to space-grouped
    // fractions.
    let r = values("Menge;Preis\n1 234;2 345,50\n5 678;1,25\n");
    assert_eq!(number(&r.values[1][0]), 1234.0);
    assert_eq!(r.analysis.convention.grouping, Grouping::Space);
}

/// A decimal comma alone does not make `640 480` a number: German files
/// group with `.`.
#[test]
fn sizes_in_a_decimal_comma_file_stay_text() {
    let r = values(
        "Artikel;Preis;Aufloesung\nA1;1,50;640 480\nA2;2,50;640 480\nA3;3,50;640 480\nA4;4,50;640 480\nA5;5,50;640 480\nA6;6,50;640 480\nB;3,99;800 600\nC;1,00;1024 768\n",
    );
    assert_eq!(r.analysis.convention.decimal, DecimalMark::Comma);
    assert_eq!(r.analysis.columns[2].kind, ColumnKind::Text);
    assert_eq!(r.values[1][2], Value::Text("640 480".into()));
    assert_eq!(r.values[8][2], Value::Text("1024 768".into()));
    assert_eq!(r.analysis.convention.grouping, Grouping::None);
}

/// Phone numbers with spaces between the digit groups are phone numbers.
#[test]
fn grouped_phone_numbers_stay_text() {
    let r = values(
        "Name;Preis;Telefon\nA1;1,50;+49 151 123 451\nA2;2,50;+49 151 123 452\nA3;3,50;+49 151 123 453\nA4;4,50;+49 151 123 454\nA5;5,50;+49 151 123 455\nB;2,00;+33 612 345 678\n",
    );
    assert_eq!(r.analysis.columns[2].kind, ColumnKind::Text);
    assert_eq!(r.values[1][2], Value::Text("+49 151 123 451".into()));
    let note = r.analysis.columns[2].notes[0];
    assert_eq!((note.kind, note.count), (NoteKind::PhoneNumbers, 6));
}

/// `1E5` reads as 100000, as in Excel, with a note that it may be a code.
#[test]
fn scientific_codes_are_numbers_with_a_note() {
    let r = values("code,qty\n1E5,1\n2E3,1\n12E4,1\n3E10,1\n7E1,1\n");
    assert_eq!(number(&r.values[1][0]), 100_000.0);
    let note = r.analysis.columns[0].notes[0];
    assert_eq!(
        (note.kind, note.count, note.first_row),
        (NoteKind::LikeCode, 5, 2)
    );
    assert!(r.analysis.to_string().contains("like 1E5"));
    // With a sign or a point it is plainly a number: no note.
    let r = values("x\n1.5E5\n2E-3\n3e2\n");
    assert_eq!(r.analysis.columns[0].notes.len(), 1);
    assert_eq!(r.analysis.columns[0].notes[0].count, 1);
}

/// Subnormal values keep fewer digits than they show.
#[test]
fn subnormals_that_round_stay_text() {
    let r = values("x\n1.2345e-320\n5e-324\n1\n");
    assert_eq!(r.values[1][0], Value::Text("1.2345e-320".into()));
    // Like leading zeros, one such value keeps the whole column text.
    assert_eq!(r.analysis.columns[0].kind, ColumnKind::Text);
    assert_eq!(r.analysis.columns[0].notes[0].kind, NoteKind::TooSmall);
    // 5e-324 reads back exactly, so on its own it is a number.
    assert_eq!(number(&Value::parse("5e-324", &US)), 5e-324);
}

#[test]
fn values_after_the_sample_settle_with_a_note() {
    // 600 neutral and 400 ambiguous dates fill the sample; row 1,002 is the
    // first that reads only day first.
    let mut text = String::from("when,amount\n");
    for i in 0..1_000 {
        if i % 5 < 3 {
            text.push_str("01/01/2026,5\n");
        } else {
            text.push_str("03/04/2026,\"1,234\"\n");
        }
    }
    text.push_str("25/12/2026,\"3,5\"\n");
    text.push_str("03/04/2026,\"1,234\"\n");
    let r = values(&text);
    let when = &r.analysis.columns[0];
    assert_eq!(when.date_order, Some(DateOrder::Dmy));
    assert_eq!(when.confidence, Confidence::Medium);
    assert!(when.why.contains("row 1002"), "{}", when.why);
    // Unclear dates in the sample stay text; those after the settling row
    // are read that way, with a note.
    assert_eq!(r.values[4][0], Value::Text("03/04/2026".into()));
    assert_eq!(r.values[1002][0], Value::Date(date(2026, 4, 3)));
    let late = when
        .notes
        .iter()
        .find(|n| n.kind == NoteKind::SettledLate)
        .unwrap();
    assert_eq!((late.count, late.first_row), (2, 1002));

    let amount = &r.analysis.columns[1];
    assert_eq!(amount.decimal, Some(DecimalMark::Comma));
    assert_eq!(number(&r.values[1002][1]), 1.234);
    assert!(amount.notes.iter().any(|n| n.kind == NoteKind::SettledLate));
}

#[test]
fn convert_takes_rows_detection_did_not_see() {
    let sample = vec![vec!["Preis"], vec!["1.234,5"], vec!["3,99"]];
    let detected = carrysheet_csv::read(b"a\n", &ReadOptions::default())
        .unwrap()
        .detected;
    let mut a = Analysis::detect(&sample, &detected, &ConventionChoice::default()).unwrap();
    let more = vec![vec!["2,5".to_owned()], vec!["7.000,25".to_owned()]];
    let values = a.convert_from(more, false);
    assert_eq!(number(&values[0][0]), 2.5);
    assert_eq!(number(&values[1][0]), 7000.25);
}

#[test]
fn date_separator_can_be_chosen() {
    let choice = ConventionChoice {
        date_separator: Some('-'),
        ..ConventionChoice::default()
    };
    let r = read_values(b"when\n25.12.2026\n", &ReadOptions::default(), &choice).unwrap();
    assert_eq!(r.analysis.convention.date_separator, '-');
    // Detected only from date columns: dots in numbers do not count.
    let r = values("when,n\n25/12/2026,1.5\n");
    assert_eq!(r.analysis.convention.date_separator, '/');
    let bad = ConventionChoice {
        date_separator: Some('x'),
        ..ConventionChoice::default()
    };
    assert!(read_values(b"a\n", &ReadOptions::default(), &bad).is_err());
}

#[test]
fn escaped_formulas_write_the_same_bytes_twice() {
    let rows = vec![vec!["=1+2", "+cmd", "-x", "@sum", "'=a", "plain"]];
    let options = WriteOptions::default();
    let first = carrysheet_csv::write(&rows, &options).unwrap();
    let back = carrysheet_csv::read(&first, &ReadOptions::default()).unwrap();
    let second = carrysheet_csv::write(&back.rows, &options).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&first),
        String::from_utf8_lossy(&second)
    );
}

// Regressions from review round 3.

fn values_with(text: &str, choice: ConventionChoice) -> carrysheet_csv::ValuesResult {
    read_values(text.as_bytes(), &ReadOptions::default(), &choice).unwrap()
}

/// French populations grouped with no-break spaces are numbers once the
/// decimal comma is decided; plain spaces need more evidence but say so.
#[test]
fn french_grouped_integers() {
    for space in ['\u{A0}', '\u{202F}'] {
        let text = "Ville;Population;Taux\nParis;2 102 650;12,5\nLyon;522 250;8,25\nMarseille;873 076;9,75\nLille;236 710;7,5\nNice;348 085;6,5\n"
            .replace(' ', &space.to_string());
        let r = values(&text);
        assert_eq!(
            r.analysis.columns[1].kind,
            ColumnKind::Number,
            "{}",
            r.analysis
        );
        assert_eq!(number(&r.values[1][1]), 2_102_650.0);
    }
    // Plain spaces: text, with a note and a headline that says why.
    let r = values(
        "Ville;Population;Taux\nParis;2 102 650;12,5\nLyon;522 250;8,25\nMarseille;873 076;9,75\nLille;236 710;7,5\nNice;348 085;6,5\n",
    );
    let population = &r.analysis.columns[1];
    assert_eq!(population.kind, ColumnKind::Text);
    assert_eq!(r.values[1][1], Value::Text("2 102 650".into()));
    assert!(
        population
            .why
            .contains("may be numbers grouped with spaces"),
        "{}",
        population.why
    );
    assert!(!population.why.contains("no value is a number"));
    let note = population.notes[0];
    assert_eq!((note.kind, note.count), (NoteKind::SpaceGrouped, 5));
    // Choosing the grouping reads them.
    let r = values_with(
        "Ville;Population;Taux\nParis;2 102 650;12,5\n",
        ConventionChoice {
            grouping: Some(Grouping::Space),
            ..ConventionChoice::default()
        },
    );
    assert_eq!(number(&r.values[1][1]), 2_102_650.0);
    // Space-grouped fractions in the file are evidence enough.
    let r = values(
        "Ville;Population;Budget\nParis;2 102 650;1 234,50\nLyon;522 250;99,00\nMarseille;873 076;5 000,25\n",
    );
    assert_eq!(number(&r.values[1][1]), 2_102_650.0);
    // Without a decimal comma: plain text, no note and no space hint.
    let r = values("Ville;Population\nParis;2 102 650\nLyon;522 250\nMarseille;873 076\n");
    assert_eq!(r.analysis.columns[1].kind, ColumnKind::Text);
    assert!(r.analysis.columns[1].notes.is_empty());
    assert!(!r.analysis.columns[1].why.contains("space grouping"));
}

/// Plus-signed output: up to seven digits, or a fraction, reads back as
/// the same number; a whole number of eight or more digits reads back as
/// text with the phone note. Text keeps every
/// character of a phone number, where a number would lose the plus sign
/// and the spacing, so the round trip gives way, not the phone rule.
#[test]
fn plus_signed_output_reads_back() {
    for c in [US, UK, DE, FR, CH, EA, ISO] {
        for value in [
            12_345_678.0,
            2_345_678.0,
            4_915_112_345_678.0,
            1.5,
            1234.5,
            1_234_567.25,
        ] {
            for grouped in [false, true] {
                let format = NumberFormat {
                    plus: true,
                    grouped,
                    ..fmt()
                };
                let rows = vec![vec![Value::Number(Number { value, format })]];
                let bytes = write_values(&rows, &c, &WriteOptions::for_convention(&c)).unwrap();
                let back = read_values(&bytes, &ReadOptions::default(), &c.into()).unwrap();
                let shown = String::from_utf8_lossy(&bytes).into_owned();
                let digits = Value::Number(Number { value, format })
                    .to_text(&c)
                    .bytes()
                    .filter(u8::is_ascii_digit)
                    .count();
                if digits >= 8 && value.fract() == 0.0 {
                    assert!(matches!(back.values[0][0], Value::Text(_)), "{shown}");
                    let note = back.analysis.columns[0].notes[0];
                    assert_eq!(note.kind, NoteKind::PhoneNumbers, "{shown}");
                } else {
                    assert_eq!(number(&back.values[0][0]), value, "{shown}");
                }
            }
        }
    }
}

/// Phone numbers in national formats stay text with a note, also next to
/// space-grouped amounts (Poland and Czechia group with spaces).
#[test]
fn national_phone_formats_stay_text() {
    let phones = [
        "+48 612 345 678",
        "+420 601 234 567",
        "+34 612 345 678",
        "+351 912 345 678",
        "+33 6 12 34 56 78",
        "+49 30 12345678",
        "+44 20 7946 0958",
        "+1 212-555-0123",
    ];
    for space in [" ", "\u{A0}", "\u{202F}"] {
        let mut text = String::from("Kwota;Telefon\n");
        for (i, phone) in phones.iter().enumerate() {
            text.push_str(&format!("1{space}234,{i}0;{}\n", phone.replace(' ', space)));
        }
        let r = values(&text);
        assert_eq!(
            r.analysis.columns[0].kind,
            ColumnKind::Number,
            "{}",
            r.analysis
        );
        let phone = &r.analysis.columns[1];
        assert_eq!(phone.kind, ColumnKind::Text, "{}", r.analysis);
        assert_eq!(
            (phone.notes[0].kind, phone.notes[0].count),
            (NoteKind::PhoneNumbers, 8)
        );
        for row in &r.values[1..] {
            assert!(matches!(row[1], Value::Text(_)), "{row:?}");
        }
    }
    // A column of changes with long plus values stays text too.
    let r = values(
        "Name;Montant;Budget\nA;+12 345 678;1 234,50\nB;-1 234 567;2,00\nC;+2 345 678;3,00\n",
    );
    assert_eq!(r.values[1][1], Value::Text("+12 345 678".into()));
}

/// No-break-space integers are numbers only in number columns where the
/// decimal comma applies, and the leading-zero rule comes first.
#[test]
fn nbsp_integers_only_where_the_comma_applies() {
    // A column that decided a point keeps them text.
    let r = values("a;b;c\n1,5;2\u{A0}102\u{A0}650;x\n2,5;3.25;y\n3,5;4.5;z\n4,5;5\u{A0}000;w\n");
    assert_eq!(r.values[1][1], Value::Text("2\u{A0}102\u{A0}650".into()));
    // Leading zeros win: SIREN-like `012 345 678`.
    let r = values(
        "Taux;SIREN\n1,5;012\u{A0}345\u{A0}678\n2,5;552\u{A0}100\u{A0}554\n3,5;123\u{A0}456\u{A0}789\n",
    );
    let siren = &r.analysis.columns[1];
    assert_eq!(siren.kind, ColumnKind::Text);
    assert_eq!(siren.notes[0].kind, NoteKind::LeadingZeros);
    // A text column stays text, with no number notes.
    let r = values("Taux;Nom\n1,5;Rue 2\u{A0}102\n2,5;Paris\n3,5;Lyon\n");
    assert!(
        r.analysis.columns[1].notes.is_empty(),
        "{}",
        r.analysis.columns[1]
    );
}

/// Without a decimal comma, `640 480` is plain text: no note, and no
/// headline that suggests space grouping.
#[test]
fn sizes_in_a_point_file_get_no_space_hint() {
    let r = values("Size;Price\n640 480;1.5\n800 600;2.5\n1024 768;3.5\n");
    let size = &r.analysis.columns[0];
    assert_eq!(size.kind, ColumnKind::Text);
    assert!(size.notes.is_empty());
    assert!(!size.why.contains("space grouping"), "{}", size.why);
}

/// No-break-space integers before a late decimal comma say why they are
/// text.
#[test]
fn values_before_a_late_comma_say_so() {
    let mut text = String::from("n;x\n");
    for i in 0..1_000 {
        text.push_str(&format!("{};a\n", i + 1));
    }
    text.push_str("2\u{A0}102\u{A0}650;a\n7,5;a\n3\u{A0}000;a\n");
    let r = values(&text);
    let n = &r.analysis.columns[0];
    assert_eq!(n.decimal, Some(DecimalMark::Comma), "{}", r.analysis);
    assert_eq!(r.values[1001][0], Value::Text("2\u{A0}102\u{A0}650".into()));
    assert_eq!(number(&r.values[1003][0]), 3000.0);
    let before = n
        .notes
        .iter()
        .find(|x| x.kind == NoteKind::BeforeSettled)
        .unwrap_or_else(|| panic!("{n}"));
    assert_eq!((before.count, before.first_row), (1, 1002));
    assert_eq!(
        before.message(),
        "1 value like 2 102 650 came before a value settled the decimal mark, so it stays text; first in row 1002."
    );
    assert!(
        !n.notes
            .iter()
            .any(|x| x.kind == NoteKind::OtherKind(ColumnKind::Number))
    );
}

#[test]
fn space_grouped_note_reads_well() {
    let r = values("Ville;Population;Taux\nParis;2 102 650;12,5\n");
    assert_eq!(
        r.analysis.columns[1].notes[0].message(),
        "1 value like 2 102 650 may be a number grouped with spaces and stays text; choose space grouping to read it; first in row 2."
    );
}

/// Text with the other grouping mark warns again: `1.234.567` with `.` as
/// the decimal mark.
#[test]
fn other_decimal_warns_for_grouped_values() {
    for (text, decimal) in [
        (
            "a;b\n1.5;1.234.567\n2.5;2.345.678\n3.5;3.456.789\n4.5;12\n",
            DecimalMark::Point,
        ),
        (
            "a;b\n1,5;1,234,567\n2,5;2,345,678\n3,5;3,456,789\n4,5;12\n",
            DecimalMark::Comma,
        ),
    ] {
        let r = values_with(
            text,
            ConventionChoice {
                decimal: Some(decimal),
                ..ConventionChoice::default()
            },
        );
        let b = &r.analysis.columns[1];
        assert_eq!(b.confidence, Confidence::Medium, "{b}");
        assert_eq!(
            (b.notes[0].kind, b.notes[0].count),
            (NoteKind::OtherDecimal, 3),
            "{b}"
        );
    }
}

/// A first data row over the sample budget is still read.
#[test]
fn a_fat_first_row_is_sampled() {
    let header: Vec<String> = (0..1100).map(|i| format!("h{i}")).collect();
    let fat: Vec<String> = (0..1100)
        .map(|i| format!("{i}.5{}", "0".repeat(990)))
        .collect();
    let thin: Vec<String> = (0..1100).map(|i| format!("{i}.25")).collect();
    let mut text = header.join(",") + "\n" + &fat.join(",") + "\n";
    for _ in 0..3 {
        text.push_str(&thin.join(","));
        text.push('\n');
    }
    let r = values(&text);
    assert!(r.analysis.header);
    assert_eq!(r.analysis.sample_rows, 1);
    assert_eq!(r.analysis.columns[0].kind, ColumnKind::Number);
}

#[test]
fn too_small_notes_read_well() {
    let r = values("x\n1.2345e-320\n");
    let note = r.analysis.columns[0].notes[0];
    assert_eq!(
        note.message(),
        "1 value is too small for a number to keep its digits; it stays text; first in row 2."
    );
    assert!(r.analysis.columns[0].why.contains("too small"));
}
