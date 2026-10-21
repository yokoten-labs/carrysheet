# GridCraft (vendored)

Carrysheet's calculation kernel, copied from GridCraft (record 0003, issue 45).

- Upstream: https://github.com/storytold/gridcraft
- Commit: `823c56582d00be91e31b4f7f3680496cf89e84fb` (tag v0.8.0, committed 2026-10-18 UTC)
- Copied: 2026-10-20
- License: MIT OR Apache-2.0, at your choice (`LICENSE-MIT`, `LICENSE-APACHE`). `NOTICE` is
  upstream's, unchanged. Each crate's `license` field is upstream's.

We do not follow upstream's releases. A fix that would help upstream too goes back there as a pull
request; then the change here names it.

## What was copied

Only the crates the core path needs (open xlsx, read, edit, calculate, save), with their `src/`
and `Cargo.toml`. Upstream's integration tests (`tests/`), examples, apps, UI, MCP and xtask were
not copied. Unit tests inside `src/` came along but are not built here.

| Folder | Crate | Rust files | Lines |
|---|---|---|---|
| `crates/base/core` | gridcraft-core | 5 | 1,451 |
| `crates/base/numfmt` | gridcraft-numfmt | 6 | 2,805 |
| `crates/base/formula` | gridcraft-formula | 8 | 2,438 |
| `crates/base/functions` | gridcraft-functions | 16 | 15,661 |
| `crates/book/model` | gridcraft-model | 11 | 3,036 |
| `crates/book/calc` | gridcraft-calc | 5 | 4,629 |
| `crates/draw/chart` | gridcraft-chart | 8 | 4,039 |
| `crates/draw/pdf` | gridcraft-pdf | 3 | 1,126 |
| `crates/xlsx` | gridcraft-xlsx | 50 | 21,625 |
| `crates/engine` | gridcraft-engine | 37 | 31,197 |

`chart` and `pdf` are here because `engine` depends on them (and on `image`, for inserting
pictures). Upstream keeps all crates in one flat `crates/` folder; they are grouped here so no
folder holds more than seven entries.

## What was changed

Items 1 and 2 are what building inside Carrysheet needs; item 3 lists the fixes Carrysheet made here. Each patch is kept small.

1. Manifests: every crate sets `publish = false`. The workspace in `Cargo.toml` here replaces
   upstream's root manifest: the internal paths follow the folders above and carry a version
   (cargo-deny refuses path dependencies without one), and the dependencies only the UI and
   apps use (egui, eframe, rfd, rayon and the like) are gone.
2. zip 3.0.0 → zip 9.0.2 (`deflate-flate2`, with flate2's pure-Rust backend added to
   gridcraft-xlsx). Carrysheet already uses zip 9.0.2, and zip versions before 9 have advisories
   RUSTSEC-2026-0367/0368/0369. Two lines follow from it:
   - `xlsx/src/package.rs`: `ZipArchive::name_for_index` returns a `Result` in zip 9. An entry
     whose name cannot be decoded is skipped, as a missing name was before.
   - `xlsx/src/ods.rs`: `match r.as_ref()` became `match &*r`; zip 9's `typed-path` dependency
     adds an `AsRef` impl that made the old call ambiguous.

