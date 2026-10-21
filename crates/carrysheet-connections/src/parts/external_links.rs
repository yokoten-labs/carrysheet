//! External links (ECMA-376 Part 1, 18.14): other workbooks, DDE and OLE.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::hidden::{hidden, longer_than, too_long};
use super::safe;
use super::walk;
use crate::mask::{clip, mask_secrets};
use crate::package::Package;
use crate::package::rels::Relationship;
use crate::package::xml::Node;
use crate::report::{ExternalLink, LinkKind, LinkLocation, RawItem, Target};

/// Reads the external links the workbook lists, in formula order, then any
/// link part nothing points at.
pub(crate) fn parse_all(
    package: &mut Package<'_>,
    workbook_part: &str,
    references: &[String],
) -> Vec<ExternalLink> {
    let workbook_rels = package.relationships(workbook_part);
    let mut by_id: HashMap<&str, &Relationship> = HashMap::new();
    for rel in &workbook_rels {
        by_id.entry(rel.id.as_str()).or_insert(rel);
    }
    let budget = Rc::clone(&package.budget);
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (position, rel_id) in references.iter().enumerate() {
        if budget.output_spent() || budget.reading_spent() {
            break;
        }
        let part = by_id
            .get(rel_id.as_str())
            .and_then(|r| package.target(workbook_part, r));
        let index = u32::try_from(position + 1).unwrap_or(u32::MAX);
        match part {
            Some(part) if seen.insert(part.to_ascii_lowercase()) => {
                let link = parse(package, &part, index);
                if budget.output(link.text_len()) {
                    out.push(link);
                }
            }
            Some(part) => package.problem(
                Some(workbook_part),
                format!(
                    "External link [{index}] points at the same part as an earlier link; it is left out."
                ),
                format!("Two of the workbook's external references lead to {part}."),
                "Open the workbook in Excel and use Data > Edit Links to fix or remove the link.",
            ),
            None => package.problem(
                Some(workbook_part),
                format!(
                    "External link [{index}] points at a part that is missing; it is left out."
                ),
                "The workbook lists the link, but its relationship or part is gone.",
                "Open the workbook in Excel and use Data > Edit Links to fix or remove the link.",
            ),
        }
    }
    // Links nothing lists are numbered after the listed ones.
    let mut next_index = references.len();
    for part in package.names_like("xl/externallinks/", ".xml") {
        if budget.output_spent() || budget.reading_spent() {
            break;
        }
        if out.len() >= package.limits.max_items {
            package.too_many(&part, "external links");
            break;
        }
        if seen.insert(part.to_ascii_lowercase()) {
            next_index += 1;
            let index = u32::try_from(next_index).unwrap_or(u32::MAX);
            let mut link = parse(package, &part, index);
            link.notes.push(
                "The workbook does not list this link, so formulas cannot use it; its number is only a position in this list."
                    .to_string(),
            );
            if budget.output(link.text_len()) {
                out.push(link);
            }
        }
    }
    out
}

