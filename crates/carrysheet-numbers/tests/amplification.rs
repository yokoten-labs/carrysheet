//! Small documents that would expand into gigabytes: one stored thing
//! referred to or repeated many times. Each must stop at a limit (or finish)
//! quickly, with bounded memory. Built in code; see `common`.

mod common;

use std::time::{Duration, Instant};

use carrysheet_numbers::{Document, Formula, Limits, NumbersError, Value};
use common::*;

/// The most memory this test process may ever have used. Without the
/// limits, the documents below take gigabytes.
const MAX_PEAK: u64 = 512 << 20;

/// The peak memory of this process so far (Linux only; elsewhere `None`).
fn peak_memory() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// Runs `read` and checks it is quick and stays within [`MAX_PEAK`].
fn bounded<T>(seconds: u64, read: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = read();
    let took = start.elapsed();
    assert!(
        took < Duration::from_secs(seconds),
        "took {took:?}, more than {seconds} s"
    );
    if let Some(peak) = peak_memory() {
        assert!(peak < MAX_PEAK, "peak memory {} MiB", peak >> 20);
    }
    result
}

fn too_large(result: Result<Document, NumbersError>, what: &str) {
    match result {
        Err(NumbersError::TooLarge { what: w, .. }) => assert!(w.contains(what), "{w}"),
        Err(e) => panic!("expected too large ({what}), got: {e}"),
        Ok(_) => panic!("expected too large ({what}), but it read"),
    }
}

fn read_with(bytes: &[u8], limits: &Limits) -> Result<Document, NumbersError> {
    Document::from_bytes_with_limits(bytes, limits)
}

// --- H1: decoded objects much larger than their bytes ----------------------

#[test]
fn empty_sub_messages_are_charged_before_decoding() {
    // 1 million empty row infos: 2 MB on the wire, over 200 MB as structs
    // (a little over 120 bytes each, with room for the Vec to grow). With
    // the default 1 GiB, 8 million would be refused the same way.
    let doc = Doc {
        rows_msgs: vec![Msg::new(); 1_000_000],
        ..Doc::default()
    };
    let bytes = doc.bytes();
    assert!(bytes.len() < 3 << 20);
    let mut limits = Limits::default();
    limits.max_decoded_memory = 64 << 20;
    too_large(
        bounded(30, || read_with(&bytes, &limits)),
        "memory for decoded objects",
    );
}

#[test]
fn archive_headers_are_capped() {
    // An archive header of 40,000 empty message infos (80 KB).
    let mut info = Msg::new().uint(1, 1);
    for _ in 0..40_000 {
        info = info.msg(2, Msg::new());
    }
    let mut payload = Vec::new();
    varint(info.0.len() as u64, &mut payload);
    payload.extend_from_slice(&info.0);
    let bytes = stored(&[("Index/Document.iwa", iwa(&payload))]);
    too_large(bounded(10, || read(&bytes)), "archive header");
}

// --- H2: one stored thing copied into every cell ---------------------------

/// `rows` rows of `columns` copies of one cell.
fn grid(rows: u64, columns: usize, cell: &[u8]) -> Vec<Msg> {
    (0..rows)
        .map(|r| row(r, &vec![Some(cell.to_vec()); columns]))
        .collect()
}

#[test]
fn one_long_string_in_many_cells() {
    // A 1 MiB string in 300,000 cells: 300 GB if each cell had a copy.
    let text: &'static str = Box::leak("x".repeat(1 << 20).into_boxed_str());
    let doc = Doc {
        rows: 300,
        columns: 1000,
        rows_msgs: grid(300, 1000, &text_cell(1)),
        strings: vec![(1, text)],
        ..Doc::default()
    };
    let bytes = doc.bytes();
    assert!(bytes.len() < 8 << 20);
    too_large(bounded(30, || read(&bytes)), "bytes of text in cells");

    // A short string in the same 300,000 cells reads, sharing one string.
    let doc = Doc {
        rows: 300,
        columns: 1000,
        rows_msgs: grid(300, 1000, &text_cell(1)),
        strings: vec![(1, "shared")],
        ..Doc::default()
    };
    let doc = bounded(30, || read(&doc.bytes())).unwrap();
    let t = &doc.sheets()[0].tables()[0];
    assert_eq!(t.value(299, 999), &Value::Text("shared".into()));
    match (t.value(0, 0), t.value(299, 999)) {
        (Value::Text(a), Value::Text(b)) => assert!(std::sync::Arc::ptr_eq(a, b)),
        other => panic!("{other:?}"),
    }
}

