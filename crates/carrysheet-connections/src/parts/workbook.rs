//! The workbook part and what takes data from connections: query tables
//! (ECMA-376 Part 1, 18.12), tables (18.5) and pivot caches (18.10).

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::safe;
use super::walk;
use crate::mask::clip;
use crate::package::Package;
use crate::package::budget::Budget;
use crate::package::rels::Relationship;
use crate::package::xml::Node;
use crate::report::{QueryTableSettings, Use, UseKind};

/// Longest defined-name formula kept; a query table's range is short.
const MAX_NAME_TEXT_BYTES: usize = 8 * 1024;

/// What `xl/workbook.xml` lists that matters here.
#[derive(Debug, Default)]
pub(crate) struct Workbook {
    /// (sheet name, relationship id), in tab order.
    pub sheets: Vec<(String, String)>,
    /// (name, sheet index for local names, formula text).
    pub defined_names: Vec<(String, Option<usize>, String)>,
    /// (cache id, relationship id).
    pub pivot_caches: Vec<(String, String)>,
    /// Relationship ids of external links, in formula order.
    pub external_references: Vec<String>,
}

pub(crate) fn parse_workbook(package: &mut Package<'_>, part: &str) -> Workbook {
    let Some(text) = package.read_xml(part) else {
        return Workbook::default();
    };
    let max = package.limits.max_items;
    let mut book = Workbook::default();
    let mut overflow = false;
    let mut name: Option<(String, Option<usize>, String)> = None;
    let mut name_cut = false;
    walk(package, part, &text, |node| match node {
        Node::Start(element, 3) => {
            let id = || element.prefixed_attr("id").unwrap_or_default().to_string();
            match element.name.as_str() {
                "sheet" if book.sheets.len() < max => {
                    book.sheets
                        .push((element.attr("name").unwrap_or_default().to_string(), id()));
                }
                "pivotCache" if book.pivot_caches.len() < max => {
                    book.pivot_caches.push((
                        element.attr("cacheId").unwrap_or_default().to_string(),
                        id(),
                    ));
                }
                "externalReference" if book.external_references.len() < max => {
                    book.external_references.push(id());
                }
                "definedName" if book.defined_names.len() < max => {
                    let local = element.attr("localSheetId").and_then(|v| v.parse().ok());
                    let entry = (
                        element.attr("name").unwrap_or_default().to_string(),
                        local,
                        String::new(),
                    );
                    if element.empty {
                        book.defined_names.push(entry);
                    } else {
                        name = Some(entry);
                    }
                }
                "sheet" | "pivotCache" | "externalReference" | "definedName" => overflow = true,
                _ => {}
            }
        }
        Node::Text(piece, 3) => {
            if let Some((_, _, text)) = name.as_mut()
                && !name_cut
            {
                if text.len() + piece.len() <= MAX_NAME_TEXT_BYTES {
                    text.push_str(&piece);
                } else {
                    // Keep the text whole up to here: no later piece is added
                    // after one is dropped.
                    name_cut = true;
                    text.push_str(" [cut]");
                }
            }
        }
        Node::End(3) => {
            if let Some(entry) = name.take() {
                book.defined_names.push(entry);
            }
            name_cut = false;
        }
        _ => {}
    });
    if overflow {
        package.too_many(part, "sheets, names, pivot caches or external references");
    }
    book
}

/// A use of a connection, with the connection id it names.
pub(crate) struct Found {
    pub connection_id: u32,
    pub item: Use,
}

/// The uses found so far, capped at `max_items` and charged to the budget.
struct Uses {
    found: Vec<Found>,
    max: usize,
    budget: Rc<Budget>,
    /// More uses were left out because of the cap.
    overflow: bool,
}

impl Uses {
    /// True once a use had to be left out; looking further is wasted work.
    /// Reaching the cap exactly is not enough: only a use that did not fit
    /// shows there were more.
    fn full(&self) -> bool {
        self.overflow || self.budget.output_spent()
    }

    fn push(&mut self, found: Found) {
        if self.found.len() >= self.max {
            self.overflow = true;
        } else if self.budget.output(found.item.text_len()) {
            self.found.push(found);
        }
    }
}

/// Defined names by lower-cased name, so finding a query table's range does
/// not scan every name.
struct NameIndex<'a> {
    book: &'a Workbook,
    by_name: HashMap<String, Vec<usize>>,
}

