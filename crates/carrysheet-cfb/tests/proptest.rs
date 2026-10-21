//! Random and mutated bytes never make the reader panic, and whatever it
//! reads, it writes back the same (`fuzz_targets::read_write`).

mod common;

use carrysheet_cfb::Version;
use carrysheet_cfb::fuzz_targets::read_write;
use proptest::prelude::*;

/// Valid files to mutate: both fixtures and a small file of each version.
fn seeds() -> Vec<Vec<u8>> {
    vec![
        common::fixture("v3.cfb"),
        common::fixture("v4.cfb"),
        common::sample(Version::V3),
        common::sample(Version::V4),
    ]
}

/// 256 cases by default; PROPTEST_CASES=<n> runs a longer session.
fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), ..ProptestConfig::default() })]

    #[test]
    fn random_bytes(mut bytes in proptest::collection::vec(any::<u8>(), 0..4096), signed in any::<bool>()) {
        if signed && bytes.len() >= 8 {
            bytes[..8].copy_from_slice(&carrysheet_cfb::SIGNATURE);
        }
        read_write(&bytes);
    }

    #[test]
    fn mutated_files(
        seed in 0usize..4,
        edits in proptest::collection::vec((any::<usize>(), any::<u8>(), 0u8..4), 1..12),
        cut in proptest::option::of(any::<usize>()),
    ) {
        let mut bytes = seeds().swap_remove(seed);
        for (at, value, how) in edits {
            // Bias toward the header and the first sectors, where the
            // tables are.
            let span = if how == 0 { bytes.len() } else { bytes.len().min(4096 + 1024) };
            let at = at % span;
            match how {
                0 | 1 => bytes[at] = value,
                2 => bytes[at] ^= 1 << (value % 8),
                _ => {
                    // A whole little-endian word set to a special or large value.
                    let word: u32 = [0xFFFF_FFFE, 0xFFFF_FFFF, 0xFFFF_FFFD, 0, 1, 0x7FFF_FFFF]
                        [usize::from(value) % 6];
                    let at = at & !3;
                    if at + 4 <= bytes.len() {
                        bytes[at..at + 4].copy_from_slice(&word.to_le_bytes());
                    }
                }
            }
        }
        if let Some(cut) = cut {
            let len = bytes.len();
            bytes.truncate(cut % (len + 1));
        }
        read_write(&bytes);
    }
}