/// A rich text list (object 9) whose entry 1 has this text, and the
/// payload (10) and storage (11) objects behind it.
fn rich_text(text: &str) -> (Msg, Vec<(u64, u64, Msg)>) {
    let list = Msg::new()
        .uint(1, 8)
        .uint(2, 10)
        .msg(3, Msg::new().uint(1, 1).uint(2, 1).msg(9, reference(10)));
    let payload = Msg::new().msg(1, reference(11));
    let storage = Msg::new().bytes(3, text.as_bytes());
    // Field 4 of the model is the data store; a second copy merges into
    // it, adding the rich text list (field 17).
    let model_extra = Msg::new().msg(4, Msg::new().msg(17, reference(9)));
    (
        model_extra,
        vec![(9, 6005, list), (10, 6218, payload), (11, 2001, storage)],
    )
}

#[test]
fn rich_text_is_read_once_and_charged_per_cell() {
    let rich_cell = cell(9, 0x10, &1u32.to_le_bytes());
    // A 1 MiB rich text in 300,000 cells.
    let (model_extra, objects) = rich_text(&"y".repeat(1 << 20));
    let doc = Doc {
        rows: 300,
        columns: 1000,
        rows_msgs: grid(300, 1000, &rich_cell),
        model_extra: Some(model_extra),
        extra_objects: objects,
        ..Doc::default()
    };
    too_large(bounded(30, || read(&doc.bytes())), "bytes of text in cells");

    // A short one reads; its payload and storage are decoded once.
    let (model_extra, objects) = rich_text("rich");
    let doc = Doc {
        rows: 300,
        columns: 1000,
        rows_msgs: grid(300, 1000, &rich_cell),
        model_extra: Some(model_extra),
        extra_objects: objects,
        ..Doc::default()
    };
    let mut limits = Limits::default();
    limits.max_total_bytes = 16 << 20;
    let doc = bounded(30, || read_with(&doc.bytes(), &limits)).unwrap();
    let t = &doc.sheets()[0].tables()[0];
    assert_eq!(t.value(299, 999), &Value::Text("rich".into()));
    assert_eq!(doc.warnings(), []);
}

/// A formula of `refs` references to the cell itself, added together, so
/// its text depends on the cell.
fn relative_formula(refs: usize) -> Msg {
    let mut nodes = vec![ast_node(36)];
    for _ in 1..refs {
        nodes.push(ast_node(36));
        nodes.push(ast_node(1));
    }
    formula(&nodes)
}

#[test]
fn one_formula_written_for_many_cells_is_charged_per_cell() {
    // 3,000 references in 100,000 cells: 600 million nodes to write out,
    // over the default 50 million. A limit of 5 million keeps the test
    // quick in debug builds; the default stops it the same way, later.
    let doc = Doc {
        rows: 100,
        columns: 1000,
        rows_msgs: grid(100, 1000, &formula_cell(1)),
        formulas: vec![(1, relative_formula(3000))],
        ..Doc::default()
    };
    let mut limits = Limits::default();
    limits.max_document_formula_nodes = 5_000_000;
    too_large(
        bounded(30, || read_with(&doc.bytes(), &limits)),
        "formula parts",
    );

    // The same formula in 1,000 cells reads, with each cell's own text.
    let doc = Doc {
        rows: 1,
        columns: 1000,
        rows_msgs: grid(1, 1000, &formula_cell(1)),
        formulas: vec![(1, relative_formula(3))],
        ..Doc::default()
    };
    let doc = bounded(30, || read(&doc.bytes())).unwrap();
    let t = &doc.sheets()[0].tables()[0];
    let text = |c| {
        t.cell(0, c)
            .and_then(|c| c.formula())
            .and_then(Formula::text)
    };
    assert_eq!(text(0), Some("A1+A1+A1"));
    assert_eq!(text(27), Some("AB1+AB1+AB1"));
}

