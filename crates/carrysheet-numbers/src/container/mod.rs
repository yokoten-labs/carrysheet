//! The outer container: a zip file, or a package folder from older Numbers.
//!
//! A `.numbers` document comes in three shapes:
//!
//! - a zip file with `Index/*.iwa` and `Metadata/*.plist` inside (current);
//! - a folder with an `Index.zip` and a `Metadata/` folder (older Numbers);
//! - a folder with an `Index/` folder of `.iwa` files (older still).
//!
//! This module turns any of them into a list of named byte parts, keeping
//! only the parts the reader uses, within the [`Limits`].

mod package;
mod zip;

use std::fs;
use std::io::{BufReader, Cursor};
use std::path::Path;

use crate::error::NumbersError;

/// Limits that keep a damaged or hostile document from using up memory or
/// time. Every count is checked before the memory is taken, and everything
/// the reader produces from a small input (decoded objects, cell text,
/// formula text, merged ranges, the rows and columns a program walks) is
/// charged to a budget for the whole document, so a stored thing referred
/// to many times cannot multiply.
///
/// The defaults read every real document we know of. With them the reader
/// takes at most about 2 GiB: 256 MiB of decompressed parts, up to 1 GiB of
/// decoded objects (estimated, summed over the document; far less is held
/// at once), and about 110 bytes for each of 5,000,000 cells plus their
/// text. On 32-bit targets the defaults are smaller (see each field), for
/// at most about 600 MiB. Lower them for untrusted input on small machines.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// The largest input file, in bytes. Default 256 MiB (64 MiB on
    /// 32-bit targets).
    pub max_input_bytes: u64,
    /// The most entries a zip file may have. Default 20,000.
    pub max_zip_entries: usize,
    /// The most bytes after decompression, zip and Snappy together, summed
    /// over the whole document. Objects may be decoded up to twice this many
    /// bytes in all. Default 256 MiB (128 MiB on 32-bit targets).
    pub max_total_bytes: u64,
    /// The largest single object, in bytes. Default 64 MiB.
    pub max_object_bytes: u64,
    /// The most memory decoded objects may take, summed over the document,
    /// as estimated from each object before it is decoded. Default 1 GiB
    /// (256 MiB on 32-bit targets).
    pub max_decoded_memory: u64,
    /// The most objects in the document. Default 2,000,000.
    pub max_objects: usize,
    /// The most tables in the document. Default 10,000.
    pub max_tables: usize,
    /// The most rows in one table. Default 1,000,000, as in Numbers.
    pub max_rows: u32,
    /// The most columns in one table. Default 1,000, as in Numbers.
    pub max_columns: u32,
    /// The most non-empty cells in the whole document. Default 5,000,000
    /// (1,000,000 on 32-bit targets).
    pub max_cells: usize,
    /// The most cells in the used ranges of all tables, summed: for each
    /// table, its rows up to the last row with content times its columns up
    /// to the last column with content. This is what [`Table::rows`] walks,
    /// so it bounds the work of reading every row of every table, however
    /// far apart the content is. Default 50,000,000.
    ///
    /// [`Table::rows`]: crate::Table::rows
    pub max_used_cells: u64,
    /// The most bytes of text in cells (values and formulas), summed over
    /// every cell of the document, as a program that copies each cell would
    /// hold it. Default 256 MiB (64 MiB on 32-bit targets).
    pub max_text_bytes: u64,
    /// The most merged ranges in one table. Default 100,000.
    pub max_merges: usize,
    /// The most merged ranges in the whole document. Default 1,000,000.
    pub max_document_merges: usize,
    /// The most formula nodes in one formula. Default 10,000.
    pub max_formula_nodes: usize,
    /// The most formula nodes read and written out over the whole
    /// document, counted for every cell. Default 50,000,000 (20,000,000 on
    /// 32-bit targets).
    pub max_document_formula_nodes: u64,
}

/// Defaults that differ with the address space: (input, decompressed,
/// decoded memory, cells, text, formula nodes).
#[cfg(not(target_pointer_width = "32"))]
const SIZED: (u64, u64, u64, usize, u64, u64) = (
    256 << 20,
    256 << 20,
    1 << 30,
    5_000_000,
    256 << 20,
    50_000_000,
);
#[cfg(target_pointer_width = "32")]
const SIZED: (u64, u64, u64, usize, u64, u64) = (
    64 << 20,
    128 << 20,
    256 << 20,
    1_000_000,
    64 << 20,
    20_000_000,
);

impl Default for Limits {
    fn default() -> Self {
        let (input, total, decoded, cells, text, nodes) = SIZED;
        Self {
            max_input_bytes: input,
            max_zip_entries: 20_000,
            max_total_bytes: total,
            max_object_bytes: 64 << 20,
            max_decoded_memory: decoded,
            max_objects: 2_000_000,
            max_tables: 10_000,
            max_rows: 1_000_000,
            max_columns: 1_000,
            max_cells: cells,
            max_used_cells: 50_000_000,
            max_text_bytes: text,
            max_merges: 100_000,
            max_document_merges: 1_000_000,
            max_formula_nodes: 10_000,
            max_document_formula_nodes: nodes,
        }
    }
}

