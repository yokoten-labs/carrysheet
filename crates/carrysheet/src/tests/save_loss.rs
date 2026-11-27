use crate::save::loss::*;
use crate::save::{FileFormat, SaveReport};

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
fn parts_are_compared_from_the_zip_directory_only() {
    let source = package(&[
        ("xl/workbook.xml", "<workbook/>"),
        ("xl/charts/chart1.xml", "<c/>"),
        ("xl/charts/chart2.xml", "<c/>"),
        ("xl/calcChain.xml", "<calcChain/>"),
        ("_xmlsignatures/sig1.xml", "<s/>"),
    ]);
    let output = package(&[
        ("xl/workbook.xml", "<workbook date1904=\"0\"/>"),
        ("xl/charts/chart1.xml", "<c/>"),
        ("_xmlsignatures/sig1.xml", "<s/>"),
        ("xl/styles.xml", "<styleSheet/>"),
    ]);
    let mut r = SaveReport::new(FileFormat::Xlsx);
    compare(&source, &output, &mut r);
    let parts = r.parts.unwrap();
    let counts = (parts.identical, parts.rewritten, parts.dropped, parts.added);
    assert_eq!(counts, (2, 1, 2, 1), "{parts:?}");
    assert_eq!(r.dropped_parts, vec!["\"xl/charts/chart2.xml\"".to_owned()]);
    assert_eq!(r.lost.len(), 1, "{:?}", r.lost);
    let l = &r.lost[0];
    let lost = (l.feature.as_str(), l.before, l.after);
    assert_eq!(lost, ("charts", 2, 1));
    let notes = r.notes.join("; ");
    assert!(notes.contains("Excel rebuilds it"), "{notes}");
    assert!(notes.contains("digital signature"), "{notes}");
    // The same package loses nothing, and nothing inside a part counts.
    let mut r = SaveReport::new(FileFormat::Xlsx);
    compare(&source, &source, &mut r);
    assert!(!r.loses_something(), "{r:?}");
}

#[test]
fn a_package_that_cannot_be_listed_says_why_and_what_to_do() {
    let good = package(&[("xl/workbook.xml", "<workbook/>")]);
    let mut r = SaveReport::new(FileFormat::Xlsx);
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
    let mut r = SaveReport::new(FileFormat::Xlsx);
    compare(&good, b"PK", &mut r);
    assert!(
        r.lost[0].feature.contains("in the file to be written"),
        "{:?}",
        r.lost
    );
}

#[test]
fn part_names_match_features() {
    let label = |name: &str| kind(PART_FEATURES, name);
    assert_eq!(label("customxml/item1.xml"), Some("custom XML"));
    assert_eq!(label("xl/model/item.data"), Some("data model"));
    assert_eq!(label("xl/comments1.xml"), Some("comments"));
    assert_eq!(label("xl/media/image1.png"), Some("images"));
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

    // Content types padded far beyond their size are never unpacked (issue 80).
    let types = br#"<Types><Override PartName="/xl/p.bin" ContentType="application/vnd.ms-office.vbaProject"/></Types>"#;
    let padded = with_empty_run(types, 10, 64 << 10);
    let padded = zip_of(&[("[Content_Types].xml", types, Some(padded.as_slice()))]);
    let said = macros(&padded, true).unwrap();
    assert!(said.starts_with("not checked:"), "{said}");
    let plain = with_empty_run(types, 10, 0);
    let plain = zip_of(&[("[Content_Types].xml", types, Some(plain.as_slice()))]);
    let said = macros(&plain, true).unwrap();
    assert!(said.contains("VBA project"), "{said}");

    let said = macros(b"not a zip", true).unwrap();
    assert!(
        said.starts_with("not checked:") && said.contains("never runs"),
        "{said}"
    );
}
