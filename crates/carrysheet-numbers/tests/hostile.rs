//! Hand-built hostile documents: zip bombs, huge lengths, loops between
//! objects, missing objects and broken cells. Each must give an error or a
//! warning, never a panic or a huge allocation.

mod common;

use carrysheet_numbers::{Document, Formula, Limits, NumbersError, Value};
use common::*;

// --- Tests ------------------------------------------------------------------

#[test]
fn the_built_document_reads() {
    let doc = read(&Doc::default().bytes()).unwrap();
    assert_eq!(doc.warnings(), []);
    let t = &doc.sheets()[0].tables()[0];
    assert_eq!(t.value(0, 0), &Value::Text("hi".into()));
    let c = t.cell(0, 1).unwrap();
    assert_eq!(c.value(), &Value::Duration(7.0));
    assert_eq!(c.formula(), Some(&Formula::Text("7".into())));
    assert_eq!(doc.numbers_version(), Some("14.2.2"));
}

#[test]
fn zip_bomb_stops_at_the_limit() {
    let zeros = vec![0u8; 4 << 20];
    let bomb = zip(
        &[("Index/Document.iwa", zeros)],
        zip::CompressionMethod::Deflated,
    );
    assert!(
        bomb.len() < 64 << 10,
        "the bomb should be small: {}",
        bomb.len()
    );
    let mut limits = Limits::default();
    limits.max_total_bytes = 1 << 20;
    let e = Document::from_bytes_with_limits(&bomb, &limits).unwrap_err();
    assert!(matches!(e, NumbersError::TooLarge { .. }), "{e}");
}

#[test]
fn too_many_zip_entries() {
    let entries: Vec<(String, Vec<u8>)> = (0..20)
        .map(|i| (format!("Index/x{i}.iwa"), Vec::new()))
        .collect();
    let entries: Vec<(&str, Vec<u8>)> = entries
        .iter()
        .map(|(n, d)| (n.as_str(), d.clone()))
        .collect();
    let mut limits = Limits::default();
    limits.max_zip_entries = 10;
    let e = Document::from_bytes_with_limits(&stored(&entries), &limits).unwrap_err();
    assert!(matches!(e, NumbersError::TooLarge { .. }), "{e}");
}

#[test]
fn input_size_limit() {
    let mut limits = Limits::default();
    limits.max_input_bytes = 10;
    let e = Document::from_bytes_with_limits(&Doc::default().bytes(), &limits).unwrap_err();
    assert!(matches!(e, NumbersError::TooLarge { .. }), "{e}");
}

#[test]
fn snappy_header_larger_than_its_block_is_refused_before_memory_is_taken() {
    // The block says 4 GiB but holds nothing: no block of 5 bytes can
    // become more than 110, so it is refused before memory is set aside.
    let mut block = Vec::new();
    varint(u64::from(u32::MAX), &mut block);
    let e = read(&stored(&[("Index/Document.iwa", chunk(&block))])).unwrap_err();
    assert!(matches!(e, NumbersError::Damaged { .. }), "{e}");
    assert!(
        e.to_string().contains("larger than the block can hold"),
        "{e}"
    );
    // A block that could hold its size, but the size is over the budget.
    let mut block = Vec::new();
    varint(8 << 20, &mut block);
    block.resize(1 << 20, 0);
    let mut limits = Limits::default();
    limits.max_total_bytes = 4 << 20;
    let bytes = stored(&[("Index/Document.iwa", chunk(&block))]);
    let e = Document::from_bytes_with_limits(&bytes, &limits).unwrap_err();
    assert!(matches!(e, NumbersError::TooLarge { .. }), "{e}");
    // Over 32 bits is simply wrong.
    let mut block = Vec::new();
    varint(1 << 40, &mut block);
    let e = read(&stored(&[("Index/Document.iwa", chunk(&block))])).unwrap_err();
    assert!(matches!(e, NumbersError::Damaged { .. }), "{e}");
}

#[test]
fn huge_and_broken_lengths() {
    let damaged = |payload: Vec<u8>| {
        let e = read(&stored(&[("Index/Document.iwa", payload)])).unwrap_err();
        assert!(matches!(e, NumbersError::Damaged { .. }), "{e}");
    };
    // Archive header length of 2^60: over the 64 KiB a header may have.
    let mut p = Vec::new();
    varint(1 << 60, &mut p);
    let e = read(&stored(&[("Index/Document.iwa", iwa(&p))])).unwrap_err();
    assert!(matches!(e, NumbersError::TooLarge { .. }), "{e}");
    // A header length within the cap but longer than the data.
    let mut p = Vec::new();
    varint(1000, &mut p);
    damaged(iwa(&p));
    // A varint that never ends.
    damaged(iwa(&[0xff; 12]));
    // An object longer than the data, and one over the 64 MiB an object
    // may have.
    let object = |length: u64| {
        let info = Msg::new()
            .uint(1, 1)
            .msg(2, Msg::new().uint(1, 1).uint(3, length));
        let mut p = Vec::new();
        varint(info.0.len() as u64, &mut p);
        p.extend_from_slice(&info.0);
        iwa(&p)
    };
    damaged(object(1000));
    let e = read(&stored(&[(
        "Index/Document.iwa",
        object(u64::from(u32::MAX)),
    )]))
    .unwrap_err();
    assert!(matches!(e, NumbersError::TooLarge { .. }), "{e}");
    // A chunk that does not start with zero, and one longer than the data.
    let mut bad = iwa(b"x");
    bad[0] = 1;
    damaged(bad);
    let mut bad = iwa(b"x");
    bad[1] = 0xff;
    damaged(bad);
    damaged(vec![0, 0]);
}

