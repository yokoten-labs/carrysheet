//! Hostile input: every limit has a small file built here that crosses it.
//! Nothing may panic; each case ends in a problem that says what happened.

mod common;

use carrysheet_connections::{
    Limits, MemoryParts, Options, PartError, PartSource, Report, inspect, inspect_with,
};
use common::{Method, fixture, mashup_item, power_query_parts, qdeff, zip};

const MAIN: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";

fn with_connections(body: &str) -> MemoryParts {
    let mut parts = MemoryParts::new();
    parts.insert(
        "xl/connections.xml",
        format!("<connections xmlns=\"{MAIN}\">{body}</connections>").into_bytes(),
    );
    parts
}

fn problem_text(report: &Report) -> String {
    report
        .problems
        .iter()
        .map(|p| {
            format!(
                "{} | {} | {} | {}\n",
                p.part.as_deref().unwrap_or("-"),
                p.what,
                p.why,
                p.fix
            )
        })
        .collect()
}

#[test]
fn xml_part_over_the_size_limit_is_skipped() {
    let parts = with_connections(&"<connection id=\"1\" name=\"x\"/>".repeat(10));
    let limits = Limits {
        max_xml_part_bytes: 100,
        ..Limits::default()
    };
    let report = inspect(&parts, &limits);
    assert!(report.connections.is_empty());
    let text = problem_text(&report);
    assert!(text.contains("it is 383 bytes"), "{text}");
    assert!(
        text.contains("Limits::max_xml_part_bytes is 100 bytes"),
        "{text}"
    );
    assert!(text.contains("Raise Limits::max_xml_part_bytes"), "{text}");
}