fn parse(package: &mut Package<'_>, part: &str, index: u32) -> ExternalLink {
    let max_name = package.limits.max_name_chars;
    let max_items = package.limits.max_items;
    let mut link = ExternalLink {
        index,
        kind: LinkKind::Unknown,
        target: None,
        location: None,
        sheet_names: Vec::new(),
        dde_service: None,
        dde_topic: None,
        program_id: None,
        items: Vec::new(),
        part: clip(part.to_string(), max_name),
        hidden: Vec::new(),
        raw: Vec::new(),
        notes: Vec::new(),
    };
    let include_raw = package.options.include_raw;
    // Raw texts of this link: what each is, and the text.
    let mut raw_texts: Vec<(&'static str, String)> = Vec::new();
    // Sheet names and items that are not plain text, counted.
    let mut hidden_names = 0usize;
    let Some(text) = package.read_xml(part) else {
        return link;
    };
    let mut rel_id: Option<String> = None;
    let mut overflow = false;
    let budget = Rc::clone(&package.budget);
    // Each sheet name and item is charged as it is read, so many link parts
    // with many names each cannot multiply past the budget.
    let keep = |list: &mut Vec<String>, name: &str, hidden_names: &mut usize| match safe::safe_text(
        name,
    ) {
        Ok(name) => {
            if budget.output(name.len()) {
                list.push(name);
            }
        }
        Err(_) => *hidden_names += 1,
    };
    walk(package, part, &text, |node| {
        let Node::Start(element, depth) = node else {
            return;
        };
        match (depth, element.name.as_str()) {
            (2, "externalBook") => {
                link.kind = LinkKind::Workbook;
                rel_id = element.prefixed_attr("id").map(str::to_string);
            }
            (2, "ddeLink") => {
                link.kind = LinkKind::Dde;
                link.dde_service = element
                    .attr("ddeService")
                    .and_then(|v| plain(&mut raw_texts, &mut link, "DDE service", v));
                if let Some(topic) = element.attr("ddeTopic") {
                    match safe::dde_topic(topic) {
                        Ok(topic) => link.dde_topic = Some(topic),
                        Err(why) => left_out(&mut raw_texts, &mut link, "DDE topic", topic, why),
                    }
                }
            }
            (2, "oleLink") => {
                link.kind = LinkKind::Ole;
                rel_id = element.prefixed_attr("id").map(str::to_string);
                link.program_id = element
                    .attr("progId")
                    .and_then(|v| plain(&mut raw_texts, &mut link, "OLE program", v));
            }
            (4, "sheetName") => {
                if link.sheet_names.len() < max_items {
                    if let Some(name) = element.attr("val") {
                        keep(&mut link.sheet_names, name, &mut hidden_names);
                    }
                } else {
                    overflow = true;
                }
            }
            (4, "ddeItem" | "oleItem") => {
                if link.items.len() < max_items {
                    if let Some(name) = element.attr("name") {
                        keep(&mut link.items, name, &mut hidden_names);
                    }
                } else {
                    overflow = true;
                }
            }
            _ => {}
        }
    });
    if overflow {
        package.too_many(part, "sheet names or items");
    }
    if hidden_names > 0 {
        link.notes.push(format!(
            "{hidden_names} sheet names or items are not shown: they are not plain text, or they mention a password or key."
        ));
    }
    if let Some(rel_id) = rel_id {
        let rels = package.relationships(part);
        match rels.iter().find(|r| r.id == rel_id) {
            Some(rel) => {
                let mut target = Target::default();
                if safe::set_location(&mut target, &rel.target) {
                    if target.hidden_path.is_some() {
                        raw_texts.push(("target", rel.target.clone()));
                    }
                    link.target = Some(target);
                } else {
                    raw_texts.push(("target", rel.target.clone()));
                    link.hidden.push(hidden(
                        "target",
                        &rel.target,
                        "because it is not a path, or a URL with a host this report recognizes",
                    ));
                }
                link.location = Some(location(rel));
            }
            None => link
                .notes
                .push("The link's target is missing from its relationships.".to_string()),
        }
    }
    if include_raw {
        // The raw texts replace the notes about them; one longer than the
        // name limit stays hidden whole, never cut.
        link.hidden.clear();
        for (what, text) in raw_texts {
            if longer_than(&text, max_name) {
                link.hidden.push(too_long(
                    what,
                    text.chars().count(),
                    "Limits::max_name_chars",
                    max_name,
                ));
            } else {
                link.raw.push(RawItem {
                    what: what.to_string(),
                    text: mask_secrets(&text),
                });
            }
        }
    }
    link
}

/// Shows a service or program id when it is plain text; otherwise records
/// it as left out.
fn plain(
    raw_texts: &mut Vec<(&'static str, String)>,
    link: &mut ExternalLink,
    what: &'static str,
    value: &str,
) -> Option<String> {
    match safe::safe_text(value) {
        Ok(value) => Some(value),
        Err(why) => {
            left_out(raw_texts, link, what, value, why);
            None
        }
    }
}

/// Records a text of the link that is not shown, and keeps it for the raw
/// texts.
fn left_out(
    raw_texts: &mut Vec<(&'static str, String)>,
    link: &mut ExternalLink,
    what: &'static str,
    value: &str,
    why: &str,
) {
    raw_texts.push((what, value.to_string()));
    link.hidden.push(hidden(what, value, why));
}

fn location(rel: &Relationship) -> LinkLocation {
    match rel.kind.as_str() {
        "externalLinkPath" | "oleObject" => LinkLocation::Path,
        "xlPathMissing" => LinkLocation::PathMissing,
        "xlStartup" => LinkLocation::Startup,
        "xlAlternateStartup" => LinkLocation::AlternateStartup,
        "xlLibrary" => LinkLocation::Library,
        _ => LinkLocation::Other,
    }
}
