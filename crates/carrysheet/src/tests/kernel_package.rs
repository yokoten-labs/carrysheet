use crate::commands::engine::SaveReport;
use crate::kernel::package::*;

/// A part to zip: its name, its text, and its raw deflate stream, if
/// any.
type Part<'a> = (&'a str, &'a [u8], Option<&'a [u8]>);

/// A zip package of `parts`: each stored, or with the raw deflate
/// stream given (which must unpack to the text).
fn zip_of(parts: &[Part<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, text, deflated) in parts {
        let (method, body): (u16, &[u8]) = match deflated {
            Some(d) => (8, d),
            None => (0, text),
        };
        let mut crc = flate2::Crc::new();
        crc.update(text);
        let offset = out.len() as u32;
        let mut fields = Vec::new();
        fields.extend(20u16.to_le_bytes()); // version needed
        fields.extend(0u16.to_le_bytes()); // flags
        fields.extend(method.to_le_bytes());
        fields.extend(0u32.to_le_bytes()); // time, date
        fields.extend(crc.sum().to_le_bytes());
        fields.extend((body.len() as u32).to_le_bytes());
        fields.extend((text.len() as u32).to_le_bytes());
        fields.extend((name.len() as u16).to_le_bytes());
        fields.extend(0u16.to_le_bytes()); // extra
        out.extend(b"PK\x03\x04");
        out.extend(&fields);
        out.extend(name.as_bytes());
        out.extend(body);
        central.extend(b"PK\x01\x02");
        central.extend(20u16.to_le_bytes()); // version made by
        central.extend(&fields);
        central.extend([0u8; 10]); // comment, disk, attributes
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let at = out.len() as u32;
    out.extend(&central);
    out.extend(b"PK\x05\x06");
    out.extend([0u8; 4]);
    out.extend((parts.len() as u16).to_le_bytes());
    out.extend((parts.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(at.to_le_bytes());
    out.extend([0u8; 2]);
    out
}

fn package(parts: &[(&str, &str)]) -> Vec<u8> {
    let parts: Vec<Part<'_>> = parts
        .iter()
        .map(|(n, t)| (*n, t.as_bytes(), None))
        .collect();
    zip_of(&parts)
}

#[test]
fn content_lost_inside_rewritten_parts_is_reported() {
    let sheet = r#"<worksheet><sheetPr codeName="Sheet1"/><sheetData><row><c r="B2"><f t="dataTable" ref="B2:B3" r1="A1"/><v>3</v></c><c r="C2"><f>_xlfn.NEWFN(1)</f><v>7</v></c></row></sheetData><x:hyperlinks xmlns:x="m"><x:hyperlink ref="A1" display="Go &amp; see"/></x:hyperlinks></worksheet>"#;
    let source = package(&[
        (
            "xl/workbook.xml",
            r#"<workbook><fileSharing userName="Ann"/><workbookPr codeName="ThisWorkbook"/></workbook>"#,
        ),
        ("xl/worksheets/sheet1.xml", sheet),
        (
            "xl/sharedStrings.xml",
            "<sst><si><r><rPr><b/></rPr><t>a</t></r></si></sst>",
        ),
        (
            "xl/persons/person.xml",
            r#"<personList><person displayName="Ann" id="{1}" userId="a@x" providerId="AD"/></personList>"#,
        ),
    ]);
    // The same content, written differently: nothing is reported.
    let same = package(&[
        (
            "xl/workbook.xml",
            r#"<workbook><fileSharing userName='Ann'/><workbookPr date1904="0" codeName="ThisWorkbook"/></workbook>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            &sheet.replace("x:", "").replace("&amp;", "&#38;"),
        ),
        (
            "xl/sharedStrings.xml",
            "<sst><si><r><rPr><b/></rPr><t>a</t></r></si></sst>",
        ),
        (
            "xl/persons/person.xml",
            r#"<personList><person displayName="Ann" id="{9}" userId="a@x" providerId="AD"/></personList>"#,
        ),
    ]);
    let mut r = SaveReport::new(crate::commands::engine::FileFormat::Xlsx);
    compare(&source, &same, &mut r);
    assert!(r.lost.is_empty(), "{:?}", r.lost);

    // Every part rewritten without it: each is reported, by name.
    let lossy = package(&[
        ("xl/workbook.xml", "<workbook><workbookPr/></workbook>"),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet><sheetData><row><c r="B2"><v>3</v></c><c r="C2" t="e"><f>NEWFN(1)</f><v>#NAME?</v></c></row></sheetData><hyperlinks><hyperlink ref="A1" display="A1"/></hyperlinks></worksheet>"#,
        ),
        ("xl/sharedStrings.xml", "<sst><si><t>a</t></si></sst>"),
        (
            "xl/persons/person.xml",
            r#"<personList><person displayName="Ann" id="{1}" userId="Ann" providerId="None"/></personList>"#,
        ),
    ]);
    let mut r = SaveReport::new(crate::commands::engine::FileFormat::Xlsx);
    compare(&source, &lossy, &mut r);
    let lost: Vec<&str> = r.lost.iter().map(|l| l.feature.as_str()).collect();
    for feature in [
        "file sharing settings",
        "workbook code name (VBA)",
        "sheet code names (VBA)",
        "what-if data tables",
        "hyperlink text and tips",
        "rich text in cells",
        "people of threaded comments (user ids)",
        "formula results (they would become #NAME?)",
        "newer Excel function names (_xlfn.)",
    ] {
        assert!(lost.contains(&feature), "{feature}: {lost:?}");
    }
}

#[test]
fn a_package_too_large_to_search_needs_confirm_loss() {
    // Round 2, N4: what the gate could not search is a loss, not a note.
    let sheet = r#"<worksheet><sheetPr codeName="Sheet1"/><sheetData/></worksheet>"#;
    let parts = [
        ("xl/workbook.xml", "<workbook/>"),
        ("xl/worksheets/sheet1.xml", sheet),
    ];
    let source = package(&parts);
    let small = Caps {
        part: 16,
        total: 16,
    };
    let mut r = SaveReport::new(crate::commands::engine::FileFormat::Xlsx);
    compare_within(&source, &source, &mut r, small);
    let lost: Vec<&str> = r.lost.iter().map(|l| l.feature.as_str()).collect();
    assert!(
        lost.iter()
            .any(|l| l.contains("content inside rewritten parts")),
        "{lost:?}"
    );
    assert!(lost.iter().any(|l| l.contains("not checked")), "{lost:?}");
    assert!(lost.iter().all(|l| !l.contains("slowly")), "{lost:?}");
    assert!(lost.iter().any(|l| l.contains("too large")), "{lost:?}");
    // With room to search, the same package loses nothing.
    let mut r = SaveReport::new(crate::commands::engine::FileFormat::Xlsx);
    compare(&source, &source, &mut r);
    assert!(r.lost.is_empty(), "{:?}", r.lost);
}

/// Raw deflate of `text` in stored blocks, with `filler` bytes of
/// empty stored blocks (5 bytes each, no output) after the first
/// `split` bytes of output.
fn with_empty_run(text: &[u8], split: usize, filler: usize) -> Vec<u8> {
    fn stored(out: &mut Vec<u8>, data: &[u8], last: bool) {
        let chunks: Vec<&[u8]> = data.chunks(65_535).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            out.push(u8::from(last && i + 1 == chunks.len()));
            let n = chunk.len() as u16;
            out.extend(n.to_le_bytes());
            out.extend((!n).to_le_bytes());
            out.extend(*chunk);
        }
    }
    let mut out = Vec::new();
    stored(&mut out, &text[..split], false);
    for _ in 0..filler / 5 {
        out.extend(b"\x00\x00\x00\xff\xff");
    }
    stored(&mut out, &text[split..], true);
    out
}

#[test]
fn a_long_run_of_empty_blocks_between_outputs_is_refused_and_named() {
    // Issue 80: a sheet part no relationship names (only this check
    // unpacks it) gives 192 KiB of output, then 40 KiB of empty
    // blocks, then the rest. Its total stays within what its output
    // earns (a quarter more input than output), so only the cap on a
    // run without output refuses it, far below the 64 MiB part cap.
    let mut sheet = b"<worksheet><sheetData/><!--".to_vec();
    sheet.resize(256 << 10, b'x');
    sheet.extend(b"--></worksheet>");
    let packed = with_empty_run(&sheet, 192 << 10, 40 << 10);
    assert!(packed.len() as u64 <= sheet.len() as u64 * 5 / 4);
    let workbook = b"<workbook/>".as_slice();
    let source = zip_of(&[
        ("xl/workbook.xml", workbook, None),
        (
            "xl/worksheets/sheet9.xml",
            sheet.as_slice(),
            Some(packed.as_slice()),
        ),
    ]);
    // Read whole, it is refused for its shape, and nothing else.
    let mut p = Package::open(&source).unwrap();
    let e = p.parts["xl/worksheets/sheet9.xml"].1;
    assert!(p.unpack(e).is_none());
    assert_eq!(
        p.unsearched.iter().copied().collect::<Vec<_>>(),
        [Unsearched::Slow]
    );

    let mut r = SaveReport::new(crate::commands::engine::FileFormat::Xlsx);
    compare(&source, &source, &mut r);
    let lost: Vec<&str> = r.lost.iter().map(|l| l.feature.as_str()).collect();
    assert!(!lost.is_empty(), "{lost:?}");
    for l in &lost {
        assert!(l.contains("not checked"), "{l}");
        assert!(l.contains("unpacks far more slowly"), "{l}");
        assert!(!l.contains("too large"), "{l}");
    }

    // The same part without the run is searched, and loses nothing.
    let plain = zip_of(&[
        ("xl/workbook.xml", workbook, None),
        (
            "xl/worksheets/sheet9.xml",
            sheet.as_slice(),
            Some(with_empty_run(&sheet, 192 << 10, 0).as_slice()),
        ),
    ]);
    let mut r = SaveReport::new(crate::commands::engine::FileFormat::Xlsx);
    compare(&plain, &plain, &mut r);
    assert!(r.lost.is_empty(), "{:?}", r.lost);
}

#[test]
fn a_package_that_cannot_be_listed_says_why_and_what_to_do() {
    let good = package(&[("xl/workbook.xml", "<workbook/>")]);
    let mut r = SaveReport::new(crate::commands::engine::FileFormat::Xlsx);
    compare(b"not a zip at all, but long enough", &good, &mut r);
    let lost: Vec<&str> = r.lost.iter().map(|l| l.feature.as_str()).collect();
    assert_eq!(lost.len(), 1, "{lost:?}");
    assert!(
        lost[0].contains(
            "could not be listed because its zip structure cannot be read (the zip is damaged: "
        ),
        "{}",
        lost[0]
    );
    assert!(lost[0].contains("confirm_loss"), "{}", lost[0]);
    let mut r = SaveReport::new(crate::commands::engine::FileFormat::Xlsx);
    compare(&good, b"PK", &mut r);
    assert!(
        r.lost[0].feature.contains("in the file to be written"),
        "{:?}",
        r.lost
    );
}

#[test]
fn tags_are_read_with_local_names_and_decoded_values() {
    let mut seen = vec![];
    for_each_tag(
        br#"<?xml version="1.0"?><!-- <b x="1"/> --><a:t q='x>y' r="&lt;&#65;&#x42;"/><![CDATA[<c/>]]></a:t>"#,
        &mut |name, attrs| {
            seen.push(format!(
                "{name}:{}",
                attrs
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        },
    );
    assert_eq!(seen, vec!["t:q=x>y,r=<AB"]);
}

#[test]
fn counting_needles() {
    assert_eq!(count(b"<a ><a ><b>", b"<a "), 2);
    assert_eq!(count(b"", b"<a "), 0);
    assert_eq!(count(b"aaaa", b"aa"), 2);
}

#[test]
fn part_names_match_features() {
    let label = |name: &str| PART_FEATURES.iter().find(|(_, t)| t(name)).map(|(l, _)| *l);
    assert_eq!(label("xl/charts/chart1.xml"), Some("charts"));
    assert_eq!(label("xl/charts/_rels/chart1.xml.rels"), None);
    assert_eq!(label("xl/vbaproject.bin"), Some("VBA project"));
    assert_eq!(
        label("xl/drawings/vmldrawing1.vml"),
        Some("notes and form controls (VML)")
    );
    assert_eq!(label("xl/drawings/drawing1.xml"), Some("drawings"));
    assert_eq!(label("customxml/itemprops1.xml"), None);
    assert_eq!(label("xl/worksheets/sheet1.xml"), None);
}

#[test]
fn macros_are_found_by_part_name_or_said_unchecked() {
    let vba = zip_of(&[("xl/vbaProject.bin", b"x", None)]);
    let said = macros(&vba, true).unwrap();
    assert!(
        said.contains("VBA project") && said.contains("keeps them"),
        "{said}"
    );
    let said = macros(&vba, false).unwrap();
    assert!(
        said.contains("never runs") && said.contains("may not keep"),
        "{said}"
    );

    let xlm = zip_of(&[("xl/macrosheets/sheet1.xml", b"<x/>", None)]);
    let said = macros(&xlm, true).unwrap();
    assert!(said.contains("macro sheets (Excel 4)"), "{said}");

    let none = zip_of(&[("xl/worksheets/sheet1.xml", b"<x/>", None)]);
    assert_eq!(macros(&none, true), None);

    // Excel finds the project by content type, so the part name is free.
    let renamed = zip_of(&[
        (
            "[Content_Types].xml",
            br#"<Types><Override PartName="/xl/project1.bin" ContentType="application/vnd.ms-office.VbaProject"/><Override PartName="/xl/x/m.xml" ContentType="application/vnd.ms-excel.intlmacrosheet+xml"/></Types>"#,
            None,
        ),
        ("xl/project1.bin", b"x", None),
        ("xl/x/m.xml", b"<x/>", None),
    ]);
    let said = macros(&renamed, true).unwrap();
    assert!(
        said.contains("VBA project") && said.contains("macro sheets (Excel 4)"),
        "{said}"
    );
    let plain = zip_of(&[(
        "[Content_Types].xml",
        br#"<Types><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#,
        None,
    )]);
    assert_eq!(macros(&plain, true), None);

    // Content types a byte search cannot read are said unchecked.
    let referenced = zip_of(&[
        (
            "[Content_Types].xml",
            br#"<Types><Override PartName="/xl/project1.bin" ContentType="application/vnd.ms-office.vba&#x50;roject"/></Types>"#,
            None,
        ),
        ("xl/project1.bin", b"x", None),
    ]);
    let said = macros(&referenced, true).unwrap();
    assert!(said.starts_with("not checked:"), "{said}");
    let utf16: Vec<u8> = [0xFF, 0xFE]
        .into_iter()
        .chain("<Types/>".bytes().flat_map(|b| [b, 0]))
        .collect();
    let wide = zip_of(&[
        ("[Content_Types].xml", utf16.as_slice(), None),
        ("xl/project1.bin", b"x", None),
    ]);
    let said = macros(&wide, true).unwrap();
    assert!(said.starts_with("not checked:"), "{said}");

    let said = macros(b"not a zip", true).unwrap();
    assert!(
        said.starts_with("not checked:") && said.contains("never runs"),
        "{said}"
    );
}
