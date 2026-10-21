//! Helpers shared by the integration tests: loading the `.parts` fixtures
//! and building DataMashup packages in code.

#![allow(dead_code)]

pub mod fuzz;

use std::io::Write;

use carrysheet_connections::{Limits, MemoryParts, Report, inspect};
use flate2::Compression;
use flate2::write::DeflateEncoder;

/// Loads `tests/data/<name>`: text where each `=== part/name` line starts a
/// part; lines before the first are a comment.
pub fn fixture(name: &str) -> MemoryParts {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut parts = MemoryParts::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("=== ") {
            if let Some((name, body)) = current.take() {
                parts.insert(&name, body.into_bytes());
            }
            current = Some((name.trim().to_string(), String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some((name, body)) = current {
        parts.insert(&name, body.into_bytes());
    }
    parts
}

pub fn report_of(name: &str) -> Report {
    inspect(&fixture(name), &Limits::default())
}

/// How a zip entry is stored.
#[derive(Clone, Copy)]
pub enum Method {
    Stored,
    Deflated,
}

/// Builds a zip (APPNOTE.TXT layout, no data descriptors, CRC left 0: the
/// reader does not check it).
pub fn zip(entries: &[(&str, &[u8], Method)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut directory = Vec::new();
    for (name, data, method) in entries {
        let (code, stored): (u16, Vec<u8>) = match method {
            Method::Stored => (0, data.to_vec()),
            Method::Deflated => {
                let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(data).unwrap();
                (8, encoder.finish().unwrap())
            }
        };
        let offset = u32::try_from(out.len()).unwrap();
        let name_len = u16::try_from(name.len()).unwrap();
        let compressed = u32::try_from(stored.len()).unwrap();
        let size = u32::try_from(data.len()).unwrap();
        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend(20u16.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(code.to_le_bytes());
        out.extend([0u8; 4]);
        out.extend(0u32.to_le_bytes());
        out.extend(compressed.to_le_bytes());
        out.extend(size.to_le_bytes());
        out.extend(name_len.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(name.as_bytes());
        out.extend(&stored);

        directory.extend(0x0201_4b50u32.to_le_bytes());
        directory.extend(20u16.to_le_bytes());
        directory.extend(20u16.to_le_bytes());
        directory.extend(0u16.to_le_bytes());
        directory.extend(code.to_le_bytes());
        directory.extend([0u8; 4]);
        directory.extend(0u32.to_le_bytes());
        directory.extend(compressed.to_le_bytes());
        directory.extend(size.to_le_bytes());
        directory.extend(name_len.to_le_bytes());
        directory.extend([0u8; 12]);
        directory.extend(offset.to_le_bytes());
        directory.extend(name.as_bytes());
    }
    let directory_offset = u32::try_from(out.len()).unwrap();
    let count = u16::try_from(entries.len()).unwrap();
    let directory_len = u32::try_from(directory.len()).unwrap();
    out.extend(directory);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(count.to_le_bytes());
    out.extend(count.to_le_bytes());
    out.extend(directory_len.to_le_bytes());
    out.extend(directory_offset.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// The [MS-QDEFF] stream around a package parts zip.
pub fn qdeff(package_parts: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(0u32.to_le_bytes());
    let len = |bytes: &[u8]| u32::try_from(bytes.len()).unwrap().to_le_bytes();
    out.extend(len(package_parts));
    out.extend(package_parts);
    let permissions = br#"<?xml version="1.0" encoding="utf-8"?><PermissionList xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><CanEvaluateFuturePackages>false</CanEvaluateFuturePackages><FirewallEnabled>true</FirewallEnabled></PermissionList>"#;
    out.extend(len(permissions));
    out.extend(permissions);
    out.extend(0u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out
}

pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A customXml item holding a DataMashup, UTF-16 with a byte order mark as
/// Excel writes it.
pub fn mashup_item(stream: &[u8]) -> Vec<u8> {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"utf-16\"?><DataMashup xmlns=\"http://schemas.microsoft.com/DataMashup\">{}</DataMashup>",
        base64(stream)
    );
    let mut out = vec![0xFF, 0xFE];
    out.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    out
}

/// A workbook whose only part of interest is a DataMashup with this
/// `Section1.m`.
pub fn power_query_parts(section: &str, method: Method) -> MemoryParts {
    let zip = zip(&[
        ("[Content_Types].xml", b"<Types/>", Method::Stored),
        ("Config/Package.xml", b"<Package/>", method),
        ("Formulas/Section1.m", section.as_bytes(), method),
    ]);
    let mut parts = MemoryParts::new();
    parts.insert("customXml/item1.xml", mashup_item(&qdeff(&zip)));
    parts.insert(
        "customXml/itemProps1.xml",
        b"<ds:datastoreItem xmlns:ds=\"x\"/>".to_vec(),
    );
    parts
}