#[test]
fn a_formula_with_too_many_parts_is_not_decoded() {
    let doc = Doc {
        formulas: vec![(1, relative_formula(6000))],
        ..Doc::default()
    };
    let doc = bounded(10, || read(&doc.bytes())).unwrap();
    let c = doc.sheets()[0].tables()[0].cell(0, 1).unwrap();
    match c.formula() {
        Some(Formula::NotDecoded(why)) => assert!(why.contains("10000 parts"), "{why}"),
        other => panic!("{other:?}"),
    }
}

// --- H3: zip directories ---------------------------------------------------

/// A file whose zip64 end record says `entries` entries, consistent enough
/// for the zip crate to accept it and set aside memory for them.
fn zip64_claiming(entries: u64) -> Vec<u8> {
    let at = entries * 46;
    let mut data = vec![0u8; at as usize];
    data[..2].copy_from_slice(b"PK");
    data.extend_from_slice(b"PK\x06\x06");
    data.extend_from_slice(&44u64.to_le_bytes()); // record size
    data.extend_from_slice(&[45, 0, 45, 0]); // versions
    data.extend_from_slice(&[0; 8]); // disk numbers
    data.extend_from_slice(&entries.to_le_bytes()); // on this disk
    data.extend_from_slice(&entries.to_le_bytes()); // in all
    data.extend_from_slice(&0u64.to_le_bytes()); // directory size
    data.extend_from_slice(&0u64.to_le_bytes()); // directory offset
    data.extend_from_slice(b"PK\x06\x07");
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&at.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());
    data.extend_from_slice(b"PK\x05\x06");
    data.extend_from_slice(&[0; 4]);
    data.extend_from_slice(&[0xff; 12]);
    data.extend_from_slice(&[0; 2]);
    data
}

#[test]
fn zip64_entry_counts_are_checked_before_the_zip_crate() {
    let bytes = zip64_claiming(40_000);
    too_large(bounded(10, || read(&bytes)), "entries in the zip file");
    // Within the limit, the zip crate gets it and finds it damaged.
    let e = read(&zip64_claiming(100)).unwrap_err();
    assert!(matches!(e, NumbersError::Damaged { .. }), "{e}");
}

/// `zeros` bytes of padding, then `records` end records that each say one
/// entry at offset 0, where there is none: the zip crate tries every one.
fn failing_end_records(zeros: usize, records: usize) -> Vec<u8> {
    let mut data = b"PK".to_vec();
    data.resize(zeros.max(2), 0);
    for _ in 0..records {
        data.extend_from_slice(b"PK\x05\x06");
        data.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
        data.extend_from_slice(&[0; 10]);
    }
    data
}

#[test]
fn many_end_records_are_quick_then_refused() {
    // The zip crate tries each of 4,000 end records. Searching the 16 MiB
    // before each one again would be 64 GB of reading; with the archive
    // offset known it checks each in place.
    let bytes = failing_end_records(16 << 20, 4_000);
    let start = Instant::now();
    let e = read(&bytes).unwrap_err();
    let took = start.elapsed();
    assert!(matches!(e, NumbersError::Damaged { .. }), "{e}");
    assert!(
        e.to_string().starts_with("The document file is damaged"),
        "{e}"
    );
    assert!(took < Duration::from_millis(500), "took {took:?}");
    eprintln!("16 MiB with 4,000 end records: {took:?}");
    // Over the backstop of 4,096 is refused before the zip crate.
    let e = read(&failing_end_records(0, 4_097)).unwrap_err();
    assert!(e.to_string().contains("end-of-directory records"), "{e}");
}

#[test]
fn zip64_records_over_the_limit_are_refused_even_if_inconsistent() {
    // A zip64 record claiming 2^40 entries 100 bytes into the file, which
    // the zip crate's own check (46 bytes per entry) would reject today.
    let mut data = b"PK".to_vec();
    data.resize(100, 0);
    data.extend_from_slice(b"PK\x06\x06");
    data.extend_from_slice(&44u64.to_le_bytes());
    data.extend_from_slice(&[45, 0, 45, 0]);
    data.extend_from_slice(&[0; 8]);
    data.extend_from_slice(&(1u64 << 40).to_le_bytes());
    data.extend_from_slice(&(1u64 << 40).to_le_bytes());
    data.extend_from_slice(&[0; 16]);
    too_large(read(&data), "entries in the zip file");
}