impl<'a> NameIndex<'a> {
    fn new(book: &'a Workbook) -> Self {
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, (name, _, _)) in book.defined_names.iter().enumerate() {
            by_name.entry(name.to_lowercase()).or_default().push(i);
        }
        Self { book, by_name }
    }

    /// The cells a query table fills: Excel names the range after the query
    /// table (`ExternalData_1`), as a name local to its sheet.
    fn range_for(
        &self,
        name: Option<&str>,
        sheet: Option<usize>,
        max_name: usize,
    ) -> Option<String> {
        let indexes = self.by_name.get(&name?.to_lowercase())?;
        let names = &self.book.defined_names;
        indexes
            .iter()
            .map(|&i| &names[i])
            .find(|(_, local, _)| sheet.is_none() || *local == sheet)
            .or_else(|| indexes.first().map(|&i| &names[i]))
            .and_then(|(_, _, text)| safe::reference(text))
            .map(|r| clip(r, max_name))
    }
}

/// Finds every query table, table and pivot cache that names a connection.
/// Query tables not reached through relationships are found by name too.
///
/// Each sheet, table, query table and pivot cache part is read once, however
/// many relationships point at it, and the uses are capped at `max_items`.
pub(crate) fn find_uses(
    package: &mut Package<'_>,
    workbook_part: &str,
    book: &Workbook,
) -> Vec<Found> {
    let mut uses = Uses {
        found: Vec::new(),
        max: package.limits.max_items,
        budget: Rc::clone(&package.budget),
        overflow: false,
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut seen_sheets: HashSet<String> = HashSet::new();
    let mut seen_tables: HashSet<String> = HashSet::new();
    let workbook_rels = package.relationships(workbook_part);
    let mut by_id: HashMap<&str, &Relationship> = HashMap::new();
    for rel in &workbook_rels {
        by_id.entry(rel.id.as_str()).or_insert(rel);
    }
    let names = NameIndex::new(book);
    let max_name = package.limits.max_name_chars;

    'sheets: for (index, (sheet_name, rel_id)) in book.sheets.iter().enumerate() {
        let Some(sheet_part) = by_id
            .get(rel_id.as_str())
            .and_then(|r| package.target(workbook_part, r))
        else {
            continue;
        };
        if !seen_sheets.insert(sheet_part.to_ascii_lowercase()) {
            continue;
        }
        for rel in package.relationships(&sheet_part) {
            if uses.full() {
                break 'sheets;
            }
            let Some(part) = package.target(&sheet_part, &rel) else {
                continue;
            };
            match rel.kind.as_str() {
                "queryTable" if seen.insert(part.to_ascii_lowercase()) => {
                    if let Some(mut item) = query_table(package, &part) {
                        item.item.sheet = Some(clip(sheet_name.clone(), max_name));
                        item.item.range =
                            names.range_for(item.raw_name.as_deref(), Some(index), max_name);
                        uses.push(item.into_found());
                    }
                }
                "table" if seen_tables.insert(part.to_ascii_lowercase()) => {
                    tables(package, &part, sheet_name, &mut seen, &mut uses);
                }
                _ => {}
            }
        }
    }

    let mut caches: HashMap<String, Option<u32>> = HashMap::new();
    for (cache_id, rel_id) in &book.pivot_caches {
        if uses.full() {
            break;
        }
        let Some(part) = by_id
            .get(rel_id.as_str())
            .and_then(|r| package.target(workbook_part, r))
        else {
            continue;
        };
        let connection = match caches.get(&part.to_ascii_lowercase()) {
            Some(known) => *known,
            None => {
                let read = pivot_cache_connection(package, &part);
                caches.insert(part.to_ascii_lowercase(), read);
                read
            }
        };
        if let Some(connection_id) = connection {
            uses.push(Found {
                connection_id,
                item: Use {
                    kind: UseKind::PivotCache,
                    name: Some(clip(format!("pivot cache {cache_id}"), max_name)),
                    sheet: None,
                    table: None,
                    range: None,
                    part: clip(part.clone(), max_name),
                    query_table: None,
                },
            });
        }
    }

    for part in package.names_like("xl/querytables/", ".xml") {
        if uses.full() {
            break;
        }
        if seen.insert(part.to_ascii_lowercase())
            && let Some(mut item) = query_table(package, &part)
        {
            item.item.range = names.range_for(item.raw_name.as_deref(), None, max_name);
            uses.push(item.into_found());
        }
    }
    if uses.overflow {
        let max = package.limits.max_items;
        package.problem(
            Some(workbook_part),
            format!("Only the first {max} uses of connections were listed."),
            format!(
                "The workbook's query tables, tables and pivot caches that take data from connections are more than Limits::max_items ({max})."
            ),
            "Raise Limits::max_items if the file is trusted.",
        );
    }
    uses.found
}