#[test]
fn missing_objects() {
    let mut objects = Doc::default().objects();
    objects.remove(0);
    let e = read(&document(&objects)).unwrap_err();
    assert!(
        matches!(e, NumbersError::MissingObject { id: 1, .. }),
        "{e}"
    );

    let doc = Doc {
        table_model_ref: 99,
        ..Doc::default()
    };
    let e = read(&doc.bytes()).unwrap_err();
    assert!(
        matches!(e, NumbersError::MissingObject { id: 99, .. }),
        "{e}"
    );
    assert!(e.what().contains("sheet 1, table 1"), "{}", e.what());
}

#[test]
fn reference_loops_end() {
    // The table info names itself as its table model.
    let doc = Doc {
        table_model_ref: 3,
        ..Doc::default()
    };
    let e = read(&doc.bytes()).unwrap_err();
    assert!(
        matches!(e, NumbersError::WrongObjectType { id: 3, .. }),
        "{e}"
    );

    // The string list names itself as one of its segments.
    let doc = Doc {
        string_list_extra: Some(Msg::new().msg(4, reference(6))),
        ..Doc::default()
    };
    let e = read(&doc.bytes()).unwrap_err();
    assert!(
        matches!(e, NumbersError::WrongObjectType { id: 6, .. }),
        "{e}"
    );

    // The document lists itself as a sheet: not a sheet, so skipped.
    let mut objects = Doc::default().objects();
    objects[0] = archive(1, 1, &Msg::new().msg(1, reference(1)));
    let doc = read(&document(&objects)).unwrap();
    assert!(doc.sheets().is_empty());
}

#[test]
fn objects_referred_to_many_times() {
    // The same sheet twice: read once, with a warning.
    let mut objects = Doc::default().objects();
    objects[0] = archive(1, 1, &Msg::new().msg(1, reference(2)).msg(1, reference(2)));
    let doc = read(&document(&objects)).unwrap();
    assert_eq!(doc.sheets().len(), 1);
    assert_eq!(doc.warnings().len(), 1);

    // One tile listed 200,000 times: the work of decoding it is bounded.
    let doc = Doc {
        tile_repeats: 200_000,
        ..Doc::default()
    };
    let mut limits = Limits::default();
    limits.max_total_bytes = 2 << 20;
    let e = Document::from_bytes_with_limits(&doc.bytes(), &limits).unwrap_err();
    assert!(matches!(e, NumbersError::TooLarge { .. }), "{e}");
}

#[test]
fn deeply_nested_formulas_are_cut_off() {
    // 1000 thunk arrays inside each other.
    let mut inner = Msg::new();
    for _ in 0..1000 {
        inner = Msg::new().msg(1, ast_node(26).msg(14, inner));
    }
    let doc = Doc {
        formulas: vec![(1, Msg::new().msg(1, inner))],
        ..Doc::default()
    };
    let e = read(&doc.bytes()).unwrap_err();
    assert!(matches!(e, NumbersError::Damaged { .. }), "{e}");
}

#[test]
fn table_size_limits() {
    let doc = Doc {
        rows: u64::from(u32::MAX),
        ..Doc::default()
    };
    let e = read(&doc.bytes()).unwrap_err();
    assert!(matches!(e, NumbersError::TooLarge { .. }), "{e}");

    let mut limits = Limits::default();
    limits.max_cells = 1;
    let e = Document::from_bytes_with_limits(&Doc::default().bytes(), &limits).unwrap_err();
    assert!(matches!(e, NumbersError::TooLarge { .. }), "{e}");

    let mut limits = Limits::default();
    limits.max_objects = 3;
    let e = Document::from_bytes_with_limits(&Doc::default().bytes(), &limits).unwrap_err();
    assert!(matches!(e, NumbersError::TooLarge { .. }), "{e}");
}

