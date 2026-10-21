//! A stable, seeded mutation test (no nightly toolchain needed): every
//! sample is truncated and has bytes flipped at random, and the reader must
//! neither panic nor hang. The cargo-fuzz target in fuzz/ does the same
//! with coverage guidance.
//!
//! A longer run: `BIFF_MUTATIONS=2000 cargo test --release -p carrysheet-biff
//! --test mutation -- --nocapture`.

// The test reports what it compared with --nocapture; the workspace denies
// println! only so that library and MCP code never write to stdout.
#![allow(clippy::print_stdout)]

mod common;

use std::cell::RefCell;
use std::time::{Duration, Instant};

use common::*;

/// xorshift64*: small, fast, and the same on every machine.
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

thread_local! {
    /// Where each panic in the current case happened ("file:line").
    static PANICS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn corpus() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for path in sample_files() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(&path).unwrap();
        // Mutating the BIFF stream itself reaches deeper than mutating the
        // OLE2 container around it.
        if let Ok(stream) = carrysheet_biff::workbook_stream(&bytes)
            && stream.as_slice() != bytes.as_slice()
        {
            out.push((format!("{name} (stream)"), stream));
        }
        out.push((name, bytes));
    }
    out.push(("biff2 (built)".into(), biff2_sheet()));
    out.push((
        "biff5 cp936 (built)".into(),
        biff5_book(936, b"\xB1\xED", b"\xD6\xD0\xCE\xC4"),
    ));
    out.push(("biff8 split SST (built)".into(), biff8_split_sst()));
    out.push(("biff4 workbook (built)".into(), biff4_workbook()));
    out
}

#[test]
fn mutated_samples_never_panic_or_hang() {
    // A quiet hook: record where each panic happened instead of printing it.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        let at = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        PANICS.with(|p| p.borrow_mut().push(at));
    }));

    let corpus = corpus();
    // BIFF_MUTATIONS=<cases per input> runs a longer session; the seed can
    // be changed with BIFF_MUTATION_SEED.
    let per_item = std::env::var("BIFF_MUTATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6);
    let seed = std::env::var("BIFF_MUTATION_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x00C0_FFEE_D00D_F00D);
    let mut rng = Rng(seed);
    let mut cases = 0;
    let mut slowest = (Duration::ZERO, String::new());
    let mut outcomes = [0usize; 2];
    for (name, original) in &corpus {
        for case in 0..per_item {
            let mut bytes = original.clone();
            let what = match case % 3 {
                0 => {
                    let at = rng.below(bytes.len());
                    bytes.truncate(at);
                    format!("truncated at {at}")
                }
                1 => {
                    let n = 1 + rng.below(8);
                    for _ in 0..n {
                        let at = rng.below(bytes.len());
                        if let Some(b) = bytes.get_mut(at) {
                            *b ^= 1 << rng.below(8);
                        }
                    }
                    format!("{n} bits flipped")
                }
                _ => {
                    // Overwrite a stretch with random bytes (often a record
                    // header or a length field), then truncate.
                    let at = rng.below(bytes.len());
                    let len = 1 + rng.below(16);
                    for i in at..(at + len).min(bytes.len()) {
                        bytes[i] = rng.next() as u8;
                    }
                    let cut = bytes.len() - rng.below(bytes.len() / 4 + 1);
                    bytes.truncate(cut);
                    format!("{len} bytes at {at} randomized, cut to {cut}")
                }
            };
            PANICS.with(|p| p.borrow_mut().clear());
            let start = Instant::now();
            let result = std::panic::catch_unwind(|| carrysheet_biff::read(&bytes));
            let took = start.elapsed();
            let panics = PANICS.with(|p| std::mem::take(&mut *p.borrow_mut()));
            // Any panic is a bug: the compound file reader returns errors.
            if !panics.is_empty() {
                drop(std::panic::take_hook());
                panic!("{name}, case {case} ({what}): the reader panicked at {panics:?}");
            }
            match result {
                Ok(Ok(_)) => outcomes[0] += 1,
                Ok(Err(e)) => {
                    outcomes[1] += 1;
                    assert!(!e.what().is_empty() && !e.why().is_empty());
                }
                Err(_) => unreachable!("a panic is caught above"),
            }
            assert!(
                took < Duration::from_secs(10),
                "{name}, case {case} ({what}): took {took:?}"
            );
            if took > slowest.0 {
                slowest = (took, format!("{name} ({what})"));
            }
            cases += 1;
        }
    }
    std::panic::set_hook(default_hook);
    println!(
        "{cases} cases from {} inputs: {} read, {} refused; slowest {:?} for {}",
        corpus.len(),
        outcomes[0],
        outcomes[1],
        slowest.0,
        slowest.1
    );
    assert!(cases >= 300);
}
