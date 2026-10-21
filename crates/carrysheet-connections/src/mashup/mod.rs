//! Power Query: the DataMashup in a customXml part ([MS-QDEFF]).
//!
//! The customXml item holds `<DataMashup>` with base64 text. Decoded, it is a
//! little-endian stream: a 4-byte version (0), then four length-prefixed
//! blocks: package parts (a zip), permissions, metadata and permission
//! bindings. The queries live in the zip as `Formulas/Section1.m`. Only that
//! far is read: the metadata and the bindings are measured, not parsed.

mod calls;
mod section;
mod zip;

use crate::mask::{clean, clip, mask_text_owned};
use crate::package::xml::{Node, XmlReader};
use crate::package::{Limit, Package};
use crate::parts::hidden::{hidden, longer_than, too_long};
use crate::report::{PowerQuery, Query};

/// Looks through `candidates` (customXml parts) for the DataMashup.
pub(crate) fn find(package: &mut Package<'_>, candidates: &[String]) -> Option<PowerQuery> {
    let limits = package.limits.clone();
    // All customXml parts read together stay within the mashup limit.
    let mut budget = limits.max_mashup_bytes;
    let mut found: Option<PowerQuery> = None;
    for part in candidates {
        if budget == 0 {
            package.problem(
                Some(part),
                "This customXml part and those after it were not read.",
                "The customXml parts together are larger than the DataMashup limit.",
                "Raise Limits::max_mashup_bytes if the file is trusted.",
            );
            break;
        }
        let limit = Limit {
            name: "Limits::max_mashup_bytes",
            value: limits.max_mashup_bytes,
        };
        let Some(bytes) = package.read(part, budget, limit) else {
            continue;
        };
        budget = budget.saturating_sub(bytes.len() as u64);
        let size = bytes.len() as u64;
        // UTF-8 text takes over the bytes' memory; UTF-16 is converted and
        // its bytes freed inside.
        let Some(text) = package.decode(part, bytes) else {
            continue;
        };
        // The base64 is decoded as the reader hands it over, so it is never
        // held as text next to the part.
        let stream = match mashup_stream(&text, limits.max_depth) {
            Ok(Some(stream)) => stream,
            Ok(None) => continue,
            Err(why) => {
                package.xml_problem(part, &why);
                continue;
            }
        };
        drop(text);
        if found.is_some() {
            package.problem(
                Some(part),
                "A second DataMashup was found and left out.",
                "Excel keeps one Power Query package per workbook.",
                "Open and save the workbook in Excel; it keeps the one it uses.",
            );
            continue;
        }
        found = Some(read_mashup(package, part, size, stream));
    }
    found
}

/// The decoded stream of a `<DataMashup>` root, `None` when the root is
/// something else, or why the XML or the base64 in it is bad.
fn mashup_stream(text: &str, max_depth: usize) -> Result<Option<Result<Vec<u8>, String>>, String> {
    let mut reader = XmlReader::new(text, max_depth);
    let mut decoder = Base64::with_capacity(0);
    let mut in_mashup = false;
    while let Some(node) = reader.next()? {
        match node {
            Node::Start(element, 1) => {
                if element.name != "DataMashup" {
                    return Ok(None);
                }
                in_mashup = true;
                decoder.out.reserve(text.len() / 4 * 3);
            }
            Node::Text(piece, 1) if in_mashup => {
                if let Err(why) = decoder.push(&piece) {
                    return Ok(Some(Err(why)));
                }
            }
            _ => {}
        }
    }
    Ok(in_mashup.then(|| decoder.finish()))
}

fn read_mashup(
    package: &mut Package<'_>,
    part: &str,
    size: u64,
    stream: Result<Vec<u8>, String>,
) -> PowerQuery {
    let limits = package.limits.clone();
    let mut power_query = PowerQuery {
        part: clean(part, limits.max_name_chars),
        size_bytes: size,
        package_bytes: 0,
        queries: Vec::new(),
        notes: Vec::new(),
    };
    let fault = |package: &mut Package<'_>, why: String| {
        package.problem(
            Some(part),
            "The Power Query package could not be read; its queries are not listed.",
            why,
            "Open the workbook in Excel, open Power Query, and save again.",
        );
    };
    let stream = match stream {
        Ok(stream) => stream,
        Err(why) => {
            fault(package, why);
            return power_query;
        }
    };
    power_query.package_bytes = stream.len() as u64;
    let found = formula_members(&stream, &limits);
    // The members own their text; the decoded package is no longer needed
    // while they are masked.
    drop(stream);
    power_query.notes.extend(found.notes);
    for f in found.faults {
        match f {
            Fault::Bad(why) => fault(package, why),
            Fault::Limit { what, why, fix } => package.problem(Some(part), what, why, fix),
        }
    }
    let include_raw = package.options.include_raw;
    for member in found.members {
        let (calls, other_calls) = calls::calls(&member.formula, limits.max_items);
        // A formula is never cut: a cut can end inside a secret, where
        // masking no longer recognizes it. One over the limit stays hidden.
        let max = limits.max_formula_chars;
        let (hidden, formula) = if longer_than(&member.formula, max) {
            let characters = member.formula.chars().count();
            let note = too_long("M formula", characters, "Limits::max_formula_chars", max);
            (Some(note), None)
        } else if include_raw {
            (None, Some(mask_text_owned(member.formula)))
        } else {
            let note = hidden(
                "M formula",
                &member.formula,
                "because it may contain passwords or other secrets; its calls are listed",
            );
            (Some(note), None)
        };
        let query = Query {
            // The query's name is how people find it: shown as written.
            name: clip(member.name, limits.max_name_chars),
            calls,
            other_calls,
            hidden,
            formula,
        };
        if !package.budget.output(query.text_len()) {
            break;
        }
        power_query.queries.push(query);
    }
    if found.more {
        package.too_many(part, "Power Query queries");
    }
    power_query
}