#[test]
fn broken_cells_become_warnings() {
    let doc = Doc {
        rows_msgs: vec![
            // A row far outside the table.
            row(70_000, &[Some(text_cell(1))]),
            row(
                0,
                &[
                    // A text key that is not in the string list.
                    Some(text_cell(42)),
                    // Too short to be a cell.
                    Some(vec![5, 3, 0]),
                ],
            ),
            row(
                1,
                &[
                    // An unknown storage version, and an unknown cell type.
                    Some(cell(9, 0, &[])),
                    Some(cell(77, 0, &[])),
                ],
            ),
        ],
        ..Doc::default()
    };
    let doc = read(&doc.bytes()).unwrap();
    let t = &doc.sheets()[0].tables()[0];
    assert_eq!(t.non_empty_rows().count(), 0);
    assert_eq!(doc.warnings().len(), 5, "{:#?}", doc.warnings());
    assert_eq!(doc.warnings()[1].cell, Some((0, 0)));
    assert_eq!(
        doc.warnings()[1].to_string(),
        "sheet 1, table 1, cell A1: the text is missing from the string list"
    );
}

#[test]
fn cell_offsets_outside_the_buffer() {
    let r = Msg::new()
        .uint(1, 0)
        .uint(2, 1)
        .bytes(3, b"")
        .bytes(4, b"")
        .bytes(6, &text_cell(1))
        .bytes(7, &[0x00, 0x40, 0x00, 0x00]);
    let doc = Doc {
        rows_msgs: vec![r],
        ..Doc::default()
    };
    let doc = read(&doc.bytes()).unwrap();
    assert!(doc.sheets()[0].tables()[0].cell(0, 0).is_none());
    assert!(!doc.warnings().is_empty());
}

#[test]
fn bad_formulas_are_reported_not_fatal() {
    let doc = Doc {
        formulas: vec![(1, formula(&[ast_node(1)]))],
        ..Doc::default()
    };
    let doc = read(&doc.bytes()).unwrap();
    let c = doc.sheets()[0].tables()[0].cell(0, 1).unwrap();
    assert!(
        matches!(c.formula(), Some(Formula::NotDecoded(_))),
        "{:?}",
        c.formula()
    );
    assert_eq!(doc.warnings()[0].cell, Some((0, 1)));

    // Too many nodes.
    let nodes = vec![ast_node(22); 50];
    let doc = Doc {
        formulas: vec![(1, formula(&nodes))],
        ..Doc::default()
    };
    let mut limits = Limits::default();
    limits.max_formula_nodes = 10;
    let doc = Document::from_bytes_with_limits(&doc.bytes(), &limits).unwrap();
    let c = doc.sheets()[0].tables()[0].cell(0, 1).unwrap();
    assert!(matches!(c.formula(), Some(Formula::NotDecoded(_))));

    // 1001 strings of 100 bytes joined by & go over the 64 KiB text limit.
    let mut nodes = vec![ast_node(19).bytes(6, &[b'x'; 100])];
    for _ in 0..1000 {
        nodes.push(ast_node(19).bytes(6, &[b'x'; 100]));
        nodes.push(ast_node(6));
    }
    let doc = Doc {
        formulas: vec![(1, formula(&nodes))],
        ..Doc::default()
    };
    let doc = read(&doc.bytes()).unwrap();
    let c = doc.sheets()[0].tables()[0].cell(0, 1).unwrap();
    assert!(matches!(c.formula(), Some(Formula::NotDecoded(_))));
}

#[test]
fn bad_merges_are_skipped() {
    let doc = Doc {
        // A1:B1, and one far outside the table.
        merges: vec![(0, 2 << 16 | 1), (0x0100_0100, 0x0001_0001)],
        ..Doc::default()
    };
    let doc = read(&doc.bytes()).unwrap();
    let t = &doc.sheets()[0].tables()[0];
    assert_eq!(t.merged_ranges().len(), 1);
    assert_eq!(t.merged_ranges()[0].to_string(), "A1:B1");
    assert!(t.cell(0, 1).is_none(), "covered by the merge");
    assert_eq!(doc.warnings().len(), 1);
}

#[test]
fn bad_property_lists_are_ignored() {
    let objects = Doc::default().objects().concat();
    let bytes = stored(&[
        ("Index/Document.iwa", iwa(&objects)),
        ("Metadata/Properties.plist", b"bplist00\xff\xff".to_vec()),
        (
            "Metadata/BuildVersionHistory.plist",
            b"<plist><array><string>".to_vec(),
        ),
    ]);
    let doc = read(&bytes).unwrap();
    assert_eq!(doc.numbers_version(), None);
    assert!(doc.build_history().is_empty());
}

#[test]
fn password_parts_are_detected() {
    let bytes = stored(&[(".iwpv2", vec![0; 104]), ("Index/Document.iwa", Vec::new())]);
    assert!(matches!(read(&bytes), Err(NumbersError::PasswordProtected)));
}