#[test]
fn large_property_lists_are_skipped_unread() {
    // 2 MiB of property list, deflated to a few KB. The decompression
    // budget of 1.5 MiB shows it was not read.
    let mut plist = PROPERTIES.as_bytes().to_vec();
    plist.resize(2 << 20, b' ');
    let bytes = zip(
        &[
            (
                "Index/Document.iwa",
                iwa(&Doc::default().objects().concat()),
            ),
            ("Metadata/Properties.plist", plist),
        ],
        zip::CompressionMethod::Deflated,
    );
    let mut limits = Limits::default();
    limits.max_total_bytes = 3 << 19;
    let doc = bounded(10, || read_with(&bytes, &limits)).unwrap();
    assert_eq!(doc.numbers_version(), None);
    let messages: Vec<String> = doc.warnings().iter().map(|w| w.message.clone()).collect();
    assert!(
        messages[0].starts_with("Metadata/Properties.plist is larger than 1024 KiB"),
        "{messages:?}"
    );
}

#[test]
fn a_nested_index_zip_is_held_to_the_input_limit() {
    // A small deflated file holding a 1 MB Index.zip.
    let inner = stored(&[("Index/Skip.bin", vec![0; 1 << 20])]);
    let outer = zip(&[("Index.zip", inner)], zip::CompressionMethod::Deflated);
    assert!(outer.len() < 64 << 10);
    let mut limits = Limits::default();
    limits.max_input_bytes = 256 << 10;
    too_large(
        bounded(10, || read_with(&outer, &limits)),
        "nested Index.zip",
    );
}

// --- H5: merged ranges -----------------------------------------------------

#[test]
fn merges_in_one_table_are_capped() {
    // 100,001 single-row merges in the merge region map.
    let doc = Doc {
        rows: 200_000,
        merges: (0..100_001u32).map(|r| (r, 1 << 16 | 1)).collect(),
        ..Doc::default()
    };
    too_large(
        bounded(30, || read(&doc.bytes())),
        "merged ranges in one table",
    );
}

fn uuid(lower: u64) -> Msg {
    Msg::new().uint(1, lower).uint(2, 0)
}

/// The calculation engine (50), and owner records: the tables' haunted
/// owner (51) maps them to owner id 7; `merges` ranges (52) belong to
/// owner id 8, which is no table.
fn engine(merges: u32) -> (Msg, Vec<(u64, u64, Msg)>) {
    let entry =
        |internal: u64, id: u64| Msg::new().uint(1, internal).msg(2, Msg::new().uint(2, id));
    let map = Msg::new().msg(1, entry(9, 7)).msg(1, entry(10, 8));
    let engine = Msg::new().msg(2, Msg::new().msg(3, map));
    let haunted = Msg::new()
        .msg(1, uuid(1))
        .uint(2, 1)
        .uint(3, 35)
        .msg(12, uuid(7));
    let mut ranges = Msg::new();
    for r in 0..merges {
        let range = Msg::new()
            .uint(1, 0)
            .uint(2, u64::from(r))
            .uint(3, 1)
            .uint(4, u64::from(r));
        let reference = Msg::new().uint(1, 10).msg(2, range);
        ranges = ranges.msg(2, Msg::new().uint(1, 0).uint(2, 0).msg(4, reference));
    }
    let owner = Msg::new()
        .msg(1, uuid(2))
        .uint(2, 2)
        .uint(3, 5)
        .msg(5, ranges);
    // Field 84 of the model: its haunted owner.
    let model_extra = Msg::new().msg(84, Msg::new().msg(1, uuid(1)));
    (
        model_extra,
        vec![(50, 4000, engine), (51, 4008, haunted), (52, 4008, owner)],
    )
}

#[test]
fn merges_are_grouped_by_owner_once() {
    // 5,000 tables and 500,000 merged ranges of another owner: looking
    // through every range for every table would be 2.5 billion steps.
    let (model_extra, objects) = engine(500_000);
    let doc = Doc {
        extra_tables: 4_999,
        model_extra: Some(model_extra),
        extra_objects: objects,
        ..Doc::default()
    };
    let doc = bounded(60, || read(&doc.bytes())).unwrap();
    assert_eq!(doc.sheets()[0].tables().len(), 5_000);
    assert!(doc.sheets()[0].tables()[0].merged_ranges().is_empty());
}