#[test]
fn doctype_and_entities_are_refused() {
    let mut parts = MemoryParts::new();
    let laughs = format!(
        "<?xml version=\"1.0\"?><!DOCTYPE lolz [<!ENTITY lol \"lol\"><!ENTITY lol2 \"&lol;&lol;&lol;&lol;\">]><connections xmlns=\"{MAIN}\"><connection id=\"1\" name=\"&lol2;\"/></connections>"
    );
    parts.insert("xl/connections.xml", laughs.into_bytes());
    let report = inspect(&parts, &Limits::default());
    assert!(report.connections.is_empty());
    assert!(
        problem_text(&report).contains("DOCTYPE"),
        "{}",
        problem_text(&report)
    );

    let parts = with_connections("<connection id=\"1\" name=\"&undefined;\"/>");
    let report = inspect(&parts, &Limits::default());
    assert!(
        problem_text(&report).contains("&undefined;"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn nesting_depth_is_limited() {
    let deep = format!(
        "<connection id=\"1\" name=\"x\">{}{}</connection>",
        "<a>".repeat(100),
        "</a>".repeat(100)
    );
    let report = inspect(&with_connections(&deep), &Limits::default());
    assert!(
        problem_text(&report).contains("deeper than 64"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn connection_count_is_limited() {
    let body: String = (1..=5)
        .map(|i| format!("<connection id=\"{i}\" name=\"c{i}\"/>"))
        .collect();
    let limits = Limits {
        max_items: 3,
        ..Limits::default()
    };
    let report = inspect(&with_connections(&body), &limits);
    assert_eq!(report.connections.len(), 3);
    assert!(
        problem_text(&report).contains("Only the first 3 connections"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn attribute_count_is_limited() {
    let attributes: String = (0..1_001).map(|i| format!(" a{i}=\"\"")).collect();
    let body = format!("<connection id=\"1\" name=\"x\"{attributes}/>");
    let report = inspect(&with_connections(&body), &Limits::default());
    assert!(report.connections.is_empty());
    assert!(
        problem_text(&report).contains("more than 1000 attributes"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn query_table_and_problem_counts_are_limited() {
    let mut parts = with_connections("<connection id=\"1\" name=\"x\"/>");
    for i in 0..20 {
        parts.insert(
            &format!("xl/queryTables/queryTable{i}.xml"),
            format!(
                "<queryTable xmlns=\"{MAIN}\" name=\"q{i}\" connectionId=\"{}\"/>",
                100 + i
            )
            .into_bytes(),
        );
    }
    let limits = Limits {
        max_items: 5,
        ..Limits::default()
    };
    let report = inspect(&parts, &limits);
    // Each query table names a missing connection; the list stops at 5 and
    // says how many more there were.
    assert_eq!(report.problems.len(), 6, "{}", problem_text(&report));
    assert!(
        report.problems[5].what.contains("more problems"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn part_count_is_limited() {
    let parts = fixture("connections.parts");
    let limits = Limits {
        max_parts: 3,
        ..Limits::default()
    };
    let report = inspect(&parts, &limits);
    assert!(
        problem_text(&report).contains("only the first 3 were looked at"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn long_raw_command_text_is_hidden_whole() {
    // The secret straddles the limit. A cut would leave its start where
    // masking no longer recognizes it, so the text is never cut: over the
    // limit it is hidden whole, even with raw texts asked for.
    let command = format!("{} WHERE pwd = 'cut-secret-value'", "x".repeat(3_990));
    let body = format!(
        "<connection id=\"1\" name=\"x\" type=\"1\"><dbPr connection=\"DSN=a\" command=\"{command}\"/></connection>"
    );
    let parts = with_connections(&body);
    let report = inspect(&parts, &Limits::default());
    let connection = &report.connections[0];
    assert_eq!(connection.command_text, None);
    assert_eq!(connection.hidden[0].what, "SQL command");
    assert_eq!(connection.hidden[0].characters, 4_021);
    assert!(
        connection.hidden[0]
            .note
            .contains("longer than Limits::max_command_chars (4000)"),
        "{}",
        connection.hidden[0].note
    );

    let options = Options { include_raw: true };
    let report = inspect_with(&parts, &Limits::default(), &options);
    let connection = &report.connections[0];
    assert_eq!(connection.raw.as_ref().unwrap().command_text, None);
    let note = &connection.hidden[0];
    assert_eq!((note.what.as_str(), note.characters), ("command", 4_021));
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("cut-s"), "{json}");
}

#[test]
fn long_zip_member_names_are_cut_with_a_note() {
    let name = format!("Formulas/{}.m", "n".repeat(300));
    let package = zip(&[
        (
            name.as_str(),
            b"section S; shared A = 1;".as_slice(),
            Method::Stored,
        ),
        (
            name.as_str(),
            b"section S; shared A = 1;".as_slice(),
            Method::Stored,
        ),
    ]);
    let limits = Limits {
        max_name_chars: 50,
        ..Limits::default()
    };
    let report = inspect(&mashup_parts(&qdeff(&package)), &limits);
    let text = problem_text(&report);
    assert!(text.contains("twice"), "{text}");
    assert!(text.contains("[cut: 261 more characters]"), "{text}");
}

#[test]
fn mashup_over_the_size_limit_is_skipped() {
    let parts = power_query_parts("section Section1; shared A = 1;", Method::Stored);
    let limits = Limits {
        max_mashup_bytes: 64,
        ..Limits::default()
    };
    let report = inspect(&parts, &limits);
    assert!(report.power_query.is_none());
    assert!(
        problem_text(&report).contains("Limits::max_mashup_bytes is 64 bytes"),
        "{}",
        problem_text(&report)
    );
}

fn mashup_parts(stream: &[u8]) -> MemoryParts {
    let mut parts = MemoryParts::new();
    parts.insert("customXml/item1.xml", mashup_item(stream));
    parts
}

#[test]
fn bad_base64_and_headers_are_reported() {
    let mut parts = MemoryParts::new();
    parts.insert(
        "customXml/item1.xml",
        b"<DataMashup>AAAA*AAA</DataMashup>".to_vec(),
    );
    let report = inspect(&parts, &Limits::default());
    assert!(
        problem_text(&report).contains("base64"),
        "{}",
        problem_text(&report)
    );
    assert!(
        report.power_query.is_some(),
        "presence and size are still reported"
    );

    // A package parts length that runs past the end.
    let report = inspect(
        &mashup_parts(&[0, 0, 0, 0, 0xFF, 0xFF, 0xFF, 0x7F, 1, 2]),
        &Limits::default(),
    );
    assert!(
        problem_text(&report).contains("runs past the end"),
        "{}",
        problem_text(&report)
    );

    // A version that is not 0.
    let report = inspect(&mashup_parts(&[9, 0, 0, 0, 0, 0, 0, 0]), &Limits::default());
    assert!(
        problem_text(&report).contains("version 9"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn zip_bomb_is_stopped() {
    let zeros = vec![0u8; 4 * 1024 * 1024];
    let package = zip(&[("Formulas/Section1.m", &zeros, Method::Deflated)]);
    assert!(
        package.len() < 64 * 1024,
        "the bomb is small: {}",
        package.len()
    );
    let limits = Limits {
        max_zip_entry_bytes: 1024 * 1024,
        ..Limits::default()
    };
    let report = inspect(&mashup_parts(&qdeff(&package)), &limits);
    assert!(
        problem_text(&report)
            .contains("It is 4194304 bytes, and Limits::max_zip_entry_bytes is 1048576 bytes."),
        "{}",
        problem_text(&report)
    );
    assert!(report.power_query.unwrap().queries.is_empty());
}

#[test]
fn zip_bomb_with_a_false_size_is_stopped() {
    // The directory claims 10 bytes; the stream inflates to 4 MiB.
    let zeros = vec![0u8; 4 * 1024 * 1024];
    let mut package = zip(&[("Formulas/Section1.m", &zeros, Method::Deflated)]);
    let directory = package
        .windows(4)
        .rposition(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .unwrap();
    package[directory + 24..directory + 28].copy_from_slice(&10u32.to_le_bytes());
    let limits = Limits {
        max_zip_entry_bytes: 1024 * 1024,
        ..Limits::default()
    };
    let report = inspect(&mashup_parts(&qdeff(&package)), &limits);
    assert!(
        problem_text(&report).contains(
            "It inflates to more than 1048576 bytes, and Limits::max_zip_entry_bytes is 1048576 bytes."
        ),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn zip_total_budget_is_shared() {
    let text = format!("section Section1; shared A = \"{}\";", "a".repeat(600_000));
    let package = zip(&[
        ("Formulas/Section1.m", text.as_bytes(), Method::Deflated),
        ("Formulas/Section2.m", text.as_bytes(), Method::Deflated),
    ]);
    let limits = Limits {
        max_zip_total_bytes: 1_000_000,
        ..Limits::default()
    };
    let report = inspect(&mashup_parts(&qdeff(&package)), &limits);
    assert_eq!(report.power_query.as_ref().unwrap().queries.len(), 1);
    assert!(
        problem_text(&report).contains("more than Limits::max_zip_total_bytes (1000000 bytes)"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn zip_entry_count_is_limited() {
    let package = zip(&[
        ("a", b"1", Method::Stored),
        ("b", b"2", Method::Stored),
        (
            "Formulas/Section1.m",
            b"section S; shared A = 1;",
            Method::Stored,
        ),
    ]);
    let limits = Limits {
        max_zip_entries: 2,
        ..Limits::default()
    };
    let report = inspect(&mashup_parts(&qdeff(&package)), &limits);
    assert!(
        problem_text(&report)
            .contains("Its zip lists 3 entries, more than Limits::max_zip_entries (2)."),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn damaged_zips_do_not_panic() {
    let good = zip(&[(
        "Formulas/Section1.m",
        b"section S; shared A = 1;",
        Method::Deflated,
    )]);
    // Every truncation and a byte flip at every position.
    for cut in 0..good.len() {
        let _ = inspect(&mashup_parts(&qdeff(&good[..cut])), &Limits::default());
    }
    for at in 0..good.len() {
        let mut bad = good.clone();
        bad[at] ^= 0xA5;
        let _ = inspect(&mashup_parts(&qdeff(&bad)), &Limits::default());
    }
}

#[test]
fn query_count_is_limited() {
    let section: String = std::iter::once("section Section1;".to_string())
        .chain((0..10).map(|i| format!("shared Q{i} = {i};")))
        .collect();
    let parts = power_query_parts(&section, Method::Stored);
    let limits = Limits {
        max_items: 4,
        ..Limits::default()
    };
    let report = inspect(&parts, &limits);
    assert_eq!(report.power_query.as_ref().unwrap().queries.len(), 4);
    assert!(
        problem_text(&report).contains("Only the first 4 Power Query queries"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn bad_text_encodings_are_reported() {
    let mut parts = MemoryParts::new();
    parts.insert("xl/connections.xml", vec![0xFF, 0xFE, b'<']);
    let report = inspect(&parts, &Limits::default());
    assert!(
        problem_text(&report).contains("odd number of bytes"),
        "{}",
        problem_text(&report)
    );

    let mut parts = MemoryParts::new();
    parts.insert(
        "xl/connections.xml",
        b"<connections name=\"\xC3\"/>".to_vec(),
    );
    let report = inspect(&parts, &Limits::default());
    assert!(
        problem_text(&report).contains("not valid UTF-8"),
        "{}",
        problem_text(&report)
    );
}

/// A source that misbehaves: one part unreadable, one returned larger than
/// asked for.
struct Unruly;

impl PartSource for Unruly {
    fn part_names(&self) -> Vec<String> {
        vec![
            "xl/connections.xml".into(),
            "xl/externalLinks/externalLink1.xml".into(),
        ]
    }

    fn part_size(&self, _name: &str) -> Option<u64> {
        None
    }

    fn read_part(&self, name: &str, max_bytes: u64) -> Result<Option<Vec<u8>>, PartError> {
        match name {
            "xl/connections.xml" => Err(PartError::Unreadable {
                reason: "CRC mismatch in the zip".into(),
            }),
            "xl/externalLinks/externalLink1.xml" => {
                Ok(Some(vec![b' '; usize::try_from(max_bytes).unwrap() + 1]))
            }
            _ => Ok(None),
        }
    }
}

#[test]
fn misbehaving_sources_are_reported() {
    let limits = Limits {
        max_xml_part_bytes: 1_000,
        ..Limits::default()
    };
    let report = inspect(&Unruly, &limits);
    let text = problem_text(&report);
    assert!(text.contains("CRC mismatch"), "{text}");
    assert!(text.contains("it is 1001 bytes"), "{text}");
}

#[test]
fn mutated_fixtures_do_not_panic() {
    let mut state = 0x1234_5678_9ABC_DEF0u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for name in [
        "connections.parts",
        "external_links.parts",
        "fallback.parts",
    ] {
        let source = fixture(name);
        let names = source.part_names();
        for _ in 0..200 {
            let mut mutated = MemoryParts::new();
            for part in &names {
                let mut bytes = source.read_part(part, u64::MAX).unwrap().unwrap();
                if !bytes.is_empty() && next() % 3 == 0 {
                    for _ in 0..1 + next() % 4 {
                        let at = usize::try_from(next() % bytes.len() as u64).unwrap();
                        bytes[at] = b"<>\"'&;=/x\0\xFF"[usize::try_from(next() % 11).unwrap()];
                    }
                }
                mutated.insert(part, bytes);
            }
            let report = inspect(&mutated, &Limits::default());
            let _ = serde_json::to_string(&report).unwrap();
        }
    }
}

#[test]
fn masking_stays_linear_on_repetitive_text() {
    // Patterns that would make a careless scanner quadratic. 1 MiB each;
    // a quadratic pass would take minutes, a linear one milliseconds.
    let unit_patterns = [
        "a=",
        "pwd=x;",
        "pwd='",
        "pwd=''x''",
        ":",
        "password ",
        "Bearer ",
        "<a>",
        "<pwd>",
        "https://",
        "\"",
        "{",
        "==",
        "#\"",
        "[",
        "\"\"",
        "\\\"",
        "'a' ",
        "password \"x\" ",
        "Authorization(",
        "x://?a=y://?a=",
        "https://u:p@",
        "<pwd></a>",
        "< ",
        ":=",
        "=>",
        "pwd==",
        "PASSWORD(",
        "aB3dE5gH7jK9mN1pQ3sT5vW7 ",
        "eyJ",
    ];
    for unit in unit_patterns {
        let text = unit.repeat(1024 * 1024 / unit.len());
        let started = std::time::Instant::now();
        let _ = carrysheet_connections::mask_secrets(&text);
        let _ = carrysheet_connections::mask_connection_string(&text);
        let spent = started.elapsed();
        eprintln!("masking 1 MiB of `{unit}`: {spent:?}");
        assert!(spent.as_secs() < 20, "`{unit}` took {spent:?}");
    }
}

/// One stored local entry, then `copies` central-directory entries that all
/// point at it, each declaring `declared_size` as the uncompressed size.
fn stored_zip(name: &str, data: &[u8], copies: u16, declared_size: u32) -> Vec<u8> {
    let data_len = u32::try_from(data.len()).unwrap();
    let name_len = u16::try_from(name.len()).unwrap();
    let mut out = Vec::new();
    out.extend(0x0403_4b50u32.to_le_bytes());
    out.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out.extend(0u32.to_le_bytes());
    out.extend(data_len.to_le_bytes());
    out.extend(declared_size.to_le_bytes());
    out.extend(name_len.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out.extend(name.as_bytes());
    out.extend(data);
    let directory = u32::try_from(out.len()).unwrap();
    for _ in 0..copies {
        out.extend(0x0201_4b50u32.to_le_bytes());
        out.extend([20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend(0u32.to_le_bytes());
        out.extend(data_len.to_le_bytes());
        out.extend(declared_size.to_le_bytes());
        out.extend(name_len.to_le_bytes());
        out.extend([0u8; 12]);
        out.extend(0u32.to_le_bytes());
        out.extend(name.as_bytes());
    }
    let directory_len = u32::try_from(out.len()).unwrap() - directory;
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(copies.to_le_bytes());
    out.extend(copies.to_le_bytes());
    out.extend(directory_len.to_le_bytes());
    out.extend(directory.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

#[test]
fn stored_entries_cannot_dodge_the_zip_budgets() {
    let section = format!("section Section1; shared Q = \"{}\";", "x".repeat(200_000));
    let limits = Limits {
        max_zip_entry_bytes: 1024,
        max_zip_total_bytes: 4096,
        ..Limits::default()
    };

    // The review probe: 300 directory entries share one stored entry that
    // declares a size of 0.
    let package = stored_zip("Formulas/Section1.m", section.as_bytes(), 300, 0);
    let started = std::time::Instant::now();
    let report = inspect(&mashup_parts(&qdeff(&package)), &limits);
    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
    assert!(report.power_query.as_ref().unwrap().queries.is_empty());
    assert!(
        problem_text(&report).contains("two different sizes"),
        "{}",
        problem_text(&report)
    );

    // The same with honest sizes: the repeated name is refused.
    let size = u32::try_from(section.len()).unwrap();
    let package = stored_zip("Formulas/Section1.m", section.as_bytes(), 300, size);
    let report = inspect(&mashup_parts(&qdeff(&package)), &limits);
    assert!(
        problem_text(&report).contains("lists the entry Formulas/Section1.m twice"),
        "{}",
        problem_text(&report)
    );

    // One entry whose stored size and declared size disagree.
    let package = stored_zip("Formulas/Section1.m", section.as_bytes(), 1, 0);
    let report = inspect(&mashup_parts(&qdeff(&package)), &limits);
    assert!(
        problem_text(&report).contains("two different sizes"),
        "{}",
        problem_text(&report)
    );

    // An honest stored entry is still held to the entry limit.
    let package = stored_zip("Formulas/Section1.m", section.as_bytes(), 1, size);
    let report = inspect(&mashup_parts(&qdeff(&package)), &limits);
    assert!(
        problem_text(&report).contains("Limits::max_zip_entry_bytes is 1024 bytes"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn overlapping_zip_entries_are_refused() {
    // Two names whose directory entries point at the same local entry.
    let text = b"section Section1; shared A = 1;";
    let mut package = zip(&[
        ("Formulas/Section1.m", text, Method::Stored),
        ("Formulas/Section2.m", text, Method::Stored),
    ]);
    let directory = package
        .windows(4)
        .rposition(|w| w == [0x50, 0x4b, 0x01, 0x02])
        .unwrap();
    package[directory + 42..directory + 46].copy_from_slice(&0u32.to_le_bytes());
    let report = inspect(&mashup_parts(&qdeff(&package)), &Limits::default());
    assert!(
        problem_text(&report).contains("overlap"),
        "{}",
        problem_text(&report)
    );
}

const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";

/// `sheets` sheets that all point at one sheet part, whose relationships
/// point `table_rels` times at one table and `pivot_caches` pivot caches
/// that all point at one definition.
fn repeated_parts(sheets: usize, table_rels: usize, pivot_caches: usize) -> MemoryParts {
    let mut workbook = format!("<workbook xmlns=\"{MAIN}\" xmlns:r=\"{R}\"><sheets>");
    for i in 0..sheets {
        workbook.push_str(&format!(
            "<sheet name=\"S{i}\" sheetId=\"{i}\" r:id=\"rId1\"/>"
        ));
    }
    workbook.push_str("</sheets><pivotCaches>");
    for i in 0..pivot_caches {
        workbook.push_str(&format!("<pivotCache cacheId=\"{i}\" r:id=\"rId2\"/>"));
    }
    workbook.push_str("</pivotCaches></workbook>");
    let mut sheet_rels = format!("<Relationships xmlns=\"{RELS}\">");
    for i in 0..table_rels {
        sheet_rels.push_str(&format!(
            "<Relationship Id=\"r{i}\" Type=\"x/table\" Target=\"../tables/table1.xml\"/>"
        ));
    }
    sheet_rels.push_str("</Relationships>");
    let mut parts = MemoryParts::new();
    parts.insert("xl/workbook.xml", workbook);
    parts.insert(
        "xl/_rels/workbook.xml.rels",
        format!(
            "<Relationships xmlns=\"{RELS}\"><Relationship Id=\"rId1\" Type=\"x/worksheet\" Target=\"worksheets/sheet1.xml\"/><Relationship Id=\"rId2\" Type=\"x/pivotCacheDefinition\" Target=\"pivotCache/pivotCacheDefinition1.xml\"/></Relationships>"
        ),
    );
    parts.insert("xl/worksheets/sheet1.xml", "<worksheet/>");
    parts.insert("xl/worksheets/_rels/sheet1.xml.rels", sheet_rels);
    parts.insert(
        "xl/tables/table1.xml",
        format!("<table xmlns=\"{MAIN}\" id=\"1\" name=\"T\" displayName=\"T\" ref=\"A1:B2\" connectionId=\"1\"/>"),
    );
    parts.insert(
        "xl/pivotCache/pivotCacheDefinition1.xml",
        format!("<pivotCacheDefinition xmlns=\"{MAIN}\"><cacheSource type=\"external\" connectionId=\"1\"/></pivotCacheDefinition>"),
    );
    parts.insert(
        "xl/connections.xml",
        format!("<connections xmlns=\"{MAIN}\"><connection id=\"1\" name=\"c\" type=\"5\"/></connections>"),
    );
    parts
}

#[test]
fn repeated_relationships_read_each_part_once() {
    // The review probe: before, 1,000 x 1,000 took seconds and hundreds of
    // megabytes, and the defaults ran out of memory.
    let parts = repeated_parts(10_000, 10_000, 10_000);
    let started = std::time::Instant::now();
    let report = inspect(&parts, &Limits::default());
    let spent = started.elapsed();
    eprintln!("10,000 sheets x 10,000 table relationships x 10,000 pivot caches: {spent:?}");
    assert!(spent.as_secs() < 10, "{spent:?}");
    let used_by = &report.connections[0].used_by;
    // One table (read once) and one use per pivot cache, capped.
    assert_eq!(used_by.len(), 10_000);
    assert!(
        problem_text(&report).contains("uses of connections"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn uses_are_capped() {
    let limits = Limits {
        max_items: 5,
        ..Limits::default()
    };
    let report = inspect(&repeated_parts(1, 1, 50), &limits);
    assert_eq!(report.connections[0].used_by.len(), 5);
    assert!(
        problem_text(&report).contains("Only the first 5 uses of connections"),
        "{}",
        problem_text(&report)
    );
}

#[test]
fn uses_at_the_cap_are_not_a_problem() {
    // One table and four pivot caches: five uses, exactly the cap.
    let limits = Limits {
        max_items: 5,
        ..Limits::default()
    };
    let report = inspect(&repeated_parts(1, 1, 4), &limits);
    assert_eq!(report.connections[0].used_by.len(), 5);
    assert!(report.problems.is_empty(), "{}", problem_text(&report));
}

#[test]
fn namespace_declarations_are_limited() {
    let many: String = (0..300).map(|i| format!(" xmlns:p{i}=\"u\"")).collect();
    let mut parts = MemoryParts::new();
    parts.insert(
        "xl/connections.xml",
        format!(
            "<connections xmlns=\"{MAIN}\"{many}><connection id=\"1\" name=\"x\"/></connections>"
        ),
    );
    let report = inspect(&parts, &Limits::default());
    assert!(
        problem_text(&report).contains("more than 256 namespaces"),
        "{}",
        problem_text(&report)
    );

    // Within the cap, many elements under many declarations stay fast.
    let mut text = String::new();
    for level in 0..16 {
        text.push_str("<a");
        for k in 0..15 {
            text.push_str(&format!(" xmlns:p{level}x{k}=\"u\""));
        }
        text.push('>');
    }
    text.push_str(&"<b/>".repeat(200_000));
    text.push_str(&"</a>".repeat(16));
    let mut parts = MemoryParts::new();
    parts.insert("xl/connections.xml", text);
    let started = std::time::Instant::now();
    let _ = inspect(&parts, &Limits::default());
    assert!(started.elapsed().as_secs() < 10, "{:?}", started.elapsed());
}

/// `links` external link parts with `names` sheet names each.
fn many_links(links: usize, names: usize) -> MemoryParts {
    let mut parts = MemoryParts::new();
    let mut sheet_names = String::new();
    for i in 0..names {
        sheet_names.push_str(&format!("<sheetName val=\"Sheet number {i}\"/>"));
    }
    let body = format!(
        "<externalLink xmlns=\"{MAIN}\"><externalBook><sheetNames>{sheet_names}</sheetNames></externalBook></externalLink>"
    );
    for i in 0..links {
        parts.insert(
            &format!("xl/externalLinks/externalLink{i}.xml"),
            body.clone(),
        );
    }
    parts
}

#[test]
fn the_whole_report_has_one_budget() {
    // Each part is within every per-part limit; together they are not.
    let parts = many_links(200, 200);

    let limits = Limits {
        max_total_items: 1_000,
        ..Limits::default()
    };
    let report = inspect(&parts, &limits);
    let names: usize = report
        .external_links
        .iter()
        .map(|l| l.sheet_names.len() + 1)
        .sum();
    assert!(names <= 1_000, "{names}");
    assert!(
        problem_text(&report).contains("reached Limits::max_total_items (1000 items)"),
        "{}",
        problem_text(&report)
    );

    let limits = Limits {
        max_total_chars: 10_000,
        ..Limits::default()
    };
    let report = inspect(&parts, &limits);
    let chars: usize = report
        .external_links
        .iter()
        .flat_map(|l| l.sheet_names.iter())
        .map(String::len)
        .sum();
    assert!(chars <= 10_000, "{chars}");

    let limits = Limits {
        max_total_xml_bytes: 50_000,
        ..Limits::default()
    };
    let report = inspect(&parts, &limits);
    assert!(report.external_links.len() < 200);
    assert!(
        problem_text(&report).contains("Reading stopped after 50000 bytes"),
        "{}",
        problem_text(&report)
    );
}