/// What the formula files in a decoded package hold, before masking.
#[derive(Default)]
struct Members {
    members: Vec<section::Member>,
    /// More queries than `max_items`.
    more: bool,
    notes: Vec<String>,
    /// Why parts of the package could not be read.
    faults: Vec<Fault>,
}

enum Fault {
    /// The package is damaged.
    Bad(String),
    /// A limit was reached; the problem names it.
    Limit {
        what: String,
        why: String,
        fix: String,
    },
}

fn formula_members(stream: &[u8], limits: &crate::Limits) -> Members {
    let mut found = Members::default();
    let parts = match package_parts(stream) {
        Ok((parts, notes)) => {
            found.notes = notes;
            parts
        }
        Err(why) => {
            found.faults.push(Fault::Bad(why));
            return found;
        }
    };
    let entries = match zip::entries(parts, limits.max_zip_entries, limits.max_name_chars) {
        Ok(entries) => entries,
        Err(zip::ReadError::TooMany { count }) => {
            let max = limits.max_zip_entries;
            found.faults.push(Fault::Limit {
                what: "The Power Query package was not read; its queries are not listed."
                    .to_string(),
                why: format!(
                    "Its zip lists {count} entries, more than Limits::max_zip_entries ({max})."
                ),
                fix: "Raise Limits::max_zip_entries if the file is trusted.".to_string(),
            });
            return found;
        }
        Err(zip::ReadError::Bad(why)) => {
            found.faults.push(Fault::Bad(why));
            return found;
        }
        // Reading the directory inflates nothing, so this does not happen.
        Err(zip::ReadError::TooLarge { .. }) => {
            found
                .faults
                .push(Fault::Bad("the zip directory is damaged".to_string()));
            return found;
        }
    };
    let mut budget = limits.max_zip_total_bytes;
    for entry in entries.iter().filter(|e| {
        let lower = e.name.to_ascii_lowercase();
        lower.starts_with("formulas/") && lower.ends_with(".m")
    }) {
        if budget == 0 {
            found
                .faults
                .push(total_limit(&entry.label, limits.max_zip_total_bytes));
            break;
        }
        let cap = limits.max_zip_entry_bytes.min(budget);
        let bytes = match zip::read(parts, entry, cap) {
            Ok(bytes) => bytes,
            Err(zip::ReadError::Bad(why)) => {
                found.faults.push(Fault::Bad(why));
                continue;
            }
            // The entry limit decides when it is the smaller one; otherwise
            // what is left of the total did.
            Err(zip::ReadError::TooLarge { size }) if cap == limits.max_zip_entry_bytes => {
                let max = limits.max_zip_entry_bytes;
                let size = size.map_or_else(
                    || format!("inflates to more than {max} bytes"),
                    |s| format!("is {s} bytes"),
                );
                found.faults.push(Fault::Limit {
                    what: format!("The formula file {} was skipped.", entry.label),
                    why: format!("It {size}, and Limits::max_zip_entry_bytes is {max} bytes."),
                    fix: "Raise Limits::max_zip_entry_bytes if the file is trusted.".to_string(),
                });
                continue;
            }
            Err(zip::ReadError::TooLarge { .. } | zip::ReadError::TooMany { .. }) => {
                found
                    .faults
                    .push(total_limit(&entry.label, limits.max_zip_total_bytes));
                break;
            }
        };
        budget = budget.saturating_sub(bytes.len() as u64);
        let text = String::from_utf8_lossy(&bytes);
        let room = limits.max_items.saturating_sub(found.members.len());
        let (members, cut) = section::members(&text, room);
        found.more |= cut;
        found.members.extend(members);
    }
    found
}