#[test]
fn merges_in_the_document_are_capped() {
    let (model_extra, objects) = engine(1_001);
    let doc = Doc {
        model_extra: Some(model_extra),
        extra_objects: objects,
        ..Doc::default()
    };
    let mut limits = Limits::default();
    limits.max_document_merges = 1_000;
    too_large(
        bounded(10, || read_with(&doc.bytes(), &limits)),
        "merged ranges in the document",
    );
}

// --- M1, M2: defaults, declared sizes and table counts ---------------------

#[cfg(target_pointer_width = "32")]
#[test]
fn smaller_defaults_on_32_bit_targets() {
    let l = Limits::default();
    assert_eq!(l.max_input_bytes, 64 << 20);
    assert_eq!(l.max_total_bytes, 128 << 20);
    assert_eq!(l.max_decoded_memory, 256 << 20);
    assert_eq!(l.max_cells, 1_000_000);
    assert_eq!(l.max_text_bytes, 64 << 20);
    assert_eq!(l.max_document_formula_nodes, 20_000_000);
}

#[cfg(not(target_pointer_width = "32"))]
#[test]
fn defaults() {
    let l = Limits::default();
    assert_eq!(l.max_input_bytes, 256 << 20);
    assert_eq!(l.max_total_bytes, 256 << 20);
    assert_eq!(l.max_decoded_memory, 1 << 30);
    assert_eq!(l.max_used_cells, 50_000_000);
    assert_eq!(l.max_zip_entries, 20_000);
    assert_eq!(l.max_objects, 2_000_000);
    assert_eq!(l.max_tables, 10_000);
    assert_eq!((l.max_rows, l.max_columns), (1_000_000, 1_000));
    assert_eq!(l.max_cells, 5_000_000);
    assert_eq!(l.max_text_bytes, 256 << 20);
    assert_eq!((l.max_merges, l.max_document_merges), (100_000, 1_000_000));
    assert_eq!(
        (l.max_formula_nodes, l.max_document_formula_nodes),
        (10_000, 50_000_000)
    );
}

#[test]
fn a_large_empty_table_iterates_nothing() {
    // Declared 1,000,000 × 1,000, with no cells.
    let doc = Doc {
        rows: 1_000_000,
        columns: 1_000,
        rows_msgs: Vec::new(),
        ..Doc::default()
    };
    let doc = read(&doc.bytes()).unwrap();
    let t = &doc.sheets()[0].tables()[0];
    assert_eq!((t.row_count(), t.column_count()), (1_000_000, 1_000));
    let cells = bounded(5, || t.rows().map(|r| r.cells().count()).sum::<usize>());
    assert_eq!(cells, 0);
    assert_eq!(t.used_size(), (0, 0));

    // One cell in the middle: rows up to it, columns up to it.
    let doc = Doc {
        rows: 1_000_000,
        columns: 1_000,
        rows_msgs: vec![row(9, &[None, None, Some(text_cell(1))])],
        ..Doc::default()
    };
    let doc = read(&doc.bytes()).unwrap();
    let t = &doc.sheets()[0].tables()[0];
    assert_eq!(t.used_size(), (10, 3));
    assert_eq!(t.rows().count(), 10);
    assert_eq!(t.rows().last().unwrap().cells().count(), 3);
    assert_eq!(t.non_empty_rows().next().unwrap().cells().count(), 3);
}

/// One cell at A1 and one at the far corner of a 1,000,000 × 1,000 table:
/// a used range of a billion cells from a few kilobytes.
fn corners() -> Doc {
    let mut far = vec![None; 999];
    far.push(Some(text_cell(1)));
    Doc {
        rows: 1_000_000,
        columns: 1_000,
        rows_msgs: vec![row(0, &[Some(text_cell(1))]), row(999_999, &far)],
        ..Doc::default()
    }
}

