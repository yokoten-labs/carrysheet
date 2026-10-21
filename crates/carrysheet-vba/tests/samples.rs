//! Real macro files from Apache POI's test data (see tests/data/NOTICE).

mod common;

use carrysheet_vba::{
    Limits, ModuleKind, PasswordState, ReferenceKind, VbaPart, VbaProject, read_vba_project_bin,
    read_xls_vba,
};

fn names(p: &VbaProject) -> Vec<&str> {
    p.modules.iter().map(|m| m.name.as_str()).collect()
}

fn source<'a>(p: &'a VbaProject, module: &str) -> &'a str {
    p.modules
        .iter()
        .find(|m| m.name == module)
        .and_then(|m| m.source.as_deref())
        .unwrap_or_else(|| panic!("no source for {module}"))
}

/// The source as the VBA editor shows it: without `Attribute` lines.
fn editor_text(source: &str) -> String {
    source
        .split_inclusive('\n')
        .filter(|l| !l.starts_with("Attribute "))
        .collect()
}

fn check_simple_macro(p: &VbaProject) {
    assert_eq!(p.name, "VBAProject");
    assert_eq!(p.code_page, 1252);
    assert_eq!(p.encoding, "windows-1252");
    assert!(p.has_project_stream);
    assert_eq!(
        names(p),
        ["Module1", "ThisWorkbook", "Sheet1", "Sheet2", "Sheet3"]
    );
    let kinds: Vec<ModuleKind> = p.modules.iter().map(|m| m.kind).collect();
    assert_eq!(
        kinds,
        [
            ModuleKind::Standard,
            ModuleKind::Document,
            ModuleKind::Document,
            ModuleKind::Document,
            ModuleKind::Document
        ]
    );
    for m in &p.modules {
        // Sheet modules hold only `Attribute` lines: no source to show.
        assert_eq!(m.has_source, m.name == "Module1", "{}", m.name);
        assert!(m.pcode_bytes > 0, "{}", m.name);
        assert!(!m.hidden && m.error.is_none(), "{}", m.name);
    }
    // POI's expectations: document modules carry these attributes.
    for doc in ["ThisWorkbook", "Sheet1", "Sheet2", "Sheet3"] {
        let s = source(p, doc);
        assert!(s.contains("Attribute VB_Exposed = True"), "{doc}");
        assert!(s.contains("Attribute VB_TemplateDerived = False"), "{doc}");
    }
    let module1 = source(p, "Module1");
    assert!(module1.starts_with("Attribute VB_Name = \"Module1\"\r\n"));
    // SimpleMacro.vba is the macro as the editor shows it.
    let expected = String::from_utf8(common::data("SimpleMacro.vba")).unwrap();
    assert_eq!(
        editor_text(module1).trim_start_matches("\r\n"),
        expected.trim_end_matches("\r\n").to_string() + "\r\n"
    );
    assert_eq!(p.modules[0].line_count, 13);

    assert_eq!(p.references.len(), 2);
    assert_eq!(p.references[0].name.as_deref(), Some("stdole"));
    assert_eq!(p.references[0].kind, ReferenceKind::Registered);
    assert!(
        p.references[0]
            .libid
            .contains("{00020430-0000-0000-C000-000000000046}")
    );
    assert_eq!(p.references[1].name.as_deref(), Some("Office"));

    let pr = &p.protection;
    assert!(pr.cmg_present && pr.dpb_present && pr.gc_present);
    assert_eq!(pr.password, PasswordState::None);
    assert_eq!(pr.visible, Some(true));
    assert_eq!(pr.vbe_protected, Some(false));
    assert!(!pr.locked_for_viewing());

    // A recorded macro: nothing risky.
    assert_eq!(p.flags, []);
}

#[test]
fn simple_macro_xlsm_vba_project_bin() {
    let p = read_vba_project_bin(&common::data("SimpleMacro-vbaProject.bin")).unwrap();
    check_simple_macro(&p);
}

#[test]
fn simple_macro_xls_storage() {
    let p = read_xls_vba(&common::data("SimpleMacro.xls"))
        .unwrap()
        .expect("SimpleMacro.xls has macros");
    check_simple_macro(&p);
}

#[test]
fn xls_and_xlsm_give_the_same_project() {
    let a = read_vba_project_bin(&common::data("SimpleMacro-vbaProject.bin")).unwrap();
    let b = read_xls_vba(&common::data("SimpleMacro.xls"))
        .unwrap()
        .unwrap();
    assert_eq!(a.modules, b.modules);
}

#[test]
fn non_ascii_module_names_62624() {
    let p = read_vba_project_bin(&common::data("62624.bin")).unwrap();
    assert_eq!(p.modules.len(), 13);
    let m = p.modules.iter().find(|m| m.name == "Módulo1").unwrap();
    assert_eq!(m.kind, ModuleKind::Standard);
    assert_eq!(m.stream_name, "Módulo1");
    assert!(m.source.as_deref().unwrap().contains("Calcula_tributos"));
    assert_eq!(m.line_count, 915);
    let docs = p
        .modules
        .iter()
        .filter(|m| m.kind == ModuleKind::Document)
        .count();
    assert_eq!(docs, 9);
    assert!(p.modules.iter().all(|m| m.error.is_none() && !m.hidden));
    assert_eq!(p.flags, []);
}

#[test]
fn mac_excel_code_page_and_cr_line_ends_60273() {
    let p = read_xls_vba(&common::data("60273.xls")).unwrap().unwrap();
    assert_eq!(p.code_page, 10000);
    assert_eq!(p.encoding, "macintosh");
    assert_eq!(names(&p), ["Module1", "ThisWorkbook"]);
    let m = &p.modules[0];
    assert!(m.source.as_deref().unwrap().contains("9/8/2004"));
    // Lines end with CR alone; they still count as lines.
    assert_eq!(m.line_count, 12);
    assert_eq!(p.flags, []);
}

#[test]
fn many_modules_and_a_control_reference_59830() {
    let p = read_xls_vba(&common::data("59830.xls")).unwrap().unwrap();
    assert_eq!(p.modules.len(), 29);
    assert!(source(&p, "Module20").contains("here start of superscripting"));
    assert!(
        p.references
            .iter()
            .any(|r| r.kind == ReferenceKind::Control && r.name.as_deref() == Some("MSForms"))
    );
    assert_eq!(p.flags, []);
}

#[test]
fn part_read_matches_the_shortcut() {
    let bytes = common::data("62624.bin");
    let part = VbaPart::from_bin(bytes.clone());
    assert_eq!(
        part.read(&Limits::default()).unwrap(),
        read_vba_project_bin(&bytes).unwrap()
    );
}
