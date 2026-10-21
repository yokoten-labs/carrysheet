//! Deflate streams whose last few KiB inflate to far more than the
//! reader's 32 KiB output buffer, such as a part that ends in a run of
//! zeros. Once the last input byte is consumed, the inflater still holds
//! that output; the reader must drain it before it calls the data cut
//! short (round-4 review: Apache POI's 49273.xlsx, `xl/styles.xml`,
//! 1,477 bytes in and 63,770 out, was refused).
//!
//! The shipped CLI inflates with miniz_oxide; `cargo test --workspace`
//! unifies flate2 to zlib-rs. Both are run on purpose, each with a
//! feature that makes `the_expected_inflater_is_linked` check which one
//! this test binary got:
//!
//!   cargo test -p carrysheet-zip --features test-miniz
//!   cargo test -p carrysheet-zip --features test-zlib-rs
//!
//! (`scripts/check.mjs` runs both, as its `zip-miniz` and `zip-zlib-rs`
//! steps.)

mod common;

use std::io::{Cursor, Write};

use carrysheet_zip::{Archive, Error, Limits};
use common::*;
use flate2::write::DeflateEncoder;
use flate2::{Compression, Decompress, FlushDecompress};

/// The inflater flate2 links: zlib-rs names its errors, miniz_oxide's
/// errors carry no message.
fn inflater() -> &'static str {
    let mut d = Decompress::new(false);
    let mut out = [0u8; 16];
    // BFINAL 1, BTYPE 11: the reserved block type, invalid everywhere.
    let e = d
        .decompress(&[0x07, 0, 0, 0], &mut out, FlushDecompress::Finish)
        .unwrap_err();
    if e.message().is_some() {
        "zlib-rs"
    } else {
        "miniz_oxide"
    }
}

#[test]
fn the_expected_inflater_is_linked() {
    let got = inflater();
    eprintln!("inflater: {got}");
    if cfg!(feature = "test-miniz") {
        assert_eq!(
            got, "miniz_oxide",
            "feature test-miniz, but flate2 links {got}"
        );
    }
    if cfg!(feature = "test-zlib-rs") {
        assert_eq!(
            got, "zlib-rs",
            "feature test-zlib-rs, but flate2 links {got}"
        );
    }
}

/// Bytes that look like an XML part.
fn text(n: usize) -> Vec<u8> {
    let row = b"<row r=\"1\"><c r=\"A1\" s=\"3\" t=\"s\"><v>17</v></c></row>\n";
    row.iter().copied().cycle().take(n).collect()
}

/// Bytes that do not compress (xorshift).
fn noise(n: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// A raw deflate stream of `parts`, with a sync flush after each one
/// but the last when `flush` is set.
fn encode(level: u32, parts: &[&[u8]], flush: bool) -> Vec<u8> {
    let mut e = DeflateEncoder::new(Vec::new(), Compression::new(level));
    for (i, part) in parts.iter().enumerate() {
        e.write_all(part).unwrap();
        if flush && i + 1 < parts.len() {
            e.flush().unwrap();
        }
    }
    e.finish().unwrap()
}

/// A one-entry archive whose data is `stream`, inflating to `content`.
fn archive(content: &[u8], stream: Vec<u8>) -> Vec<u8> {
    let mut raw = Raw::new("xl/styles.xml", b"", Method::Deflated);
    raw.crc = crc32(content);
    raw.size = content.len() as u32;
    raw.local_size = raw.size;
    raw.compressed_size = stream.len() as u32;
    raw.local_compressed_size = raw.compressed_size;
    raw.data = stream;
    let mut b = Builder::new();
    b.raw(raw);
    b.finish()
}

fn open(bytes: Vec<u8>) -> Archive<Cursor<Vec<u8>>> {
    Archive::open(Cursor::new(bytes), Limits::default()).unwrap()
}

/// The beginnings of the parts: nothing, an XML-like text, and bytes
/// that do not compress (stored blocks before the zeros).
fn heads() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("zeros only", Vec::new()),
        ("text then zeros", text(3000)),
        ("noise then zeros", noise(70_000)),
    ]
}

/// Totals just past a multiple of the reader's 32 KiB buffer: the
/// stream's last few bytes then code more output than the buffer has
/// room left for, so the inflater has consumed all input while it still
/// holds output. Before the fix, about a quarter of these were refused
/// on each inflater (953 of 3,990 on miniz_oxide, 716 on zlib-rs, in a
/// sweep of every third total from 1 to 400 bytes past 32, 64, 96 and
/// 128 KiB).
const PAST: [usize; 8] = [7, 10, 100, 133, 136, 202, 256, 300];

#[test]
fn high_ratio_tails_read_whole_at_every_level() {
    let mut cases = 0;
    for (shape, head) in heads() {
        for k in [1, 2, 4] {
            for past in PAST {
                let total = k * (32 << 10) + past;
                let Some(zeros) = total.checked_sub(head.len()) else {
                    continue;
                };
                let content = [head.as_slice(), &vec![0u8; zeros]].concat();
                for level in [1, 6, 9] {
                    for flush in [false, true] {
                        let stream = encode(level, &[&head, &content[head.len()..]], flush);
                        let what = format!(
                            "{shape}, {total} bytes, level {level}, flush {flush}, {} bytes in",
                            stream.len()
                        );
                        let small = stream.len() <= 60 << 10;
                        let mut a = open(archive(&content, stream));
                        let out = a
                            .read(0, 64 << 20)
                            .unwrap_or_else(|e| panic!("{what}: {e}"));
                        assert!(out == content, "{what}: wrong bytes");
                        // The prefix path drains the same way (within
                        // its 64 KiB input budget).
                        if small {
                            let n = content.len() - 1;
                            let first = a
                                .read_prefix(0, n)
                                .unwrap_or_else(|e| panic!("{what}, prefix: {e}"));
                            assert!(first == content[..n], "{what}: wrong prefix");
                        }
                        cases += 1;
                    }
                }
            }
        }
    }
    eprintln!("{cases} streams read on {}", inflater());
}

/// The shape of the reported part: a tail of a few KiB that inflates to
/// well over 32 KiB, ending the stream.
#[test]
fn the_tail_is_small_and_its_output_large() {
    let content = [text(2 << 10), vec![0u8; 512 << 10]].concat();
    let stream = encode(9, &[&content], false);
    let tail_in = encode(9, &[&vec![0u8; 512 << 10]], false).len();
    assert!(tail_in < 4 << 10, "tail is {tail_in} bytes");
    let mut a = open(archive(&content, stream));
    assert_eq!(a.read(0, 64 << 20).unwrap().len(), content.len());
}

/// Draining does not hide a stream that is cut short: with the last
/// bytes gone, the reader still says so.
#[test]
fn a_cut_stream_is_still_refused() {
    let content = [text(4 << 10), vec![0u8; 256 << 10]].concat();
    let stream = encode(6, &[&content], false);
    for cut in [1, 2, 8, stream.len() / 2] {
        let short = stream[..stream.len() - cut].to_vec();
        let mut a = open(archive(&content, short));
        match a.read(0, 64 << 20) {
            Err(Error::Damaged(_)) => {}
            other => panic!("cut {cut}: {other:?}"),
        }
    }
    // Only the final block's end missing: everything inflates, then the
    // stream never ends.
    let mut a = open(archive(&content, stream[..stream.len() - 1].to_vec()));
    let e = a.read(0, 64 << 20).unwrap_err().to_string();
    assert!(
        e.contains("ends before") || e.contains("different size") || e.contains("not valid"),
        "{e}"
    );
}
