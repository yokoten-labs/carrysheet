//! Damaged files return errors, never panics. The cases follow the kernel
//! bench's fuzz check: files cut short and files with bytes changed, from a
//! fixed seed. A panic caught by the kernel comes back as `internal_error`,
//! which these tests count as a failure (record 0003, point 1).

use std::path::{Path, PathBuf};

use carrysheet_commands::engine::{Engine, FileFormat, ReadRequest};
use carrysheet_commands::{CommandError, ErrorCode};
use carrysheet_kernel::Kernel;

fn data(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

/// A small fixed-seed generator (xorshift), so every run tries the same files.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n.max(1)
    }
}

/// Opens, reads, recalculates and encodes; whatever fails must fail cleanly.
fn exercise(name: &str, bytes: Vec<u8>) -> Result<(), CommandError> {
    let k = Kernel::new();
    let book = k.open_bytes(Path::new(name), bytes)?;
    let overview = k.overview(&book)?;
    for s in &overview.sheets {
        let request = ReadRequest {
            range: carrysheet_model::SheetRange::parse(&format!(
                "{}!A1:Z50",
                carrysheet_model::format_sheet_name(&s.name)
            ))
            .ok(),
            ..ReadRequest::default()
        };
        k.read_range(&book, &request)?;
    }
    k.evaluate(&book, None, "SUM(A1:Z50)")?;
    let out = PathBuf::from(name);
    k.encode(&book, &out, FileFormat::Xlsx, None)?;
    k.encode(&book, Path::new("out.csv"), FileFormat::Csv, None)?;
    Ok(())
}

fn check(case: &str, name: &str, bytes: Vec<u8>) -> bool {
    match exercise(name, bytes) {
        Err(e) if e.code == ErrorCode::Internal => panic!("{case}: {}\n{}", e.what, e.why),
        Err(e) => {
            assert!(
                !e.what.is_empty() && !e.fix.is_empty() && !e.why.is_empty(),
                "{case}"
            );
            false
        }
        Ok(()) => true,
    }
}

#[test]
fn truncated_files_fail_cleanly() {
    for name in ["WithTwoCharts.xlsx", "SimpleMacro.xlsm"] {
        let good = data(name);
        let mut rng = Rng(0x5eed_0001);
        let mut cuts: Vec<usize> = (0..60)
            .map(|_| rng.below(good.len() as u64) as usize)
            .collect();
        cuts.extend([0, 1, 4, 30, good.len() - 22, good.len() - 1]);
        for cut in cuts {
            check(&format!("{name} cut at {cut}"), name, good[..cut].to_vec());
        }
    }
}

#[test]
fn flipped_bytes_fail_cleanly() {
    for name in ["WithTwoCharts.xlsx", "SimpleMacro.xlsm"] {
        let good = data(name);
        let mut rng = Rng(0x5eed_0002);
        let mut opened = 0;
        for case in 0..150 {
            let mut bytes = good.clone();
            for _ in 0..=rng.below(8) {
                let at = rng.below(bytes.len() as u64) as usize;
                bytes[at] ^= 1 + rng.below(255) as u8;
            }
            if check(&format!("{name} flip case {case}"), name, bytes) {
                opened += 1;
            }
        }
        // Some flips land in places that do not matter; most break the zip.
        assert!(opened < 150, "{name}: every damaged copy opened");
    }
}

#[test]
fn not_a_workbook() {
    for (name, bytes) in [
        ("empty.xlsx", Vec::new()),
        ("text.xlsx", b"just some text".to_vec()),
        ("zeros.xls", vec![0u8; 4096]),
        (
            "zip.xlsx",
            b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0".to_vec(),
        ),
    ] {
        check(name, name, bytes);
    }
}
