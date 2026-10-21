//! Mutation loop: damage each seed file in many small ways and check that
//! reading never panics, respects the limits, and that whatever reads also
//! survives a write and read round trip.

use std::path::Path;

use carrysheet_csv::{Limits, Malformed, ReadOptions, UTF_8, WriteOptions, read, write};

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

/// Bytes that matter to CSV or to the encodings.
const INTERESTING: &[u8] = &[
    b'"', b',', b';', b'\t', b'|', b'\r', b'\n', b'=', 0x00, 0x80, 0x81, 0xA1, 0xC3, 0xE4, 0xEF,
    0xFE, 0xFF,
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
            4 => out.truncate(at),
            5 => {
                let end = (at + rng.below(16)).min(out.len());
                let copy = out[at..end].to_vec();
                let to = rng.below(out.len() + 1);
                out.splice(to..to, copy);
            }
            _ => out.insert(at, rng.next() as u8),
        }
    }
    out
}

#[test]
fn mutated_seeds_never_panic_and_round_trip() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    // Fixtures sit one folder down, grouped by kind (legacy, unicode, shapes).
    let mut seeds: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .flat_map(|group| std::fs::read_dir(group).unwrap())
        .map(|entry| entry.unwrap().path())
        .filter(|path| !path.file_name().unwrap().to_string_lossy().starts_with('.'))
        .collect();
    seeds.sort();
    assert!(seeds.len() >= 10, "seed files missing in {}", dir.display());

    let limits = Limits {
        max_total_bytes: 512,
        max_rows: 16,
        max_columns: 8,
        max_cells: 64,
        max_field_bytes: 48,
    };
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (mut ok, mut errors) = (0usize, 0usize);
    for seed in &seeds {
        let original = std::fs::read(seed).unwrap();
        for _ in 0..400 {
            let input = mutate(&mut rng, &original);
            for malformed in [Malformed::Error, Malformed::Replace] {
                let options = ReadOptions {
                    malformed,
                    limits,
                    ..ReadOptions::default()
                };
                let result = match read(&input, &options) {
                    Ok(result) => result,
                    Err(e) => {
                        // Every error explains itself.
                        assert!(!e.what().is_empty() && !e.fix().is_empty());
                        errors += 1;
                        continue;
                    }
                };
                ok += 1;
                assert!(result.rows.len() <= limits.max_rows);
                assert!(result.rows.iter().all(|r| r.len() <= limits.max_columns));
                assert!(
                    result
                        .rows
                        .iter()
                        .flatten()
                        .all(|f| f.len() <= limits.max_field_bytes)
                );

                let delimiter = result.detected.delimiter;
                let options = WriteOptions {
                    delimiter,
                    encoding: UTF_8,
                    bom: false,
                    escape_formulas: false,
                    ..WriteOptions::default()
                };
                let bytes = write(&result.rows, &options).unwrap();
                let again = ReadOptions {
                    encoding: Some(UTF_8),
                    delimiter: Some(delimiter),
                    ..ReadOptions::default()
                };
                let back = read(&bytes, &again).unwrap();
                assert_eq!(back.rows, result.rows, "seed {}", seed.display());
            }
        }
    }
    // The loop should exercise both paths, not only one.
    assert!(ok > 100 && errors > 100, "ok {ok}, errors {errors}");
}
