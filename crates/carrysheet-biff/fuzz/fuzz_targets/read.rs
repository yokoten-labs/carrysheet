//! Feeds arbitrary bytes to the reader; it must return Ok or Err, never
//! panic, hang or run out of memory.
//!
//! Run from crates/carrysheet-biff with a nightly toolchain and the sample
//! files as the seed corpus:
//!
//!     cargo +nightly fuzz run read fuzz/corpus/read tests/data tests/data/fuzz
//!
//! The stable test suite runs a seeded mutation test instead
//! (tests/mutation.rs).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(book) = carrysheet_biff::read(data) {
        // Walk the result too, so the summaries are exercised.
        let _ = book.summaries();
    }
});
