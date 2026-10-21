//! What a save keeps, and what it reports (review round 1 of issue 45: H4,
//! M4, M5, L3). Record 0003: a save never loses content silently.

mod common;

use carrysheet_commands::ErrorCode;
use common::*;
use serde_json::json;

const PERSON_REL: &str = "http://schemas.microsoft.com/office/2017/10/relationships/person";
const THREADED_REL: &str =
    "http://schemas.microsoft.com/office/2017/10/relationships/threadedComment";

/// A workbook with the content round 1 found dropped on save.
fn features_book() -> Vec<u8> {
    let sheet = sheet_xml(
        r#"<row r="1"><c r="A1"><v>2</v></c><c r="B1"><f>A1*10</f><v>20</v></c><c r="C1" t="s"><v>0</v></c><c r="D1" t="s"><v>1</v></c><c r="E1"><f>_xlfn.FUTUREFN(A1)</f><v>7</v></c><c r="F1" t="str"><f>_xlfn.CONCAT("a","b")</f><v>ab</v></c></row><row r="2"><c r="A2"><v>3</v></c><c r="B2"><f t="dataTable" ref="B2:B3" dt2D="0" dtr="0" r1="A1"/><v>30</v></c></row><row r="3"><c r="A3"><v>4</v></c><c r="B3"><v>40</v></c></row>"#,
        r#"<sheetPr codeName="Sheet1"/>"#,
        r#"<scenarios current="0"><scenario name="Hi" locked="1" count="1" user="Ann"><inputCells r="A1" val="9"/></scenario></scenarios><autoFilter ref="A1:B3"><filterColumn colId="0"><filters><filter val="2"/></filters></filterColumn></autoFilter><phoneticPr fontId="1" type="noConversion"/><hyperlinks><hyperlink ref="A3" location="S!A1" display="Go home" tooltip="tip"/></hyperlinks><ignoredErrors><ignoredError sqref="A1:A3" numberStoredAsText="1"/></ignoredErrors>"#,
    );
    let mut book = Book::with_sheet(&sheet);
    book.workbook = format!(
        r#"<?xml version="1.0"?><workbook {NS}><fileSharing readOnlyRecommended="1" userName="Ann"/><workbookPr codeName="ThisWorkbook"/><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets></workbook>"#
    );
    book.strings = Some(format!(
        r#"<?xml version="1.0"?><sst {NS}><si><r><rPr><b/></rPr><t>Bold</t></r><r><t xml:space="preserve"> plain</t></r></si><si><t>x</t><rPh sb="0" eb="1"><t>ex</t></rPh><phoneticPr fontId="1"/></si></sst>"#
    ));
    book.sheet_rels = vec![
        ("comments".into(), "../comments1.xml".into(), false),
        (
            THREADED_REL.into(),
            "../threadedComments/threadedComment1.xml".into(),
            false,
        ),
    ];
    book.workbook_rels = vec![(PERSON_REL.into(), "persons/person.xml".into())];
    book.content_types = vec![
        (
            "xl/comments1.xml".into(),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.comments+xml".into(),
        ),
        (
            "xl/threadedComments/threadedComment1.xml".into(),
            "application/vnd.ms-excel.threadedcomments+xml".into(),
        ),
        (
            "xl/persons/person.xml".into(),
            "application/vnd.ms-excel.person+xml".into(),
        ),
    ];
    book.extra = vec![
        entry(
            "xl/comments1.xml",
            format!(
                r#"<?xml version="1.0"?><comments {NS}><authors><author>Ann</author><author>tc={{11111111-1111-1111-1111-111111111111}}</author></authors><commentList><comment ref="A1" authorId="0"><text><r><rPr><b/></rPr><t>Ann:</t></r><r><t xml:space="preserve"> look here</t></r></text></comment><comment ref="A2" authorId="1"><text><t>[Threaded comment] hi</t></text></comment></commentList></comments>"#
            ),
        ),
        entry(
            "xl/threadedComments/threadedComment1.xml",
            r#"<?xml version="1.0"?><ThreadedComments xmlns="http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments"><threadedComment ref="A2" dT="2026-01-01T00:00:00.00" personId="{22222222-2222-2222-2222-222222222222}" id="{11111111-1111-1111-1111-111111111111}"><text>hi</text></threadedComment></ThreadedComments>"#,
        ),
        entry(
            "xl/persons/person.xml",
            r#"<?xml version="1.0"?><personList xmlns="http://schemas.microsoft.com/office/spreadsheetml/2018/threadedcomments"><person displayName="Ann Lee" id="{22222222-2222-2222-2222-222222222222}" userId="ann@example.com" providerId="AD"/></personList>"#,
        ),
    ];
    book.bytes()
}

