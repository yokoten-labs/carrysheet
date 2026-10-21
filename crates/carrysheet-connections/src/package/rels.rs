//! Package relationships (ECMA-376 Part 2, Open Packaging Conventions).

use super::xml::{Node, XmlReader};

/// One `<Relationship>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Relationship {
    pub id: String,
    /// The last segment of the `Type` URI (`worksheet`, `externalLinkPath`,
    /// `xlPathMissing`), so transitional, strict and Microsoft namespaces
    /// all match.
    pub kind: String,
    /// The `Target` as written.
    pub target: String,
    /// `TargetMode="External"`: the target is outside the package.
    pub external: bool,
}

/// The relationships part that belongs to `part` (`""` for the package).
pub(crate) fn rels_name(part: &str) -> String {
    match part.rfind('/') {
        Some(slash) => format!("{}/_rels/{}.rels", &part[..slash], &part[slash + 1..]),
        None if part.is_empty() => "_rels/.rels".to_string(),
        None => format!("_rels/{part}.rels"),
    }
}

/// Parses a relationships part, keeping at most `max_items`.
pub(crate) fn parse(
    text: &str,
    max_depth: usize,
    max_items: usize,
) -> Result<(Vec<Relationship>, bool), String> {
    let mut reader = XmlReader::new(text, max_depth);
    let mut out = Vec::new();
    let mut truncated = false;
    while let Some(node) = reader.next()? {
        let Node::Start(element, 2) = node else {
            continue;
        };
        if element.name != "Relationship" {
            continue;
        }
        if out.len() >= max_items {
            truncated = true;
            continue;
        }
        let (Some(id), Some(kind), Some(target)) = (
            element.attr("Id"),
            element.attr("Type"),
            element.attr("Target"),
        ) else {
            continue;
        };
        out.push(Relationship {
            id: id.to_string(),
            kind: kind.rsplit('/').next().unwrap_or(kind).to_string(),
            target: target.to_string(),
            external: element
                .attr("TargetMode")
                .is_some_and(|m| m.eq_ignore_ascii_case("External")),
        });
    }
    Ok((out, truncated))
}

/// The part name an internal target points at, seen from `source` (the part
/// that owns the relationships). Returns `None` when the path climbs out of
/// the package.
pub(crate) fn resolve(source: &str, target: &str) -> Option<String> {
    let target = target.split(['#', '?']).next().unwrap_or(target);
    let mut segments: Vec<&str> = Vec::new();
    if !target.starts_with('/')
        && let Some(slash) = source.rfind('/')
    {
        segments.extend(source[..slash].split('/'));
    }
    for segment in target.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::{rels_name, resolve};

    #[test]
    fn names_and_targets() {
        assert_eq!(rels_name(""), "_rels/.rels");
        assert_eq!(rels_name("xl/workbook.xml"), "xl/_rels/workbook.xml.rels");
        assert_eq!(
            resolve("xl/workbook.xml", "worksheets/sheet1.xml").as_deref(),
            Some("xl/worksheets/sheet1.xml")
        );
        assert_eq!(
            resolve("xl/tables/table1.xml", "../queryTables/q.xml").as_deref(),
            Some("xl/queryTables/q.xml")
        );
        assert_eq!(
            resolve("xl/workbook.xml", "/xl/connections.xml").as_deref(),
            Some("xl/connections.xml")
        );
        assert_eq!(
            resolve("", "xl/workbook.xml").as_deref(),
            Some("xl/workbook.xml")
        );
        assert_eq!(resolve("xl/workbook.xml", "../../x"), None);
    }
}
