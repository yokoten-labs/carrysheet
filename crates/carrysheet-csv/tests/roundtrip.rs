//! Property tests: write then read gives the same rows, and reading never panics.

use carrysheet_csv::{
    BIG5, EUC_KR, Encoding, GB18030, GBK, LineEnding, ReadOptions, SHIFT_JIS, UTF_8, UTF_16BE,
    UTF_16LE, WINDOWS_1252, WriteOptions, read, write,
};
use proptest::prelude::*;

/// Characters that stress quoting, line endings, formulas, BOMs and `sep=`.
const TRICKY: &[char] = &[
    'a', 'Z', '0', '5', '.', ' ', ',', ';', '\t', '|', '"', '\'', '\r', '\n', '=', '+', '-', '@',
    's', 'e', 'p', 'é', '€', '中', '😀', '\u{FEFF}', '\u{0}',
];

fn field(alphabet: &'static [char]) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(alphabet), 0..8).prop_map(String::from_iter)
}

fn rows(alphabet: &'static [char]) -> impl Strategy<Value = Vec<Vec<String>>> {
    prop::collection::vec(prop::collection::vec(field(alphabet), 0..6), 0..8)
}

fn line_ending() -> impl Strategy<Value = LineEnding> {
    prop::sample::select(&[LineEnding::CrLf, LineEnding::Lf, LineEnding::Cr][..])
}

fn delimiter() -> impl Strategy<Value = char> {
    prop::sample::select(&[',', ';', '\t', '|'][..])
}

fn read_back(bytes: &[u8], encoding: &'static Encoding, delimiter: char) -> Vec<Vec<String>> {
    let options = ReadOptions {
        encoding: Some(encoding),
        delimiter: Some(delimiter),
        ..ReadOptions::default()
    };
    match read(bytes, &options) {
        Ok(r) => r.rows,
        Err(e) => panic!("read back failed: {e}"),
    }
}

proptest! {
    #[test]
    fn unicode_round_trip(
        rows in rows(TRICKY),
        delimiter in delimiter(),
        line_ending in line_ending(),
        encoding in prop::sample::select(vec![UTF_8, UTF_16LE, UTF_16BE, GB18030]),
        bom in any::<bool>(),
        sep_line in any::<bool>(),
    ) {
        let bom = bom && encoding != GB18030;
        let options = WriteOptions {
            delimiter, encoding, bom, line_ending, escape_formulas: false, sep_line,
        };
        let bytes = write(&rows, &options).unwrap();
        prop_assert_eq!(read_back(&bytes, encoding, delimiter), rows);
    }

    #[test]
    fn detection_round_trip_with_bom(
        rows in rows(TRICKY),
        line_ending in line_ending(),
        encoding in prop::sample::select(vec![UTF_8, UTF_16LE, UTF_16BE]),
        delimiter in delimiter(),
    ) {
        let options = WriteOptions {
            delimiter, encoding, bom: true, line_ending, escape_formulas: false, sep_line: true,
        };
        let bytes = write(&rows, &options).unwrap();
        let r = read(&bytes, &ReadOptions::default()).unwrap();
        prop_assert_eq!(r.detected.encoding, encoding);
        prop_assert_eq!(r.detected.delimiter, delimiter);
        prop_assert_eq!(r.rows, rows);
    }

    #[test]
    fn escaped_round_trip(rows in rows(TRICKY), delimiter in delimiter()) {
        let options = WriteOptions { delimiter, ..WriteOptions::default() };
        let bytes = write(&rows, &options).unwrap();
        let back = read_back(&bytes, UTF_8, delimiter);
        prop_assert_eq!(back.len(), rows.len());
        for (got, want) in back.iter().zip(&rows) {
            prop_assert_eq!(got.len(), want.len());
            for (g, w) in got.iter().zip(want) {
                if g != w {
                    prop_assert_eq!(g, &format!("'{w}"));
                    prop_assert!(w.starts_with(['=', '+', '-', '@', '\t', '\r']));
                }
            }
        }
    }

    #[test]
    fn legacy_round_trip(
        case in prop::sample::select(&[0usize, 1, 2, 3, 4][..]),
        seed_rows in rows(&['a', '1', ' ', ',', '"', '\n', '\r', '=']),
        extra in prop::collection::vec(prop::collection::vec(0usize..4, 0..4), 0..4),
        line_ending in line_ending(),
    ) {
        let (encoding, letters): (&'static Encoding, [&str; 4]) = [
            (GBK, ["中", "文", "汉", "字"]),
            (BIG5, ["臺", "北", "中", "文"]),
            (SHIFT_JIS, ["あ", "漢", "字", "ー"]),
            (EUC_KR, ["한", "국", "어", "글"]),
            (WINDOWS_1252, ["é", "€", "ü", "ß"]),
        ][case];
        let mut rows = seed_rows;
        rows.extend(extra.iter().map(|r| r.iter().map(|&i| letters[i].repeat(i + 1)).collect()));
        let options = WriteOptions {
            encoding, bom: false, line_ending, escape_formulas: false, ..WriteOptions::default()
        };
        let bytes = write(&rows, &options).unwrap();
        prop_assert_eq!(read_back(&bytes, encoding, ','), rows);
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = read(&bytes, &ReadOptions::default());
    }
}
