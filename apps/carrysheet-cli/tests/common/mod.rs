//! Helpers for the `macros` tests: running the binary, reading its JSON,
//! and building zip packages in code.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

/// carrysheet-zip's byte-level zip builder, used in place.
#[path = "../../../../crates/carrysheet-zip/tests/common/mod.rs"]
pub mod zip_builder;

pub use zip_builder::Method as CompressionMethod;

pub const BIN: &str = env!("CARGO_BIN_EXE_carrysheet-cli");

/// POI's samples, by path.
pub fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/carrysheet-vba/tests/data")
        .join(name)
}

/// A fresh folder for files built by a test, also used as the home folder.
/// Removed when dropped.
pub struct Scratch(pub PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Scratch {
    pub fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "carrysheet-macros-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

pub fn run(args: &[&str]) -> Output {
    let home = Scratch::new();
    Command::new(BIN)
        .args(args)
        .env("HOME", &home.0)
        .env("USERPROFILE", &home.0)
        .env("TZ", "UTC")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

pub fn macros(path: &Path, extra: &[&str]) -> Output {
    let mut args = vec!["macros", path.to_str().unwrap()];
    args.extend_from_slice(extra);
    run(&args)
}

pub fn code(o: &Output) -> i32 {
    o.status.code().expect("exited normally")
}

pub fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

/// stdout must be exactly one JSON document on one line.
pub fn json_only(o: &Output) -> Value {
    let out = stdout(o);
    assert!(out.ends_with('\n'), "stdout: {out:?}");
    assert_eq!(out.trim_end().lines().count(), 1, "stdout: {out:?}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("not JSON ({e}): {out:?}"))
}

/// A successful `--json` answer.
pub fn ok_json(path: &Path, extra: &[&str]) -> Value {
    let mut args = vec!["--json"];
    args.extend_from_slice(extra);
    let o = macros(path, &args);
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr).trim());
    assert!(o.stderr.is_empty());
    let v = json_only(&o);
    assert_eq!(v["ok"], true);
    v
}

/// A failed `--json` answer: the error object, after checking the exit code.
pub fn error_json(path: &Path, extra: &[&str], exit: i32) -> Value {
    let mut args = vec!["--json"];
    args.extend_from_slice(extra);
    let o = macros(path, &args);
    assert_eq!(code(&o), exit, "{}", stdout(&o));
    let v = json_only(&o);
    assert_eq!(v["ok"], false);
    let e = v["error"].clone();
    let keys: Vec<_> = e.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["code", "what", "fix", "why"]);
    assert!(!o.stderr.is_empty(), "the message also goes to stderr");
    e
}

pub fn module_names(v: &Value) -> Vec<String> {
    v["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The parts of a minimal macro-enabled package. `vba_target` is the
/// workbook relationship's target (None: no VBA relationship).
pub fn package_parts(vba_target: Option<&str>) -> Vec<(String, Vec<u8>)> {
    let types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Default Extension="bin" ContentType="application/vnd.ms-office.vbaProject"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.ms-excel.sheet.macroEnabled.main+xml"/></Types>"#;
    let root_rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheets/></workbook>"#;
    let mut parts = vec![
        ("[Content_Types].xml".to_owned(), types.as_bytes().to_vec()),
        ("_rels/.rels".to_owned(), root_rels.as_bytes().to_vec()),
        ("xl/workbook.xml".to_owned(), workbook.as_bytes().to_vec()),
    ];
    if let Some(target) = vba_target {
        let rels = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.microsoft.com/office/2006/relationships/vbaProject" Target="{target}"/></Relationships>"#
        );
        parts.push(("xl/_rels/workbook.xml.rels".to_owned(), rels.into_bytes()));
    }
    parts
}

pub fn vba_bin() -> Vec<u8> {
    std::fs::read(sample("SimpleMacro-vbaProject.bin")).unwrap()
}

/// Writes a zip; each part is (name, bytes, method).
pub fn write_zip(path: &Path, parts: &[(String, Vec<u8>, CompressionMethod)]) {
    let mut zip = zip_builder::Builder::new();
    for (name, bytes, method) in parts {
        zip.add(name, bytes, *method);
    }
    std::fs::write(path, zip.finish()).unwrap();
}

pub fn deflated(parts: Vec<(String, Vec<u8>)>) -> Vec<(String, Vec<u8>, CompressionMethod)> {
    parts
        .into_iter()
        .map(|(n, b)| (n, b, CompressionMethod::Deflated))
        .collect()
}

/// Sets bits in the general purpose flags, or the compression method, of
/// every header (local and central) for the entry `name`.
pub fn patch_headers(bytes: &mut [u8], name: &str, flags: u16, method: Option<u16>) {
    let name = name.as_bytes();
    let mut patched = 0;
    for i in 0..bytes.len().saturating_sub(46) {
        let (flags_at, method_at, name_len_at, name_at) = match &bytes[i..i + 4] {
            b"PK\x03\x04" => (i + 6, i + 8, i + 26, i + 30),
            b"PK\x01\x02" => (i + 8, i + 10, i + 28, i + 46),
            _ => continue,
        };
        let len = u16::from_le_bytes([bytes[name_len_at], bytes[name_len_at + 1]]) as usize;
        if bytes.get(name_at..name_at + len) != Some(name) {
            continue;
        }
        let f = u16::from_le_bytes([bytes[flags_at], bytes[flags_at + 1]]) | flags;
        bytes[flags_at..flags_at + 2].copy_from_slice(&f.to_le_bytes());
        if let Some(m) = method {
            bytes[method_at..method_at + 2].copy_from_slice(&m.to_le_bytes());
        }
        patched += 1;
    }
    assert_eq!(patched, 2, "local and central header");
}
