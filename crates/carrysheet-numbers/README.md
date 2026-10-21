# carrysheet-numbers

Read Apple Numbers (`.numbers`) spreadsheets in pure Rust: no Mac, no
Numbers app, no C libraries. It builds for Linux, macOS, Windows and
`wasm32-unknown-unknown`.

```rust
use carrysheet_numbers::{Document, Value};

let doc = Document::open("Budget.numbers")?;
for sheet in doc.sheets() {
    for table in sheet.tables() {
        println!("{} / {}: {} x {}", sheet.name(), table.name(), table.row_count(), table.column_count());
        for row in table.rows() {
            for cell in row.cells().flatten() {
                if let Value::Number(n) = cell.value() {
                    println!("{n}");
                }
            }
        }
    }
}
```

`Document::from_bytes(&[u8])` reads a document held in memory, such as an
upload in a browser.

## What it reads

| | |
|---|---|
| Containers | the single zip file of current Numbers; package folders of older Numbers, with `Index.zip` or with an `Index/` folder |
| Structure | sheets, tables (name, row and column counts, header and footer rows) |
| Values | number (and currency), text (rich text as plain text), true/false, date, duration, formula error, empty |
| Formulas | text in Numbers notation, such as `SUM(A1:A2)` or `A1×2`; references are cell addresses (`$B$1`, `Table 2::C3`, `A:A`) |
| Merged cells | the ranges; the cells they cover read as empty, as in Numbers |
| Metadata | the file format version (`numbers_version()`) and the builds that saved it |

Not yet: styles and number formats, comments, images, charts, pivot table
settings, password-protected documents, tables saved in the older "pre-BNC"
cell storage, and writing. Formula parts that are not decoded yet (for
example LET, LAMBDA, durations and category references) give
`Formula::NotDecoded` with the reason, and a warning with the cell address.

Tested with documents saved by Numbers 11.0 to 15.4 (file format versions
11.0.3 to 26.4.0). A document saved by a newer or older version, or one
that does not say, still reads, with a warning in `Document::warnings()`.

`Table::rows()` walks the table's used range: from the first cell to the
last row and the last column with content (`Table::used_size()`), empty
cells included. The declared size (`row_count()`, `column_count()`) can be
larger and costs nothing. The used range is a box, so content far apart
makes it large; `Limits::max_used_cells` bounds it over the document. To
walk only rows with content, use `Table::non_empty_rows()`.

## Damaged and hostile files

The reader treats every file as hostile. It never panics on bad input, and
`Limits` bounds both what it reads and what it produces:

- What it reads: input size, zip entries (checked in the zip's end records
  before the zip library sets memory aside for them), bytes after zip and
  Snappy decompression (zip bombs), the size of one object, objects, tables,
  rows, columns and cells.
- What it produces, for the whole document: the memory decoded objects take
  (estimated from the protobuf wire format before each object is decoded,
  since an empty sub-message of two bytes becomes a struct of hundreds), the
  text of all cells and formulas (counted once per cell, though cells with
  the same text share one string), formula parts written out, and merged
  ranges. A string, rich text or formula stored once and used by many cells
  is read once, and charged for each cell.

| Limit | Default |
|---|---|
| input file | 256 MiB |
| zip entries | 20,000 |
| bytes after decompression | 256 MiB (objects may be decoded up to twice that) |
| one object | 64 MiB |
| memory of decoded objects | 1 GiB |
| objects | 2,000,000 |
| tables | 10,000 |
| rows, columns of a table | 1,000,000 and 1,000, as in Numbers |
| non-empty cells | 5,000,000 |
| cells in used ranges (what `rows()` walks), summed over tables | 50,000,000 |
| text in cells and formulas | 256 MiB |
| merged ranges | 100,000 per table, 1,000,000 in all |
| formula parts | 10,000 per formula, 50,000,000 written out in all |

With the defaults the reader takes at most about 2 GiB. On 32-bit targets
the defaults are smaller, for at most about 600 MiB: input 64 MiB,
decompressed 128 MiB, decoded objects 256 MiB, 1,000,000 cells, 64 MiB of
text, 20,000,000 formula parts.

Property lists over 1 MiB are skipped without being read, with a warning.

Objects refer to each other only along fixed paths (document, sheet, table,
data lists), so a reference loop ends in an error, and nested formula parts
are cut off by protobuf's recursion limit. In a package folder, symbolic
links are never followed.

- A damaged structure returns a `NumbersError` that says what happened, why,
  and how to fix it. It names parts and object numbers, never cell
  contents, sheet names or file paths.
- A damaged cell is left empty and listed in `Document::warnings()` with
  its sheet, table and address.

## The format, briefly

A document is a set of `.iwa` archives. Each is a list of chunks (a zero
byte, a 24-bit length, then one raw Snappy block), and the decompressed
data is a list of protobuf messages, each an object with a number. The
document object lists the sheets; each table points to a table model; the
model points to tiles of binary cell storage, and to lists of strings,
rich text and formulas.

## Changes

- `Table::rows()` now walks the used range, not the declared size, and
  `Row::cells()` the used columns. Before, an empty table that declared
  1,000,000 × 1,000 cells gave a billion empty entries.

## Protobuf definitions

`proto/*.proto` holds the messages this crate reads, trimmed from the
definitions in numbers-parser (see `THIRD-PARTY.md`). The Rust code in
`src/proto/`, and the wire schema in `src/iwa/schema.rs` that the memory
estimate walks, are generated from them ahead of time and committed, so a
build needs neither `protoc` nor a build script. To change a message, edit the
`.proto` file and, from this folder, run:

```sh
cargo run --manifest-path codegen/Cargo.toml
cargo fmt -p carrysheet-numbers
```

The generator uses protox, a protobuf compiler written in Rust, so `protoc`
is not needed there either.

## License

MIT OR Apache-2.0. The protobuf definitions, the function name table and
the test documents come from numbers-parser under the MIT License; see
`THIRD-PARTY.md`.
