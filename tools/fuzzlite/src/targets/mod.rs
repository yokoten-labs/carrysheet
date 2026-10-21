//! Fuzz targets: one module per reader.
//!
//! Each target is a plain `fn(&[u8])` that calls the reader's public API
//! with default limits and ignores the result, so the same function plugs
//! into a `cargo fuzz` target later:
//!
//! ```text
//! fuzz_target!(|data: &[u8]| fuzzlite::targets::biff::run(data));
//! ```
//!
//! To add a reader (numbers, query, crypto): write `targets/<name>.rs`
//! with a `pub const TARGETS: &[Target]` (copy `csv.rs`), add the module
//! name to the `targets!` list below, and add the crate to Cargo.toml.

use crate::mutate::Shape;

/// A fuzz target.
#[derive(Debug)]
pub struct Target {
    /// The name on the command line, such as `biff`.
    pub name: &'static str,
    /// One line for `fuzzlite list`.
    pub about: &'static str,
    /// Runs the reader on one input. Must not care about the result.
    pub run: fn(&[u8]),
    /// Seed files, read in place from the workspace.
    pub seeds: &'static [SeedDir],
    /// Inputs to start from besides the seed files.
    pub builtin_seeds: &'static [&'static [u8]],
    /// The format-aware mutations that fit this target's input.
    pub shapes: &'static [Shape],
    /// Not listed: test-only targets.
    pub hidden: bool,
}

/// A folder of seed files.
#[derive(Debug)]
pub struct SeedDir {
    /// Relative to the workspace root, with `/` separators.
    pub dir: &'static str,
    /// File extensions to take (lowercase, without the dot); empty takes
    /// every file except license notes.
    pub extensions: &'static [&'static str],
}

macro_rules! targets {
    ($($module:ident),* $(,)?) => {
        $(pub mod $module;)*

        /// Every target, listed or hidden.
        pub fn all() -> Vec<&'static Target> {
            let mut list: Vec<&'static Target> = Vec::new();
            $(list.extend($module::TARGETS.iter());)*
            list
        }
    };
}

targets!(csv, biff, vba, selftest);

/// The target with this name.
pub fn find(name: &str) -> Option<&'static Target> {
    all().into_iter().find(|t| t.name == name)
}