/// The fault for formula files that together pass the total limit.
fn total_limit(entry: &str, max: u64) -> Fault {
    Fault::Limit {
        what: format!("The formula file {entry} and those after it were skipped."),
        why: format!(
            "The formula files together inflate to more than Limits::max_zip_total_bytes ({max} bytes)."
        ),
        fix: "Raise Limits::max_zip_total_bytes if the file is trusted.".to_string(),
    }
}

/// Checks the [MS-QDEFF] header and returns the package parts zip, with
/// notes about the other blocks.
fn package_parts(stream: &[u8]) -> Result<(&[u8], Vec<String>), String> {
    let mut at = 0usize;
    let mut next_u32 = |what: &str| -> Result<u32, String> {
        let bytes = stream
            .get(at..at + 4)
            .ok_or_else(|| format!("the package ends before its {what}"))?;
        at += 4;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    };
    let version = next_u32("version")?;
    if version != 0 {
        return Err(format!(
            "the package has version {version}; only version 0 is defined"
        ));
    }
    let parts_len = next_u32("package parts length")?;
    let start = 8usize;
    let end = start
        .checked_add(
            usize::try_from(parts_len).map_err(|_| "the package parts length is too large")?,
        )
        .filter(|&end| end <= stream.len())
        .ok_or("the package parts length runs past the end of the package")?;
    let mut notes = Vec::new();
    // The three blocks after the zip: each a length and its bytes.
    let mut at = end;
    for block in ["permissions", "metadata", "permission bindings"] {
        let Some(len) = stream
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        else {
            notes.push(format!("The package ends before its {block}."));
            break;
        };
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        match at.checked_add(4).and_then(|a| a.checked_add(len)) {
            Some(after) if after <= stream.len() => at = after,
            _ => {
                notes.push(format!("The package's {block} block runs past its end."));
                break;
            }
        }
    }
    Ok((&stream[start..end], notes))
}

/// Decodes standard base64 a piece at a time, skipping whitespace. The
/// output is at most three quarters of the input, which the caller has
/// already bounded.
struct Base64 {
    out: Vec<u8>,
    buffer: u32,
    bits: u32,
    padding: usize,
    position: usize,
}

impl Base64 {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            out: Vec::with_capacity(capacity),
            buffer: 0,
            bits: 0,
            padding: 0,
            position: 0,
        }
    }

    fn push(&mut self, text: &str) -> Result<(), String> {
        fn value(c: u8) -> Option<u32> {
            match c {
                b'A'..=b'Z' => Some(u32::from(c - b'A')),
                b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
                b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        }
        for c in text.bytes() {
            let position = self.position;
            self.position += 1;
            if c.is_ascii_whitespace() {
                continue;
            }
            if c == b'=' {
                self.padding += 1;
                continue;
            }
            if self.padding > 0 {
                return Err(format!(
                    "the base64 text has data after its padding (at character {position})"
                ));
            }
            let v = value(c).ok_or_else(|| {
                format!("the base64 text has a character it may not have (at character {position})")
            })?;
            self.buffer = (self.buffer << 6) | v;
            self.bits += 6;
            if self.bits >= 8 {
                self.bits -= 8;
                self.out
                    .push(u8::try_from((self.buffer >> self.bits) & 0xFF).unwrap_or(0));
                self.buffer &= (1 << self.bits) - 1;
            }
        }
        Ok(())
    }

    fn finish(self) -> Result<Vec<u8>, String> {
        if self.padding > 2 {
            return Err("the base64 text has too much padding".to_string());
        }
        Ok(self.out)
    }
}

#[cfg(test)]
fn decode_base64(text: &str) -> Result<Vec<u8>, String> {
    let mut decoder = Base64::with_capacity(text.len());
    decoder.push(text)?;
    decoder.finish()
}

#[cfg(test)]
mod tests {
    use super::{decode_base64, package_parts};

    #[test]
    fn base64() {
        assert_eq!(decode_base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(decode_base64("aGVs\r\nbG8h").unwrap(), b"hello!");
        assert_eq!(decode_base64("").unwrap(), b"");
        assert!(decode_base64("aG=Vs").is_err());
        assert!(decode_base64("a*b").is_err());
    }

    #[test]
    fn header() {
        let mut stream = vec![0, 0, 0, 0, 3, 0, 0, 0, 1, 2, 3];
        stream.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let (parts, notes) = package_parts(&stream).unwrap();
        assert_eq!(parts, [1, 2, 3]);
        assert!(notes.is_empty());
        assert!(package_parts(&[1, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(package_parts(&[0, 0, 0, 0, 9, 0, 0, 0, 1]).is_err());
        assert!(package_parts(&[0, 0]).is_err());
    }
}
