//! External links: workbooks, DDE and OLE.

mod common;

use carrysheet_connections::{LinkKind, LinkLocation};
use common::report_of;

#[test]
fn links_in_formula_order() {
    let report = report_of("external_links.parts");
    let links = &report.external_links;
    let summary: Vec<(u32, LinkKind)> = links.iter().map(|l| (l.index, l.kind)).collect();
    assert_eq!(
        summary,
        [
            (1, LinkKind::Workbook),
            (2, LinkKind::Workbook),
            (3, LinkKind::Workbook),
            (4, LinkKind::Dde),
            (5, LinkKind::Ole),
            (7, LinkKind::Workbook),
        ]
    );

    let budget = &links[0];
    // A file URL is read as a path: server, share, folder and file name.
    let file = budget.target.as_ref().unwrap().file.as_ref().unwrap();
    assert_eq!(file.path, "\\\\files\\finance\\Budget%202026.xlsx");
    assert_eq!(file.host.as_deref(), Some("files"));
    assert_eq!(file.share.as_deref(), Some("finance"));
    assert_eq!(file.file_name.as_deref(), Some("Budget%202026.xlsx"));
    assert_eq!(budget.location, Some(LinkLocation::Path));
    assert_eq!(budget.sheet_names, ["Budget", "Actuals"]);
    assert_eq!(budget.part, "xl/externalLinks/externalLink1.xml");

    // Any prefix for the relationships namespace works.
    let missing = &links[1];
    let file = missing.target.as_ref().unwrap().file.as_ref().unwrap();
    assert_eq!(file.path, "Old.xlsx");
    assert_eq!(file.folder, None);
    assert_eq!(missing.location, Some(LinkLocation::PathMissing));

    // User information and the query are left out, not masked.
    let shared = &links[2];
    assert_eq!(
        shared.target.as_ref().unwrap().url.as_deref(),
        Some("https://files.example.com/Shared/Book.xlsx")
    );

    let dde = &links[3];
    assert_eq!(dde.dde_service.as_deref(), Some("Excel"));
    assert_eq!(
        dde.dde_topic.as_deref(),
        Some("C:\\Prices\\[Live.xlsx]Feed")
    );
    assert_eq!(dde.items, ["R1C1", "R2C1"]);
    assert_eq!(dde.target, None);

    let ole = &links[4];
    assert_eq!(ole.program_id.as_deref(), Some("Word.Document.12"));
    let file = ole.target.as_ref().unwrap().file.as_ref().unwrap();
    assert_eq!(file.path, "C:\\Docs\\Report.docx");
    assert_eq!(file.drive.as_deref(), Some("C:"));
    assert_eq!(ole.items, ["Bookmark1"]);

    let orphan = &links[5];
    assert_eq!(orphan.location, Some(LinkLocation::Startup));
    assert!(orphan.notes[0].contains("does not list"));

    // rId404 has no relationship.
    assert_eq!(report.problems.len(), 1, "{:#?}", report.problems);
    assert!(report.problems[0].what.contains("[6]"));
}

#[test]
fn two_references_to_one_part_are_named_as_such() {
    let mut parts = carrysheet_connections::MemoryParts::new();
    parts.insert(
        "xl/workbook.xml",
        r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><externalReferences><externalReference r:id="rId1"/><externalReference r:id="rId2"/></externalReferences></workbook>"#,
    );
    parts.insert(
        "xl/_rels/workbook.xml.rels",
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="x/externalLink" Target="externalLinks/externalLink1.xml"/><Relationship Id="rId2" Type="x/externalLink" Target="externalLinks/externalLink1.xml"/></Relationships>"#,
    );
    parts.insert(
        "xl/externalLinks/externalLink1.xml",
        r#"<externalLink xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><externalBook/></externalLink>"#,
    );
    let report =
        carrysheet_connections::inspect(&parts, &carrysheet_connections::Limits::default());
    assert_eq!(report.external_links.len(), 1);
    let problems: Vec<&str> = report.problems.iter().map(|p| p.what.as_str()).collect();
    assert_eq!(
        problems,
        ["External link [2] points at the same part as an earlier link; it is left out."]
    );
}

/// One external link part with a DDE link or a workbook link to `target`.
fn link_parts(link: &str, target: Option<&str>) -> carrysheet_connections::MemoryParts {
    let mut parts = carrysheet_connections::MemoryParts::new();
    parts.insert(
        "xl/externalLinks/externalLink1.xml",
        format!(
            r#"<externalLink xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">{link}</externalLink>"#
        ),
    );
    if let Some(target) = target {
        parts.insert(
            "xl/externalLinks/_rels/externalLink1.xml.rels",
            format!(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="x/externalLinkPath" Target="{target}" TargetMode="External"/></Relationships>"#
            ),
        );
    }
    parts
}

