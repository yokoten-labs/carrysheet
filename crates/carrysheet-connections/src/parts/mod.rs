//! Finding and reading the parts that describe connections.

use std::collections::HashMap;

mod connections;
mod external_links;
pub(crate) mod hidden;
pub(crate) mod safe;
pub(crate) mod target;
mod workbook;

use crate::mashup;
use crate::mask::clip;
use crate::package::Package;
use crate::package::xml::{Node, XmlReader};
use crate::report::{DataModel, Report};

/// Runs `visit` on every node of an XML part. A malformed part is recorded as
/// a problem; nodes before the fault have been visited.
pub(crate) fn walk(package: &mut Package<'_>, part: &str, text: &str, mut visit: impl FnMut(Node)) {
    let mut reader = XmlReader::new(text, package.limits.max_depth);
    loop {
        match reader.next() {
            Ok(Some(node)) => visit(node),
            Ok(None) => return,
            Err(why) => {
                package.xml_problem(part, &why);
                return;
            }
        }
    }
}

/// The internal part a relationship of `source` with this kind points at.
fn related(package: &mut Package<'_>, source: &str, kind: &str) -> Vec<String> {
    let rels = package.relationships(source);
    rels.iter()
        .filter(|r| r.kind == kind)
        .filter_map(|r| package.target(source, r))
        .collect()
}

pub(crate) fn inspect(package: &mut Package<'_>) -> Report {
    let workbook_part = related(package, "", "officeDocument")
        .into_iter()
        .next()
        .or_else(|| package.find("xl/workbook.xml"))
        .unwrap_or_else(|| "xl/workbook.xml".to_string());
    let book = workbook::parse_workbook(package, &workbook_part);

    let connections_part = related(package, &workbook_part, "connections")
        .into_iter()
        .next()
        .or_else(|| package.find("xl/connections.xml"));
    let mut connections = match connections_part {
        Some(part) => connections::parse(package, &part),
        None => Vec::new(),
    };

    // The first connection with an id gets its uses, as Excel would.
    let mut by_id: HashMap<u32, usize> = HashMap::new();
    for (i, connection) in connections.iter().enumerate() {
        by_id.entry(connection.id).or_insert(i);
    }
    for found in workbook::find_uses(package, &workbook_part, &book) {
        match by_id
            .get(&found.connection_id)
            .map(|&i| &mut connections[i])
        {
            Some(connection) if connection.used_by.len() < package.limits.max_items => {
                connection.used_by.push(found.item);
            }
            Some(_) => {}
            None => {
                let part = found.item.part.clone();
                package.problem(
                    Some(&part),
                    format!(
                        "It takes data from connection {}, which the workbook does not have.",
                        found.connection_id
                    ),
                    "The connection was deleted or the file was edited by hand.",
                    "In Excel, open Data > Queries & Connections and point it at a connection, or delete it.",
                );
            }
        }
    }

    let external_links =
        external_links::parse_all(package, &workbook_part, &book.external_references);

    let mut custom = related(package, &workbook_part, "customXml");
    for name in package.names_like("customxml/item", ".xml") {
        let lower = name.to_ascii_lowercase();
        if !lower.contains("itemprops") && !custom.iter().any(|c| c.eq_ignore_ascii_case(&name)) {
            custom.push(name);
        }
    }
    let power_query = mashup::find(package, &custom);

    let data_model = related(package, &workbook_part, "model")
        .into_iter()
        .next()
        .or_else(|| package.find("xl/model/item.data"))
        .map(|part| DataModel {
            size_bytes: package.size(&part),
            part: clip(part.clone(), package.limits.max_name_chars),
        });

    package.report_budgets();
    let warnings = if package.options.include_raw {
        vec!["Raw texts were asked for (Options::include_raw). They are masked as well as patterns can do it, which is best effort: they may still contain passwords or other secrets.".to_string()]
    } else {
        Vec::new()
    };
    Report {
        warnings,
        connections,
        external_links,
        power_query,
        data_model,
        problems: package.take_problems(),
    }
}
