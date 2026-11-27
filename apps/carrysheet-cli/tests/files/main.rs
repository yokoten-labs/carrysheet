//! Tests of `src/files.rs`: the root folder, paths shown with `~`, bounded
//! reads and atomic writes. Moved from the library's `commands_root.rs`,
//! `commands_paths.rs` and `save.rs` (record 0010, decision 2).

#[allow(dead_code)]
#[path = "../../src/files.rs"]
mod files;

mod paths;
mod root;
mod writes;