#[test]
fn used_ranges_are_bounded() {
    let bytes = corners().bytes();
    assert!(bytes.len() < 8 << 10);
    let what = "cells in the used ranges";
    too_large(bounded(10, || read(&bytes)), what);
    let mut limits = Limits::default();
    limits.max_used_cells = 1_000_000;
    too_large(bounded(10, || read_with(&bytes, &limits)), what);
    // Summed over tables: 10 tables sharing one tile of 10 × 3.
    let doc = Doc {
        rows_msgs: vec![row(9, &[None, None, Some(text_cell(1))])],
        rows: 10,
        columns: 3,
        extra_tables: 9,
        ..Doc::default()
    };
    limits.max_used_cells = 299;
    too_large(read_with(&doc.bytes(), &limits), what);
    limits.max_used_cells = 300;
    assert!(read_with(&doc.bytes(), &limits).is_ok());
}

#[test]
fn too_many_tables() {
    let doc = Doc {
        extra_tables: 10_000,
        ..Doc::default()
    };
    too_large(bounded(30, || read(&doc.bytes())), "tables");
}

// --- M3: versions ----------------------------------------------------------

#[test]
fn a_newer_version_reads_with_a_warning() {
    let plist = PROPERTIES.replace("14.2.2", "27.1");
    let bytes = stored(&[
        (
            "Index/Document.iwa",
            iwa(&Doc::default().objects().concat()),
        ),
        ("Metadata/Properties.plist", plist.into_bytes()),
    ]);
    let doc = read(&bytes).unwrap();
    assert_eq!(doc.numbers_version(), Some("27.1"));
    assert_eq!(doc.warnings().len(), 1);
    assert!(
        doc.warnings()[0].message.contains("newer"),
        "{}",
        doc.warnings()[0]
    );
}

// --- L1: links out of a package folder -------------------------------------

#[cfg(unix)]
#[test]
fn a_metadata_link_out_of_the_package_is_not_followed() {
    let base = std::env::temp_dir().join(format!("carrysheet-numbers-link-{}", std::process::id()));
    let package = base.join("Doc.numbers");
    let outside = base.join("outside");
    std::fs::create_dir_all(package.join("Index")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        package.join("Index/Document.iwa"),
        iwa(&Doc::default().objects().concat()),
    )
    .unwrap();
    std::fs::write(outside.join("Properties.plist"), PROPERTIES).unwrap();
    std::os::unix::fs::symlink(&outside, package.join("Metadata")).unwrap();
    let doc = Document::open(&package);
    let _ = std::fs::remove_dir_all(&base);
    let doc = doc.unwrap();
    assert_eq!(doc.numbers_version(), None, "the link was followed");
}

// --- L5: dates and durations that are not numbers --------------------------

#[test]
fn dates_and_durations_must_be_finite() {
    let nan = f64::NAN.to_le_bytes();
    let doc = Doc {
        rows_msgs: vec![row(
            0,
            &[
                Some(cell(5, 0x4, &nan)),
                Some(cell(7, 0x2, &f64::INFINITY.to_le_bytes())),
            ],
        )],
        ..Doc::default()
    };
    let doc = read(&doc.bytes()).unwrap();
    let t = &doc.sheets()[0].tables()[0];
    assert_eq!(t.value(0, 0), &Value::Empty);
    assert_eq!(t.value(0, 1), &Value::Empty);
    assert_eq!(doc.warnings().len(), 2, "{:?}", doc.warnings());
}

#[test]
fn a_large_property_list_in_a_package_is_skipped() {
    let base =
        std::env::temp_dir().join(format!("carrysheet-numbers-plist-{}", std::process::id()));
    let package = base.join("Doc.numbers");
    std::fs::create_dir_all(package.join("Index")).unwrap();
    std::fs::create_dir_all(package.join("Metadata")).unwrap();
    std::fs::write(
        package.join("Index/Document.iwa"),
        iwa(&Doc::default().objects().concat()),
    )
    .unwrap();
    let mut plist = PROPERTIES.as_bytes().to_vec();
    plist.resize(2 << 20, b' ');
    std::fs::write(package.join("Metadata/Properties.plist"), plist).unwrap();
    let doc = Document::open(&package);
    let _ = std::fs::remove_dir_all(&base);
    let doc = doc.unwrap();
    assert_eq!(doc.numbers_version(), None);
    assert!(doc.warnings()[0].message.contains("larger than 1024 KiB"));
}
