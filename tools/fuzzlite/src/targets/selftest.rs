//! Hidden targets that misbehave on purpose, for fuzzlite's own tests: each
//! one fails only when its trigger byte is in the input, so the minimiser
//! has something to shrink.

use std::hint::black_box;
use std::time::{Duration, Instant};

use super::Target;

const fn hidden(name: &'static str, about: &'static str, run: fn(&[u8])) -> Target {
    Target {
        name,
        about,
        run,
        seeds: &[],
        builtin_seeds: &[b"hello ! ~ # % ^ world"],
        shapes: &[],
        hidden: true,
    }
}

pub const TARGETS: &[Target] = &[
    hidden("selftest-ok", "does nothing", |_| {}),
    hidden("selftest-panic", "panics when the input holds '!'", panic),
    hidden("selftest-loop", "never returns when the input holds '~'", spin),
    hidden("selftest-alloc", "fills memory when the input holds '#'", alloc),
    hidden("selftest-slow", "takes 1.5 s when the input holds '%'", slow),
    hidden("selftest-big", "uses 96 MiB when the input holds '^'", big),
    hidden(
        "selftest-caught",
        "catches its own panic when the input holds '!'",
        caught,
    ),
];

fn panic(data: &[u8]) {
    assert!(!data.contains(&b'!'), "selftest-panic found '!'");
}

fn spin(data: &[u8]) {
    if data.contains(&b'~') {
        let mut n = 0u64;
        loop {
            n = black_box(n.wrapping_add(1));
        }
    }
}

fn alloc(data: &[u8]) {
    if data.contains(&b'#') {
        // Touch every page, so the memory is real, 64 MiB at a time.
        let mut blocks = Vec::new();
        loop {
            blocks.push(vec![1u8; 64 << 20]);
            black_box(&blocks);
        }
    }
}

fn slow(data: &[u8]) {
    if data.contains(&b'%') {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(1500) {
            black_box(start);
        }
    }
}

fn big(data: &[u8]) {
    if data.contains(&b'^') {
        black_box(vec![1u8; 96 << 20]);
    }
}

/// Like a reader's `catch_unwind` guard around its own code.
fn caught(data: &[u8]) {
    let _ = std::panic::catch_unwind(|| panic(data));
}
