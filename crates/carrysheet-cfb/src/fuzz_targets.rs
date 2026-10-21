//! Entry points for fuzzers (tools/fuzzlite, issue 8) and the proptest.
//!
//! Each function takes any bytes and must return without panicking; a
//! panic is a bug. They are public for the harness only, not API.

use crate::{Builder, CompoundFile};

/// Opens `data`, walks it and reads every entry three ways; then copies
/// it into a [`Builder`], writes it, and checks that the written file
/// opens and holds the same entries, names, metadata and bytes.
///
/// Every step costs at most a few times the input: no per-entry path
/// lookups, and the copy and its check each start from a fresh open, so
/// one step's reads do not use up another's budget.
///
/// Panics only when a file that was read cannot be written back the same,
/// which is a bug in this crate.
pub fn read_write(data: &[u8]) {
    let Ok(file) = CompoundFile::open(data) else {
        return;
    };
    if let Ok(walk) = file.walk("/") {
        for e in walk.entries() {
            let _ = file.read(e);
            let _ = file.read_prefix(e, 100);
            let _ = file.read_lenient(e);
        }
        // A few path lookups, not one per entry.
        for i in 0..walk.len().min(8) {
            if let Some(path) = walk.path(i) {
                let _ = file.read_stream(&path);
            }
        }
    }
    let _ = file.entry("/Workbook");
    let _ = file.is_storage("/_VBA_PROJECT_CUR/VBA");

    // The copy, from a fresh open.
    let Ok(file) = CompoundFile::open(data) else {
        return;
    };
    let Ok(builder) = Builder::from_file(&file) else {
        return;
    };
    let written = match builder.to_bytes() {
        Ok(w) => w,
        Err(e) => panic!("a file that was read could not be written: {e}"),
    };
    // The check, from a fresh open of the written file: it holds each
    // stream once, so copying it never reaches the budget.
    let again = match CompoundFile::open(&written) {
        Ok(f) => f,
        Err(e) => panic!("a written file could not be read: {e}"),
    };
    let copy = match Builder::from_file(&again) {
        Ok(b) => b,
        Err(e) => panic!("a written file could not be copied: {e}"),
    };
    assert!(
        copy.losses().is_empty(),
        "a written file lost data: {:?}",
        copy.losses()
    );
    assert!(
        builder.canonical() == copy.canonical(),
        "the written file differs from the one read"
    );
    // Writing the copy again gives the same bytes: the layout is fixed.
    assert!(
        copy.to_bytes().ok() == Some(written),
        "a second write differs"
    );
}