fn tables(
    package: &mut Package<'_>,
    table_part: &str,
    sheet_name: &str,
    seen: &mut HashSet<String>,
    uses: &mut Uses,
) {
    let max_name = package.limits.max_name_chars;
    let Some(text) = package.read_xml(table_part) else {
        return;
    };
    let mut info: Option<(Option<String>, Option<String>, Option<u32>)> = None;
    walk(package, table_part, &text, |node| {
        if let Node::Start(element, 1) = node {
            info = Some((
                element
                    .attr("displayName")
                    .or(element.attr("name"))
                    .map(str::to_string),
                element.attr("ref").map(str::to_string),
                element.number("connectionId"),
            ));
        }
    });
    let Some((table_name, table_ref, table_connection)) = info else {
        return;
    };
    let range = table_ref
        .and_then(|r| safe::reference(&format!("{}!{r}", quote_sheet(sheet_name))))
        .map(|r| clip(r, max_name));
    let mut fed_by_query_table = false;
    for rel in package.relationships(table_part) {
        if rel.kind != "queryTable" {
            continue;
        }
        let Some(part) = package.target(table_part, &rel) else {
            continue;
        };
        if !seen.insert(part.to_ascii_lowercase()) {
            continue;
        }
        if let Some(mut item) = query_table(package, &part) {
            item.item.sheet = Some(clip(sheet_name.to_string(), max_name));
            item.item.table = table_name.as_deref().map(|t| clip(t.to_string(), max_name));
            item.item.range = range.clone();
            uses.push(item.into_found());
            fed_by_query_table = true;
        }
    }
    if let Some(connection_id) = table_connection
        && !fed_by_query_table
    {
        uses.push(Found {
            connection_id,
            item: Use {
                kind: UseKind::Table,
                name: table_name.as_deref().map(|t| clip(t.to_string(), max_name)),
                sheet: Some(clip(sheet_name.to_string(), max_name)),
                table: table_name.as_deref().map(|t| clip(t.to_string(), max_name)),
                range,
                part: clip(table_part.to_string(), max_name),
                query_table: None,
            },
        });
    }
}

/// A query table read from its part, before the sheet and range are known.
struct QueryTable {
    connection_id: u32,
    raw_name: Option<String>,
    item: Use,
}

impl QueryTable {
    fn into_found(self) -> Found {
        Found {
            connection_id: self.connection_id,
            item: self.item,
        }
    }
}

fn query_table(package: &mut Package<'_>, part: &str) -> Option<QueryTable> {
    let max_name = package.limits.max_name_chars;
    let text = package.read_xml(part)?;
    let mut out = None;
    walk(package, part, &text, |node| {
        if let Node::Start(element, 1) = node
            && element.name == "queryTable"
        {
            let raw_name = element.attr("name").map(str::to_string);
            out = Some(QueryTable {
                connection_id: element.number("connectionId").unwrap_or(0),
                item: Use {
                    kind: UseKind::QueryTable,
                    name: raw_name.as_deref().map(|n| clip(n.to_string(), max_name)),
                    sheet: None,
                    table: None,
                    range: None,
                    part: clip(part.to_string(), max_name),
                    query_table: Some(QueryTableSettings {
                        refresh_on_load: element.flag("refreshOnLoad", false),
                        background_refresh: element.flag("backgroundRefresh", true),
                        refresh_disabled: element.flag("disableRefresh", false),
                        remove_data_on_save: element.flag("removeDataOnSave", false),
                    }),
                },
                raw_name,
            });
        }
    });
    if out.is_none() {
        package.problem(
            Some(part),
            "The query table part has no <queryTable> root element; it is left out.",
            "The part is empty or is not a query table.",
            "Open and save the workbook in Excel to repair it, then try again.",
        );
    }
    out
}

fn pivot_cache_connection(package: &mut Package<'_>, part: &str) -> Option<u32> {
    let text = package.read_xml(part)?;
    let mut out = None;
    walk(package, part, &text, |node| {
        if let Node::Start(element, 2) = node
            && element.name == "cacheSource"
        {
            out = element.number("connectionId").filter(|&id| id > 0);
        }
    });
    out
}

/// A sheet name as a formula writes it: quoted when it has anything but
/// letters, digits and `_`.
fn quote_sheet(name: &str) -> String {
    if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}