#[test]
fn a_save_keeps_what_round_one_found_dropped() {
    let dir = scratch("keep");
    let src = dir.join("features.xlsx");
    std::fs::write(&src, features_book()).unwrap();
    let out = dir.join("out.xlsx");
    let done = call("convert", json!({ "path": p(&src), "output": p(&out) })).unwrap();
    assert_eq!(done["save"]["lost"], json!([]), "{done:?}");

    let wb = part_text(&out, "xl/workbook.xml");
    assert!(
        wb.contains(r#"<fileSharing readOnlyRecommended="1" userName="Ann"/>"#),
        "{wb}"
    );
    assert!(wb.contains(r#"codeName="ThisWorkbook""#), "{wb}");

    let sheet = part_text(&out, "xl/worksheets/sheet1.xml");
    for kept in [
        r#"<sheetPr codeName="Sheet1">"#,
        r#"<f t="dataTable" ref="B2:B3" dt2D="0" dtr="0" r1="A1"/><v>30</v>"#,
        r#"<scenario name="Hi" locked="1" count="1" user="Ann"><inputCells r="A1" val="9"/></scenario>"#,
        r#"<filter val="2"/>"#,
        r#"<phoneticPr fontId="1" type="noConversion"/>"#,
        r#"<ignoredError sqref="A1:A3" numberStoredAsText="1"/>"#,
        r#"display="Go home""#,
        // A newer function GridCraft does not know keeps its prefix and its
        // saved value; it is never rewritten to #NAME?.
        r#"<f>_xlfn.FUTUREFN(A1)</f><v>7</v>"#,
        r#"_xlfn.CONCAT("a","b")"#,
    ] {
        assert!(sheet.contains(kept), "{kept} missing from {sheet}");
    }
    assert!(!sheet.contains("#NAME?"), "{sheet}");

    let strings = part_text(&out, "xl/sharedStrings.xml");
    assert!(strings.contains("<rPr><b/></rPr><t>Bold</t>"), "{strings}");
    assert!(strings.contains(r#"<rPh sb="0" eb="1">"#), "{strings}");
    let comments = part_text(&out, "xl/comments1.xml");
    assert!(
        comments.contains("<rPr><b/></rPr><t>Ann:</t>"),
        "{comments}"
    );
    let persons = part_text(&out, "xl/persons/person.xml");
    assert!(
        persons.contains(r#"displayName="Ann Lee""#)
            && persons.contains(r#"userId="ann@example.com" providerId="AD""#),
        "{persons}"
    );

    // The value is the file's, and the report says so.
    let page = call("read", json!({ "path": p(&out), "range": "E1" })).unwrap();
    assert_eq!(page["rows"][0]["cells"][0], json!(7.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn moved_rows_report_what_no_longer_fits() {
    // Scenarios, ignored-error ranges and data tables name cells, and
    // GridCraft does not move them: after inserting rows they are not
    // written, and the report lists them, so the save needs confirm_loss.
    let dir = scratch("moved");
    let src = dir.join("features.xlsx");
    std::fs::write(&src, features_book()).unwrap();
    let out = dir.join("out.xlsx");
    let ops = json!([{ "op": "insert_rows", "target": "1:1" }]);
    let e = call(
        "edit",
        json!({ "path": p(&src), "ops": ops, "output": p(&out) }),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::ConfirmationRequired, "{e:?}");
    for feature in ["scenarios", "ignored error flags", "what-if data tables"] {
        assert!(e.what.contains(feature), "{feature}: {}", e.what);
    }
    assert!(!out.exists());
    let done = call(
        "edit",
        json!({ "path": p(&src), "ops": ops, "output": p(&out), "confirm_loss": true }),
    )
    .unwrap();
    assert!(lost(&done).contains(&"scenarios".to_owned()), "{done:?}");

    // A fill over a data table's anchor drops that table only.
    let ops = json!([{ "op": "set_value", "target": "B2", "value": 1 }]);
    let out = dir.join("filled.xlsx");
    let dry = call(
        "edit",
        json!({ "path": p(&src), "ops": ops, "output": p(&out), "dry_run": true }),
    )
    .unwrap();
    assert_eq!(
        lost(&dry),
        vec!["what-if data tables".to_owned()],
        "{dry:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csv_escapes_text_that_could_run_as_a_formula() {
    let dir = scratch("csv-escape");
    let src = dir.join("data.xlsx");
    let rows = r#"<row r="1"><c r="A1" t="inlineStr"><is><t>=HYPERLINK("http://x")</t></is></c><c r="B1" t="inlineStr"><is><t>+1</t></is></c><c r="C1"><v>-5</v></c><c r="D1" t="inlineStr"><is><t>@SUM(1)</t></is></c><c r="E1" t="inlineStr"><is><t>plain</t></is></c></row>"#;
    std::fs::write(&src, Book::new(rows).bytes()).unwrap();
    let out = dir.join("out.csv");
    let done = call("convert", json!({ "path": p(&src), "output": p(&out) })).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        text,
        "\"'=HYPERLINK(\"\"http://x\"\")\",'+1,-5,'@SUM(1),plain\r\n"
    );
    let notes = done["save"]["notes"].to_string();
    assert!(
        notes.contains("3 cells") && notes.contains("--csv-raw"),
        "{notes}"
    );

    let raw = call(
        "convert",
        json!({ "path": p(&src), "output": p(&out), "overwrite": true, "confirm": true, "csv_raw": true }),
    )
    .unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        text,
        "\"=HYPERLINK(\"\"http://x\"\")\",+1,-5,@SUM(1),plain\r\n"
    );
    assert!(!raw["save"]["notes"].to_string().contains("--csv-raw"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overview_cuts_long_text_and_says_so() {
    let dir = scratch("cut");
    let src = dir.join("long.xlsx");
    let long = "y".repeat(10_000);
    let rows = format!(
        r#"<row r="1"><c r="A1" t="inlineStr"><is><t>{long}</t></is></c><c r="B1" t="inlineStr"><is><t>short</t></is></c></row>"#
    );
    std::fs::write(&src, Book::new(&rows).bytes()).unwrap();
    let o = call("overview", json!({ "path": p(&src) })).unwrap();
    let header = &o["sheets"][0]["header"];
    let first = header["cells"][0].as_str().unwrap();
    assert!(first.len() < long.len(), "{}", first.len());
    assert_eq!(header["cells"][1], "short");
    let cut = header["cut"].to_string();
    assert!(cut.contains("S!A1"), "{o:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn links_to_other_workbooks_never_name_home_folders() {
    // A link that would need the folders above the source's to be written
    // relative to the output keeps its original target (review L3).
    let dir = scratch("links");
    let deep = dir.join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    let mut book = Book::new(r#"<row r="1"><c r="A1"><f>[1]S!A1</f><v>5</v></c></row>"#);
    book.workbook = format!(
        r#"<?xml version="1.0"?><workbook {NS}><sheets><sheet name="S" sheetId="1" r:id="rId1"/></sheets><externalReferences><externalReference r:id="rId2"/></externalReferences></workbook>"#
    );
    book.workbook_rels = vec![(
        "externalLink".into(),
        "externalLinks/externalLink1.xml".into(),
    )];
    book.content_types = vec![(
        "xl/externalLinks/externalLink1.xml".into(),
        "application/vnd.openxmlformats-officedocument.spreadsheetml.externalLink+xml".into(),
    )];
    book.extra = vec![
        entry(
            "xl/externalLinks/externalLink1.xml",
            format!(
                r#"<?xml version="1.0"?><externalLink {NS}><externalBook r:id="rId1"><sheetNames><sheetName val="S"/></sheetNames></externalBook></externalLink>"#
            ),
        ),
        entry(
            "xl/externalLinks/_rels/externalLink1.xml.rels",
            r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLinkPath" Target="data.xlsx" TargetMode="External"/></Relationships>"#,
        ),
    ];
    let src = deep.join("book.xlsx");
    std::fs::write(&src, book.bytes()).unwrap();
    // From <scratch>/elsewhere the file would be "../a/b/data.xlsx": names
    // of the folder the book was read from. The link is kept as written.
    let other = dir.join("elsewhere");
    std::fs::create_dir_all(&other).unwrap();
    let out = other.join("out.xlsx");
    let done = call(
        "convert",
        json!({ "path": p(&src), "output": p(&out), "confirm_loss": true }),
    )
    .unwrap();
    let rels = part_text(&out, "xl/externalLinks/_rels/externalLink1.xml.rels");
    let home = std::env::temp_dir().display().to_string();
    assert!(!rels.contains(&home), "{rels}");
    assert!(rels.contains(r#"Target="data.xlsx""#), "{rels}");
    assert!(
        done["save"]["notes"]
            .to_string()
            .contains("keep their paths"),
        "{done:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sheet_protection_keeps_objects_and_scenarios_as_read() {
    // Round 2, (g): `objects` and `scenarios` default to 0; the writer set
    // both to 1, so a save protected what the file left editable.
    let dir = scratch("protection");
    for (attrs, objects) in [("", false), (r#" objects="1" scenarios="1""#, true)] {
        let sheet = sheet_xml(
            r#"<row r="1"><c r="A1"><v>1</v></c></row>"#,
            "",
            &format!(r#"<sheetProtection sheet="1"{attrs}/>"#),
        );
        let path = dir.join("p.xlsx");
        std::fs::write(&path, Book::with_sheet(&sheet).bytes()).unwrap();
        let out = dir.join("out.xlsx");
        let _ = std::fs::remove_file(&out);
        call("convert", json!({ "path": p(&path), "output": p(&out) })).unwrap();
        let written = part_text(&out, "xl/worksheets/sheet1.xml");
        assert!(written.contains("<sheetProtection"), "{written}");
        assert_eq!(written.contains(r#"objects="1""#), objects, "{written}");
        assert_eq!(written.contains(r#"scenarios="1""#), objects, "{written}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