#[test]
fn dde_topics_follow_the_same_rules() {
    let secret = "aB3dE5gH7jK9mN1pQ3sT5vW7";
    for topic in [format!("pwd={secret}"), secret.to_string()] {
        let parts = link_parts(
            &format!(
                r#"<ddeLink ddeService="Feed" ddeTopic="{topic}"><ddeItems><ddeItem name="x;key={secret}"/><ddeItem name="R1C1"/></ddeItems></ddeLink>"#
            ),
            None,
        );
        let report =
            carrysheet_connections::inspect(&parts, &carrysheet_connections::Limits::default());
        let link = &report.external_links[0];
        assert_eq!(link.dde_topic, None);
        assert_eq!(link.hidden[0].what, "DDE topic");
        assert_eq!(link.items, ["R1C1"]);
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("aB3dE5"), "{json}");
    }
}

#[test]
fn link_paths_with_credentials_are_hidden_whole() {
    for (target, what) in [
        (
            "https://contoso.sharepoint.com/:x:/g/personal/ann/EaBcD3fG",
            "URL path",
        ),
        (
            "https://hooks.example.com/services/T1/B2/aB3dE5gH7jK9mN1pQ3sT5vW7",
            "URL path",
        ),
        (
            "\\\\files\\share;token=aB3dE5gH7jK9\\Book.xlsx",
            "file path",
        ),
    ] {
        let parts = link_parts(r#"<externalBook r:id="rId1"/>"#, Some(target));
        let report =
            carrysheet_connections::inspect(&parts, &carrysheet_connections::Limits::default());
        let t = report.external_links[0].target.as_ref().unwrap();
        let hidden = t.hidden_path.as_ref().expect("a hidden path");
        assert_eq!(hidden.what, what, "{target}");
        assert!(hidden.note.contains("To see it: set Options::include_raw"));
        assert_eq!(t.file, None, "{target}");
        if what == "URL path" {
            let url = t.url.as_deref().unwrap();
            assert_eq!(url.matches('/').count(), 2, "only scheme and host: {url}");
        }
    }
}

#[test]
fn services_programs_and_sheet_names_are_checked_like_topics() {
    let parts = link_parts(
        r#"<ddeLink ddeService="pwd=Zq9v7K" ddeTopic="System"><ddeItems><ddeItem name="R1C1"/></ddeItems></ddeLink>"#,
        None,
    );
    let report =
        carrysheet_connections::inspect(&parts, &carrysheet_connections::Limits::default());
    let link = &report.external_links[0];
    assert_eq!(link.dde_service, None);
    assert_eq!(link.dde_topic.as_deref(), Some("System"));
    assert_eq!(link.hidden[0].what, "DDE service");
    assert!(!serde_json::to_string(&report).unwrap().contains("Zq9v7K"));

    let parts = link_parts(
        r#"<externalBook r:id="rId1"><sheetNames><sheetName val="Budget"/><sheetName val="password Zq9v7K"/></sheetNames></externalBook>"#,
        Some("Book.xlsx"),
    );
    let report =
        carrysheet_connections::inspect(&parts, &carrysheet_connections::Limits::default());
    let link = &report.external_links[0];
    assert_eq!(link.sheet_names, ["Budget"]);
    assert!(
        link.notes
            .iter()
            .any(|n| n.starts_with("1 sheet names or items")),
        "{:?}",
        link.notes
    );
    assert!(!serde_json::to_string(&report).unwrap().contains("Zq9v7K"));

    let parts = link_parts(
        r#"<oleLink r:id="rId1" progId="Word.Document.12"/>"#,
        Some("C:\\a.docx"),
    );
    let report =
        carrysheet_connections::inspect(&parts, &carrysheet_connections::Limits::default());
    assert_eq!(
        report.external_links[0].program_id.as_deref(),
        Some("Word.Document.12")
    );
}

#[test]
fn raw_link_texts_are_items_with_what_and_text() {
    let parts = link_parts(
        r#"<ddeLink ddeService="Feed" ddeTopic="Prices\Feed"/>"#,
        None,
    );
    let options = carrysheet_connections::Options { include_raw: true };
    let report = carrysheet_connections::inspect_with(
        &parts,
        &carrysheet_connections::Limits::default(),
        &options,
    );
    let link = &report.external_links[0];
    assert_eq!(link.raw.len(), 1);
    assert_eq!(link.raw[0].what, "DDE topic");
    assert_eq!(link.raw[0].text, "Prices\\Feed");
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["external_links"][0]["raw"][0]["what"], "DDE topic");
}
