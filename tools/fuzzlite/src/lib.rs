//! fuzzlite: a mutation fuzzer for Carrysheet's file readers that needs
//! only stable Rust.
//!
//! It mutates the committed test files of each reader (generic byte
//! mutations plus format-aware ones for BIFF, OLE2, VBA and CSV), runs each
//! case in a child process with a time and memory limit, and reports
//! panics, aborts, timeouts, memory blow-ups and amplification (small
//! inputs that take seconds or hundreds of megabytes), minimised, as JSON
//! lines. See README.md for how to run it.
//!
//! The targets in [`targets`] are plain `fn(&[u8])`, so they can back
//! `cargo fuzz` targets later without change.

// All output goes through `run::sink`, which hides the home folder.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod mutate;
pub mod rng;
pub mod run;
pub mod seeds;
pub mod targets;