/// Counts decompressed bytes against [`Limits::max_total_bytes`].
pub(crate) struct Budget {
    left: u64,
    limit: u64,
}

impl Budget {
    pub(crate) fn new(limit: u64) -> Self {
        Self { left: limit, limit }
    }

    /// Bytes still allowed.
    pub(crate) fn left(&self) -> u64 {
        self.left
    }

    /// Takes `n` bytes from the budget, or fails when they are not there.
    pub(crate) fn take(&mut self, n: u64) -> Result<(), NumbersError> {
        match self.left.checked_sub(n) {
            Some(left) => {
                self.left = left;
                Ok(())
            }
            None => Err(self.exceeded()),
        }
    }

    pub(crate) fn exceeded(&self) -> NumbersError {
        NumbersError::TooLarge {
            what: "bytes after decompression",
            limit: self.limit,
        }
    }
}

/// One part of the package, such as `Index/Document.iwa`.
pub(crate) struct Part {
    pub(crate) name: String,
    pub(crate) data: Vec<u8>,
}

/// The parts the reader uses.
#[derive(Default)]
pub(crate) struct Parts {
    /// `.iwa` archives, in the order they appear.
    pub(crate) archives: Vec<Part>,
    /// `Metadata/Properties.plist`, when present.
    pub(crate) properties: Option<Vec<u8>>,
    /// `Metadata/BuildVersionHistory.plist`, when present.
    pub(crate) build_history: Option<Vec<u8>>,
    /// Property lists too large to be real, skipped without being read.
    pub(crate) skipped: Vec<String>,
}

/// The part name for the document file itself; errors say "the document
/// file" for it.
const OUTER: &str = "";
const PROPERTIES: &str = "Metadata/Properties.plist";
const BUILD_HISTORY: &str = "Metadata/BuildVersionHistory.plist";
const PASSWORD_PARTS: [&str; 2] = [".iwpv2", ".iwph"];

/// What a part is used for, judged by its name.
enum Use {
    Archive,
    Properties,
    BuildHistory,
    NestedIndex,
    Password,
    Skip,
}

fn use_of(name: &str) -> Use {
    // Some documents have every part under one root folder.
    let base = name.rsplit('/').next().unwrap_or(name);
    if PASSWORD_PARTS.contains(&base) {
        Use::Password
    } else if name.ends_with(".iwa") {
        Use::Archive
    } else if name.ends_with(PROPERTIES) {
        Use::Properties
    } else if name.ends_with(BUILD_HISTORY) {
        Use::BuildHistory
    } else if base.eq_ignore_ascii_case("Index.zip") {
        Use::NestedIndex
    } else {
        Use::Skip
    }
}

fn input_too_large(limits: &Limits) -> NumbersError {
    NumbersError::TooLarge {
        what: "bytes of input",
        limit: limits.max_input_bytes,
    }
}

/// Reads a document held in memory: a zip file.
pub(crate) fn read_bytes(
    bytes: &[u8],
    limits: &Limits,
    budget: &mut Budget,
) -> Result<Parts, NumbersError> {
    if bytes.len() as u64 > limits.max_input_bytes {
        return Err(input_too_large(limits));
    }
    let mut parts = Parts::default();
    zip::read(Cursor::new(bytes), OUTER, limits, budget, &mut parts, true)?;
    finish(parts)
}

/// Reads a document from a path: a zip file or a package folder.
pub(crate) fn read_path(
    path: &Path,
    limits: &Limits,
    budget: &mut Budget,
) -> Result<Parts, NumbersError> {
    let meta = fs::metadata(path)?;
    if meta.is_dir() {
        let parts = package::read(path, limits, budget)?;
        return finish(parts);
    }
    if !meta.is_file() {
        return Err(NumbersError::NotNumbers {
            reason: "it is neither a file nor a folder",
        });
    }
    if meta.len() > limits.max_input_bytes {
        return Err(input_too_large(limits));
    }
    // The zip is read in place, not copied into memory first.
    let file = fs::File::open(path)?;
    if file.metadata()?.len() > limits.max_input_bytes {
        return Err(input_too_large(limits));
    }
    let mut parts = Parts::default();
    zip::read(
        BufReader::new(file),
        OUTER,
        limits,
        budget,
        &mut parts,
        true,
    )?;
    finish(parts)
}

fn finish(parts: Parts) -> Result<Parts, NumbersError> {
    if parts.archives.is_empty() {
        return Err(NumbersError::NotNumbers {
            reason: "it has no Index/*.iwa parts",
        });
    }
    Ok(parts)
}
