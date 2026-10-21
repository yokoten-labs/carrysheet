//! Mutation loop with a fixed seed: damage each fixture in many small ways,
//! at three levels (the zip bytes, the compressed `.iwa` bytes, and the
//! decompressed protobuf and cell bytes), and check that reading never
//! panics and stays inside the limits.

use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;

use carrysheet_numbers::{Document, Limits};

/// carrysheet-zip's byte-level zip builder, used in place.
#[path = "../../carrysheet-zip/tests/common/mod.rs"]
mod zip_builder;

/// A small deterministic generator (xorshift64*), so failures repeat.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Bytes that matter to varints, lengths, wire types and cell storage.
const INTERESTING: &[u8] = &[
    0x00, 0x01, 0x02, 0x05, 0x07, 0x08, 0x0a, 0x7f, 0x80, 0xfe, 0xff,
];

fn mutate(rng: &mut Rng, input: &[u8]) -> Vec<u8> {
    let mut out = input.to_vec();
    for _ in 0..=rng.below(4) {
        let at = rng.below(out.len() + 1);
        match rng.below(7) {
            0 if at < out.len() => out[at] ^= 1 << rng.below(8),
            1 if at < out.len() => out[at] = INTERESTING[rng.below(INTERESTING.len())],
            2 => out.insert(at, INTERESTING[rng.below(INTERESTING.len())]),
            3 if at < out.len() => {
                out.remove(at);
            }
            4 if rng.below(4) == 0 => out.truncate(at),
            5 => {
                let end = (at + rng.below(32)).min(out.len());
                let copy = out[at..end].to_vec();
                let to = rng.below(out.len() + 1);
                out.splice(to..to, copy);
            }
            _ if at + 4 <= out.len() => {
                // A large little-endian number, as in cell flags or offsets.
                out[at..at + 4].copy_from_slice(&(rng.next() as u32 | 0x8000_0000).to_le_bytes());
            }
            _ => {}
        }
    }
    out
}

fn unzip(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let limits = carrysheet_zip::Limits::default();
    let mut archive = carrysheet_zip::Archive::open(Cursor::new(bytes), limits).unwrap();
    (0..archive.entries().len())
        .map(|i| {
            let name = archive.entries()[i].name.clone();
            (name, archive.read(i, u64::MAX).unwrap())
        })
        .collect()
}

fn rezip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut b = zip_builder::Builder::new();
    for (name, data) in entries {
        b.add(name, data, zip_builder::Method::Stored);
    }
    b.finish()
}

/// The decompressed contents of an `.iwa` part.
fn iwa_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let len = usize::from(data[pos + 1])
            | usize::from(data[pos + 2]) << 8
            | usize::from(data[pos + 3]) << 16;
        let block = &data[pos + 4..pos + 4 + len];
        out.extend(snap::raw::Decoder::new().decompress_vec(block).unwrap());
        pos += 4 + len;
    }
    out
}

/// An `.iwa` part holding `payload` in one chunk.
fn iwa_encode(payload: &[u8]) -> Vec<u8> {
    let block = snap::raw::Encoder::new().compress_vec(payload).unwrap();
    let len = block.len() as u32;
    let mut out = vec![0, len as u8, (len >> 8) as u8, (len >> 16) as u8];
    out.extend_from_slice(&block);
    out
}

/// Reads the bytes and walks every cell. Returns a short outcome label.
fn exercise(bytes: &[u8], limits: &Limits) -> String {
    match Document::from_bytes_with_limits(bytes, limits) {
        Ok(doc) => {
            for sheet in doc.sheets() {
                for table in sheet.tables() {
                    for row in table.rows() {
                        for cell in row.cells().flatten() {
                            let _ = format!("{:?} {:?}", cell.value(), cell.formula());
                        }
                    }
                    for m in table.merged_ranges() {
                        let _ = m.to_string();
                    }
                }
            }
            for w in doc.warnings() {
                let _ = w.to_string();
            }
            if doc.warnings().is_empty() {
                "ok"
            } else {
                "ok with warnings"
            }
            .to_owned()
        }
        Err(e) => {
            let _ = e.to_string();
            let debug = format!("{e:?}");
            debug
                .split([' ', '(', '{'])
                .next()
                .unwrap_or_default()
                .to_owned()
        }
    }
}

const FIXTURES: &[&str] = &[
    "cells/test-1.numbers",
    "cells/test-4.numbers",
    "cells/test-empty-rows.numbers",
    "formulas/test-10.numbers",
    "formulas/issue-42.numbers",
    "formulas/test-issue-253.numbers",
    "merges/test-9.numbers",
    "merges/issue-18.numbers",
];

/// Rounds per fixture; set `MUTATION_ROUNDS` for a longer local run.
fn rounds() -> usize {
    std::env::var("MUTATION_ROUNDS")
        .ok()
        .and_then(|r| r.parse().ok())
        .unwrap_or(200)
}

#[test]
fn mutated_fixtures_never_panic() {
    let mut limits = Limits::default();
    limits.max_total_bytes = 32 << 20;
    limits.max_cells = 100_000;
    let mut rng = Rng(0x5eed_cafe_f00d_0001);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut outcomes = std::collections::BTreeMap::new();
    for name in FIXTURES {
        let original = std::fs::read(root.join(name)).unwrap();
        let entries = unzip(&original);
        let iwa: Vec<usize> = (0..entries.len())
            .filter(|&i| entries[i].0.ends_with(".iwa"))
            .collect();
        let decoded: Vec<Vec<u8>> = iwa.iter().map(|&i| iwa_decode(&entries[i].1)).collect();
        // Unpacked and packed again without damage, the fixture still reads
        // (issue-18 with a warning: it does not say its version).
        let expected = if name.contains("issue-18") {
            "ok with warnings"
        } else {
            "ok"
        };
        assert_eq!(exercise(&rezip(&entries), &limits), expected, "{name}");
        for round in 0..rounds() {
            let bytes = match rng.below(4) {
                0 => mutate(&mut rng, &original),
                1 => {
                    let pick = rng.below(iwa.len());
                    let mut e = entries.clone();
                    e[iwa[pick]].1 = mutate(&mut rng, &entries[iwa[pick]].1);
                    rezip(&e)
                }
                _ => {
                    let pick = rng.below(iwa.len());
                    let mut e = entries.clone();
                    e[iwa[pick]].1 = iwa_encode(&mutate(&mut rng, &decoded[pick]));
                    rezip(&e)
                }
            };
            let outcome = catch_unwind(AssertUnwindSafe(|| exercise(&bytes, &limits)));
            let Ok(outcome) = outcome else {
                let out =
                    std::env::temp_dir().join(format!("carrysheet-numbers-panic-{round}.numbers"));
                std::fs::write(&out, &bytes).unwrap();
                panic!(
                    "{name}, round {round}: the reader panicked; input saved to {}",
                    out.display()
                );
            };
            *outcomes.entry(outcome).or_insert(0usize) += 1;
        }
    }
    // `cargo test -- --nocapture` shows how the damage was handled.
    eprintln!("{outcomes:#?}");
    assert!(
        outcomes.len() > 3,
        "the mutations should reach different checks"
    );
}