3. Patches from review round 1 of issue 45 (record 0003: a hostile file fails fast and a save
   loses nothing silently). Each is marked `Carrysheet patch` in the code; Carrysheet's
   regression tests are in `crates/carrysheet-kernel/tests/limits.rs` and `keep.rs`.

   Limits of one call (H2, H3, M3, M7). Carrysheet sets them before each call
   (`crates/carrysheet-kernel/src/limits.rs`; the values are in
   `crates/carrysheet-commands/src/limits.rs`). Without them GridCraft behaves as upstream.

   | File | Change | Why |
   |---|---|---|
   | `core/src/limits.rs` (new), `core/src/lib.rs` | Thread-local limits and a work counter: array cells, work units, seconds, styles, shared strings, XML nodes, entry bytes, inflate ratio | One place for every limit, so issue 56 can make it one work budget |
   | `functions/src/util.rs`, `dynamic.rs`, `info.rs`, `lift.rs`, `math.rs`, `text.rs` | Array builders compare with `max_cells()` (the caller's limit) instead of `MAX_CELLS` | Two `SEQUENCE(1000000,10)` aborted the process for memory |
   | `calc/src/eval.rs` | `MAP`, `MAKEARRAY` and the other array builders use the caller's limit | Same |
   | `calc/src/recalc.rs` | Work is counted per formula, per cell read and per array cell; out of work or time gives `#CALC!` and the caller reports the limit. Array results over the limit give `#NUM!`. Chains deeper than 256 formulas are computed from their far end (`compute_root`, a work stack) instead of by deeper recursion; a key met again while waiting is a circular reference. Iterative calculation keeps upstream's depth of 2,000 | A `SUMPRODUCT` over 100,000 cells in 100,000 cells ran 300 s; deep chains gave errors or could overflow a 2 MiB thread stack |
   | `xlsx/src/package.rs` | Zip entries are read through a counter: at most `entry_bytes` in or out, and after the first 256 KiB at most `min_inflate_ratio` packed bytes per byte out | 33 MB of empty deflate blocks took 64 s to open |
   | `xlsx/src/xml.rs` | End tags must match, a second root element and a part cut short are errors, and a whole-tree parse holds at most `xml_nodes` elements and attributes | Damaged XML was repaired silently; trees had no size limit |
   | `xlsx/src/sheet_read.rs` | A sheet that does not parse is an error (not the rows read so far); `<col>` ranges are applied last to first with a "next unset column" index, so each column is set once | 100,000 full-width `<col>` took 78 s |
   | `xlsx/src/read.rs` | A sheet read error stops the open; styles are read with errors kept and each list is checked against `styles`; shared strings are counted against `shared_strings`, a damaged part is an error, empty strings share one allocation | 3 million `<xf/>` or 20 million `<si/>` grew without limit |
   | `xlsx/src/write.rs`, `sheet_write.rs` | Shared strings are indexed by the address of the cell's shared text, so a text is hashed once; a sheet's XML stops at the part size limit | One 20 MB text in 100,000 cells timed out on save |
   | `xlsx/src/csv.rs` | The size limit is checked after each field too | One row of huge cells passed the limit before it was checked |
   | `engine/src/cmd/mod.rs` | Row height fitting measures at most the start of a long text | The same 20 MB text was scanned per cell on open |

   Content kept on save (H4). GridCraft does not model these; they are kept as read and written
   back in schema order. Elements of a sheet that name cells are dropped by Carrysheet when rows
   or columns move (`crates/carrysheet-kernel/src/edit.rs`, `forget_kept`), and the save report
   then lists them.

   | File | Change | Why |
   |---|---|---|
   | `xlsx/src/xml.rs` | `write_el` writes a parsed element back as XML | Needed by the rows below |
   | `xlsx/src/read.rs`, `write.rs` | `<fileSharing>`, the `<workbookPr>` attributes other than `date1904` (`codeName`…), each person's `userId` and `providerId` (by display name), and shared strings with rich text or phonetic runs (by their text) are kept in the workbook's `Preserved` elements | Saves dropped them and reported nothing |
   | `xlsx/src/sheet_read.rs`, `sheet_write.rs` | `<sheetPr>` attributes (`codeName`…), `<scenarios>`, `<phoneticPr>`, `<ignoredErrors>`, data table formulas (`<f t="dataTable">`, by cell) and formatted note text (by cell and text) are kept in the sheet's `Preserved` elements | Same |
   | `model/src/features.rs`, `xlsx/src/sheet_read.rs`, `sheet_write.rs` (and the three places that build a `Hyperlink`) | `Hyperlink::display` | The link text was replaced by the target |
   | `xlsx/src/fmla.rs`, `sheet_read.rs`, `read.rs`, `write.rs` | Functions read with `_xlfn.` that GridCraft has no file name for are remembered (`xlfn:NAME`) and written with their prefix | `_xlfn.NEWFN(…)` was saved as `NEWFN(…)`, which Excel cannot compute |
   | `calc/src/recalc.rs` | A formula calling such a function keeps the value the file saved | It became `#NAME?` |

   Other:

   | File | Change | Why |
   |---|---|---|
   | `xlsx/src/passthrough.rs` | `rebase` keeps a link's target as written when the new relative path would need another drive or names of the source's folders | A save could write home or user folder names into the file (L3) |

   Review round 2 of issue 45 (same tests, plus `crates/carrysheet-kernel/src/package.rs`).
   Carrysheet now sets the limits once per command, so opening, calculating and saving share one
   time budget.

   | File | Change | Why |
   |---|---|---|
   | `core/src/limits.rs` | `check_time()` reads the clock outside `charge`; `charge` reads it every 4,096 units instead of 65,536 | Parsing and unpacking had no time check (N5, N6) |
   | `calc/src/eval.rs` | Each lambda call (`MAP`, `REDUCE`, `SCAN`, `BYROW`, `BYCOL`, `MAKEARRAY`) costs one unit, and each loop stops once a limit ran out | `REDUCE(0,SEQUENCE(2000000),LAMBDA(a,b,a+SUM(SEQUENCE(1000))))` ran with no limit (N2) |
   | `functions/src/util.rs`, `lift.rs`, `dynamic.rs` | An array a function builds costs one unit per cell; `SORT` and `SORTBY` are charged before sorting | Same |
   | `xlsx/src/package.rs` | Unpacking checks the time limit on each read | One literal per block stays under the inflate ratio and costs about 1.5 s per MiB (N6) |
   | `xlsx/src/xml.rs` | A streamed parse counts the nodes inside each split element (`row`, `si`) against `xml_nodes`, again from zero for each, and checks the time every 4,096 events | Nodes inside a row or string were never counted (N3) |
   | `model/src/features.rs`, `xlsx/src/sheet_read.rs`, `sheet_write.rs`, `engine/src/cmd/review.rs` | `SheetProtection::objects_editable` and `scenarios_editable`: the `objects` and `scenarios` attributes of `<sheetProtection>` are kept as read (both default to 0); new protections still write 1 | Every save protected objects and scenarios the file left editable (g) |
   | `calc/src/recalc.rs` | `compute_root` tracks the keys on its work stack in a set | `todo.contains` scanned the whole stack per step (L-b) |

Unit tests in `src/` still use zip 3's API in a few places (for example `engine/src/io.rs`); they
are not built here.

## What the code does outside memory

Checked in the copied crates:

- `unsafe`: none. Every crate forbids it (`unsafe_code = "forbid"`).
- Build scripts: none.
- Network: none. `WEBSERVICE` returns `#VALUE!`.
- Files: `engine/src/io.rs` (`read_file`, `write_file`, used by the `file.open` and `file.save`
  commands) and `engine/src/cmd/spelling.rs` (reads `/usr/share/dict/words` for the spelling
  commands). Carrysheet calls neither; it reads and writes bytes itself.
- Environment: `GRIDCRAFT_PROFILE` makes `calc` and `engine` print timings to stderr.
- Panics: `engine::Session::execute` catches a panic in a command, restores the workbook and
  returns an error.

Carrysheet's own lints (`print_stdout`, `clippy.toml`) do not apply here: this folder is a
workspace of its own, built as path dependencies of `crates/carrysheet-kernel`.
