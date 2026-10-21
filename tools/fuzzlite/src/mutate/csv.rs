//! CSV mutations: huge quoted fields, many delimiters and rows, byte order
//! marks, UTF-16, legacy and invalid byte sequences, `sep=` lines.

use super::bytes::insert;
use crate::rng::Rng;

const DELIMS: &[u8] = b",;\t|";
const BOMS: &[&[u8]] = &[
    b"\xEF\xBB\xBF",
    b"\xFF\xFE",
    b"\xFE\xFF",
    b"\x00\x00\xFE\xFF",
    b"\xFF\xFE\x00\x00",
];
/// Bytes from legacy encodings and broken UTF-8: GBK, Big5, Shift_JIS,
/// EUC-KR lead bytes, lone continuation bytes, overlong forms, surrogates.
const ODD: &[&[u8]] = &[
    b"\xB0\xA1",
    b"\xA4\x40",
    b"\x82\xA0",
    b"\xB0",
    b"\x80",
    b"\xC3\x28",
    b"\xC0\x80",
    b"\xED\xA0\x80",
    b"\xF4\x90\x80\x80",
    b"\xF0\x9F\x98\x80",
    b"\x00",
    b"\xE2\x80\xA8",
    b"\x81\x30\x81\x30",
];

pub fn mutate(rng: &mut Rng, input: &[u8], max_len: usize) -> (Vec<u8>, &'static str) {
    let mut b = input.to_vec();
    let at = rng.below(b.len() + 1);
    let room = max_len.saturating_sub(b.len()).max(1);
    let op = match rng.below(9) {
        0 | 1 => {
            // A quoted field past the field limit, closed or not.
            let n = rng
                .pick(&[1 << 10, 1 << 16, (1 << 20) + 1, 300_000])
                .min(room);
            let filler = rng.pick(&[&b"x"[..], b",", b"\n", b"\"\"", b"\r\n", b"\xC3\xA9"]);
            let mut field = vec![b'"'];
            field.extend(filler.iter().copied().cycle().take(n));
            if rng.one_in(2) {
                field.push(b'"');
            }
            insert(&mut b, at, &field, max_len);
            "csv:huge_quoted"
        }
        2 => {
            // Many delimiters in one row, or in many rows.
            let d = rng.pick(DELIMS);
            let cols = rng.pick(&[100usize, 16_384, 16_385, 70_000]);
            let rows = rng.count(room / cols.max(1));
            let mut row = vec![d; cols];
            row.push(b'\n');
            insert(&mut b, at, &row.repeat(rows), max_len);
            "csv:many_delimiters"
        }
        3 => {
            let end = rng.pick(&[&b"\n"[..], b"\r", b"\r\n", b"\n\r"]);
            let n = rng.count(room / end.len());
            insert(&mut b, at, &end.repeat(n), max_len);
            "csv:many_rows"
        }
        4 => {
            let bom = rng.pick(BOMS);
            match rng.below(3) {
                0 => insert(&mut b, 0, bom, max_len),
                1 => insert(&mut b, at, bom, max_len),
                _ => {
                    let drop = bom.len().min(b.len());
                    b.drain(..drop);
                }
            }
            "csv:bom"
        }
        5 => {
            // The text, or part of it, as UTF-16.
            let big = rng.one_in(2);
            let from = if rng.one_in(2) { 0 } else { at.min(b.len()) };
            let mut out = b[..from].to_vec();
            if from == 0 && rng.one_in(2) {
                out.extend_from_slice(if big { b"\xFE\xFF" } else { b"\xFF\xFE" });
            }
            for &c in &b[from..] {
                if big {
                    out.extend_from_slice(&[0, c]);
                } else {
                    out.extend_from_slice(&[c, 0]);
                }
            }
            if rng.one_in(4) {
                out.pop();
            }
            b = out;
            "csv:utf16"
        }
        6 => {
            let odd = rng.pick(ODD);
            let n = rng.count(room / odd.len()).min(4096);
            insert(&mut b, at, &odd.repeat(n), max_len);
            "csv:encoding"
        }
        7 => {
            let line = rng.pick(&[
                &b"sep=;\n"[..],
                b"sep=\n",
                b"sep=\"\n",
                b"sep=\xC3\xA9\n",
                b"sep=\0\n",
                b"sep=;;\r\n",
                b"\"sep=,\"\n",
            ]);
            insert(&mut b, if rng.one_in(4) { at } else { 0 }, line, max_len);
            "csv:sep_line"
        }
        _ => {
            let unit = rng.pick(&[&b"\""[..], b"\"\"", b"\",\"", b"\"\n\""]);
            let n = rng.count(room / unit.len());
            insert(&mut b, at, &unit.repeat(n), max_len);
            "csv:quotes"
        }
    };
    (b, op)
}
