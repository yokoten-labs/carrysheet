//! Read Apple Numbers (`.numbers`) spreadsheets in pure Rust.
//!
//! ```no_run
//! use carrysheet_numbers::{Document, Value};
//!
//! let doc = Document::open("Budget.numbers")?;
//! println!("saved as format {:?}", doc.numbers_version());
//! let table = &doc.sheets()[0].tables()[0];
//! println!("{} has {} rows", table.name(), table.row_count());
//! if let Some(cell) = table.cell(1, 2) {
//!     match cell.value() {
//!         Value::Number(n) => println!("C2 = {n}"),
//!         Value::Text(t) => println!("C2 = {t:?}"),
//!         other => println!("C2 = {other:?}"),
//!     }
//!     if let Some(formula) = cell.formula().and_then(|f| f.text()) {
//!         println!("C2 has the formula {formula}");
//!     }
//! }
//! # Ok::<(), carrysheet_numbers::NumbersError>(())
//! ```
//!
//! # What it reads
//!
//! - All three shapes of a document: the single zip file of current Numbers,
//!   and the package folders of older versions (with `Index.zip`, or with
//!   an `Index/` folder of `.iwa` files). [`Document::from_bytes`] reads the
//!   zip form from memory.
//! - Sheets, tables (name, rows, columns, header and footer rows), and cell
//!   values: numbers, text (rich text as plain text), booleans, dates,
//!   durations, formula errors.
//! - Formulas as text in Numbers notation (see [`Formula`]), and merged
//!   ranges.
//! - The file format version and the history of app builds that saved it.
//!
//! Styles, number formats, comments, images, charts, pivot table settings
//! and password-protected documents are not read yet. Writing is not
//! supported.
//!
//! # Hostile input
//!
//! Every input is treated as hostile. The reader never panics on bad input,
//! and [`Limits`] bounds both what it reads (input, zip entries, bytes after
//! zip and Snappy decompression, objects, tables, rows, columns, cells) and
//! what it produces for the whole document: the memory decoded objects
//! take, estimated before each is decoded; the text of every cell and
//! formula; formula parts written out; merged ranges; the used ranges of
//! tables, which [`Table::rows`] walks. Something stored once
//! and used by many cells is read once and charged for each cell, so a small
//! file cannot expand into gigabytes. A damaged structure returns a
//! [`NumbersError`] that says what, why and how to fix it; a damaged cell is
//! left empty and listed in [`Document::warnings`].
//!
//! # The format
//!
//! A document is a set of `.iwa` archives: chunks of raw Snappy blocks that
//! hold protobuf messages, each an object with a number. Objects refer to
//! each other by number: the document lists sheets, a sheet's tables point
//! to table models, and a model points to tiles of binary cell storage and
//! to lists of strings and formulas. The protobuf definitions come from
//! [numbers-parser](https://github.com/masaccio/numbers-parser) (MIT); see
//! `THIRD-PARTY.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod container;
mod error;
mod iwa;
mod model;
mod plist;
mod proto;

pub use container::Limits;
pub use error::NumbersError;
pub use model::{
    Cell, CellRange, Date, DateParts, Document, Formula, Row, Sheet, Table, Value, Warning,
};
