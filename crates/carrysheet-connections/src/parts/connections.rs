//! `xl/connections.xml` (ECMA-376 Part 1, 18.13; the `x15:connection`
//! extension from MS-XLSX).

//!
//! By default only recognized fields are kept (see `target.rs` and
//! `safe.rs`); the raw texts are listed as hidden, except SQL command text
//! that is a plain read (see `hidden.rs`).

use super::hidden::{hidden, longer_than, plain_object_name, sql_verdict, too_long};
use super::{safe, target, walk};
use crate::Limits;
use crate::mask::{clip, mask_connection_string, mask_secrets};
use crate::package::Package;
use crate::package::xml::{Element, Node};
use crate::report::{
    CommandType, Connection, ConnectionKind, Credentials, RawText, Refresh, Target,
};

/// What one `<connection>` says, before its kind is decided. The raw texts
/// stay inside the crate; `finish` decides what of them is shown.
struct Draft {
    connection: Connection,
    description: Option<String>,
    connection_string: Option<String>,
    command: Option<String>,
    url: Option<String>,
    source_file: Option<String>,
    connection_file: Option<String>,
    has_olap: bool,
    has_web: bool,
    has_text: bool,
    model: bool,
    parameters: usize,
}

pub(crate) fn parse(package: &mut Package<'_>, part: &str) -> Vec<Connection> {
    let Some(text) = package.read_xml(part) else {
        return Vec::new();
    };
    let limits = package.limits.clone();
    let include_raw = package.options.include_raw;
    let budget = std::rc::Rc::clone(&package.budget);
    let mut out: Vec<Connection> = Vec::new();
    let mut current: Option<Draft> = None;
    let mut skipped = 0usize;
    let mut count = 0usize;
    let mut keep = |connection: Connection| {
        if budget.output(connection.text_len()) {
            out.push(connection);
        }
    };
    walk(package, part, &text, |node| match node {
        Node::Start(element, 2) if element.name == "connection" && element.is_main() => {
            if count >= limits.max_items {
                skipped += 1;
                current = None;
                return;
            }
            count += 1;
            current = Some(start(&element, &limits));
            if element.empty
                && let Some(draft) = current.take()
            {
                keep(finish(draft, &limits, include_raw));
            }
        }
        Node::Start(element, depth) if depth >= 3 => {
            if let Some(draft) = current.as_mut() {
                child(draft, &element);
            }
        }
        Node::End(2) => {
            if let Some(draft) = current.take() {
                keep(finish(draft, &limits, include_raw));
            }
        }
        _ => {}
    });
    drop(keep);
    if skipped > 0 {
        package.too_many(part, "connections");
    }
    out
}

fn start(element: &Element, limits: &Limits) -> Draft {
    let name_max = limits.max_name_chars;
    let mut notes = Vec::new();
    if element.flag("deleted", false) {
        notes.push("Marked as deleted: Excel keeps it but nothing uses it.".to_string());
    }
    if element.flag("onlyUseConnectionFile", false) {
        notes.push("Always reads its settings from the connection file.".to_string());
    }
    if element.flag("new", false) {
        notes.push("Never refreshed yet.".to_string());
    }
    Draft {
        connection: Connection {
            id: element.number("id").unwrap_or(0),
            // The connection's own name is how people find it; it is shown
            // as written, only cut.
            name: element
                .attr("name")
                .map(|v| clip(v.to_string(), name_max))
                .unwrap_or_default(),
            kind: ConnectionKind::Other,
            type_number: element.number("type"),
            target: Target::default(),
            auth: Vec::new(),
            command_type: None,
            command_text: None,
            hidden: Vec::new(),
            raw: None,
            connection_file: None,
            refresh: Refresh {
                on_load: element.flag("refreshOnLoad", false),
                background: element.flag("background", false),
                interval_minutes: element.number("interval").filter(|&m| m > 0),
                save_data: element.flag("saveData", false),
                save_password: element.flag("savePassword", false),
                credentials: match element.attr("credentials") {
                    Some("none") => Credentials::None,
                    Some("stored") => Credentials::Stored,
                    Some("prompt") => Credentials::Prompt,
                    _ => Credentials::Integrated,
                },
                keep_alive: element.flag("keepAlive", false),
            },
            used_by: Vec::new(),
            notes,
        },
        description: element.attr("description").map(str::to_string),
        connection_string: None,
        command: None,
        url: None,
        source_file: element.attr("sourceFile").map(str::to_string),
        connection_file: element.attr("odcFile").map(str::to_string),
        has_olap: false,
        has_web: false,
        has_text: false,
        model: false,
        parameters: 0,
    }
}

fn child(draft: &mut Draft, element: &Element) {
    let c = &mut draft.connection;
    match (element.name.as_str(), !element.is_main()) {
        ("dbPr", false) | ("oledbPr" | "dataFeedPr", true) => {
            if draft.connection_string.is_none() {
                draft.connection_string = element.attr("connection").map(str::to_string);
            }
            if draft.command.is_none()
                && let Some(command) = element.attr("command")
            {
                draft.command = Some(command.to_string());
                // CT_DbPr says commandType is 2 (SQL) when it is absent.
                let default = (element.name == "dbPr").then_some(2);
                c.command_type = Some(match element.number("commandType").or(default) {
                    Some(1) => CommandType::Cube,
                    Some(2) => CommandType::Sql,
                    Some(3) => CommandType::Table,
                    Some(4) => CommandType::Default,
                    Some(5) => CommandType::WebList,
                    _ => CommandType::Unknown,
                });
            }
        }
        ("dbCommand", true) => {
            if draft.command.is_none()
                && let Some(command) = element.attr("text")
            {
                draft.command = Some(command.to_string());
                c.command_type = Some(CommandType::Unknown);
            }
        }
        ("olapPr", false) => draft.has_olap = true,
        ("webPr", false) => {
            draft.has_web = true;
            if draft.url.is_none() {
                draft.url = element.attr("url").map(str::to_string);
            }
            if draft.command.is_none()
                && let Some(post) = element.attr("post")
            {
                draft.command = Some(post.to_string());
                c.command_type = Some(CommandType::Post);
            }
            if element.flag("xml", false) {
                c.notes.push("Reads an XML source.".to_string());
            }
        }
        ("textPr", _) | ("modelTextPr", true) => {
            draft.has_text = true;
            if draft.source_file.is_none() {
                draft.source_file = element.attr("sourceFile").map(str::to_string);
            }
            if element.flag("prompt", true) && element.name == "textPr" && element.is_main() {
                c.notes
                    .push("Asks for the file name on refresh.".to_string());
            }
        }
        ("connection", true) => {
            draft.model |= element.flag("model", false);
            if element.flag("excludeFromRefreshAll", false) {
                c.notes.push("Left out of Refresh All.".to_string());
            }
        }
        ("rangePr", true) => {
            if c.target.table.is_none() {
                c.target.table = element.attr("sourceName").and_then(safe::name);
            }
        }
        ("parameter", false) => draft.parameters += 1,
        _ => {}
    }
}

fn finish(draft: Draft, limits: &Limits, include_raw: bool) -> Connection {
    let mut c = draft.connection.clone();
    let parsed = draft
        .connection_string
        .as_deref()
        .map(target::parse)
        .unwrap_or_default();
    merge(&mut c.target, parsed.target);
    c.auth = parsed.auth;
    if let Some(url) = draft.url.as_deref() {
        safe::set_url(&mut c.target, url);
    }
    // Paths that are not shown, as written: neither a path nor a URL this
    // report recognizes. They are listed as hidden like any other text.
    let mut unshown: Unshown = (None, None);
    if let Some(path) = draft.source_file.as_deref()
        && !safe::set_location(&mut c.target, path)
    {
        unshown.0 = Some(path);
    }
    if let Some(path) = draft.connection_file.as_deref() {
        let mut t = Target::default();
        if safe::set_location(&mut t, path) {
            c.connection_file = Some(t);
        } else {
            unshown.1 = Some(path);
        }
    }
    command(&mut c, draft.command.as_deref(), limits, include_raw);
    if include_raw {
        raw(&mut c, &draft, unshown, limits);
    } else {
        describe_hidden(&mut c, &draft, unshown);
    }
    c.kind = kind(&c, &draft, &parsed.provider, parsed.embedded);
    if draft.parameters > 0 {
        c.notes
            .push(format!("Takes {} parameters.", draft.parameters));
    }
    c
}

/// Adds the raw texts, masked. A text longer than its limit is not cut,
/// since a cut can end inside a secret where masking no longer sees it: it
/// stays hidden whole, and the note names the limit.
fn raw(c: &mut Connection, draft: &Draft, unshown: Unshown<'_>, limits: &Limits) {
    let mut raw = RawText::default();
    let mut add = |slot: &mut Option<String>,
                   text: Option<&str>,
                   what: &str,
                   field: &str,
                   max: usize,
                   mask: fn(&str) -> String| {
        let Some(text) = text else {
            return;
        };
        if longer_than(text, max) {
            c.hidden
                .push(too_long(what, text.chars().count(), field, max));
        } else {
            *slot = Some(mask(text));
        }
    };
    let name_field = "Limits::max_name_chars";
    let command_field = "Limits::max_command_chars";
    add(
        &mut raw.description,
        draft.description.as_deref(),
        "description",
        name_field,
        limits.max_name_chars,
        mask_secrets,
    );
    add(
        &mut raw.connection_string,
        draft.connection_string.as_deref(),
        "connection string",
        command_field,
        limits.max_command_chars,
        mask_connection_string,
    );
    add(
        &mut raw.command_text,
        draft.command.as_deref(),
        "command",
        command_field,
        limits.max_command_chars,
        mask_secrets,
    );
    add(
        &mut raw.url,
        draft.url.as_deref(),
        "full URL",
        name_field,
        limits.max_name_chars,
        mask_secrets,
    );
    add(
        &mut raw.source_file,
        unshown.0,
        "source file",
        name_field,
        limits.max_name_chars,
        mask_secrets,
    );
    add(
        &mut raw.connection_file,
        unshown.1,
        "connection file",
        name_field,
        limits.max_name_chars,
        mask_secrets,
    );
    c.raw = Some(raw);
}

/// Fills the empty fields of `into` from `from`.
fn merge(into: &mut Target, from: Target) {
    let pairs = [
        (&mut into.server, from.server),
        (&mut into.database, from.database),
        (&mut into.url, from.url),
        (&mut into.provider, from.provider),
        (&mut into.data_source_name, from.data_source_name),
        (&mut into.query, from.query),
        (&mut into.table, from.table),
    ];
    for (slot, value) in pairs {
        if slot.is_none() {
            *slot = value;
        }
    }
    if into.file.is_none() {
        into.file = from.file;
    }
    if into.hidden_path.is_none() {
        into.hidden_path = from.hidden_path;
    }
    into.port = into.port.or(from.port);
}

/// Shows a table or cube name as the target's table, and SQL whole when it
/// is a plain read; anything else is hidden whole. With `include_raw` the
/// hidden command is in `raw` instead.
fn command(c: &mut Connection, command: Option<&str>, limits: &Limits, include_raw: bool) {
    let Some(text) = command else {
        return;
    };
    let max = limits.max_command_chars;
    let what = match c.command_type {
        Some(CommandType::Table | CommandType::Cube) => {
            match safe::object_name(text).map(|n| plain_object_name(&n).map(|()| n)) {
                Some(Ok(name)) => {
                    if c.target.table.is_none() {
                        c.target.table = Some(name);
                    }
                    return;
                }
                Some(Err(why)) => ("table or cube name", why),
                None => ("command", "because it is not a plain table or cube name"),
            }
        }
        Some(CommandType::Sql) if longer_than(text, max) => {
            if !include_raw {
                c.hidden.push(too_long(
                    "SQL command",
                    text.chars().count(),
                    "Limits::max_command_chars",
                    max,
                ));
            }
            return;
        }
        Some(CommandType::Sql) => match sql_verdict(text) {
            Ok(()) => {
                c.command_text = Some(text.to_string());
                return;
            }
            Err(why) => ("SQL command", why),
        },
        Some(CommandType::Post) => (
            "POST data",
            "because it is sent as is and may contain a password or other secret",
        ),
        _ => (
            "command",
            "because it may contain a password or other secret",
        ),
    };
    if !include_raw {
        c.hidden.push(hidden(what.0, text, what.1));
    }
}

/// The source file and connection file, as written, when they are not shown.
type Unshown<'a> = (Option<&'a str>, Option<&'a str>);

const NOT_A_PATH: &str = "because it is not a path or a URL this report recognizes";

/// Lists the raw texts left out.
fn describe_hidden(c: &mut Connection, draft: &Draft, unshown: Unshown<'_>) {
    if let Some(path) = unshown.0 {
        c.hidden.push(hidden("source file", path, NOT_A_PATH));
    }
    if let Some(path) = unshown.1 {
        c.hidden.push(hidden("connection file", path, NOT_A_PATH));
    }
    if let Some(text) = draft.connection_string.as_deref() {
        c.hidden.push(hidden(
            "connection string",
            text,
            "because it may contain a password or other secret; the fields above are read from it",
        ));
    }
    if let Some(text) = draft.url.as_deref() {
        c.hidden.push(hidden(
            "full URL",
            text,
            "because its user information, query and fragment may contain secrets; the scheme, host and path are shown",
        ));
    }
    if let Some(text) = draft.description.as_deref() {
        c.hidden.push(hidden(
            "description",
            text,
            "because it is free text that may contain secrets",
        ));
    }
}

fn kind(c: &Connection, draft: &Draft, provider: &str, embedded: bool) -> ConnectionKind {
    if provider.contains("microsoft.mashup.oledb") {
        ConnectionKind::PowerQuery
    } else if draft.model || embedded || c.name == "ThisWorkbookDataModel" {
        ConnectionKind::DataModel
    } else if c.target.table.is_some() && c.command_type.is_none() {
        ConnectionKind::Worksheet
    } else if draft.has_olap || provider.starts_with("msolap") {
        ConnectionKind::Olap
    } else {
        match c.type_number {
            Some(1) => ConnectionKind::Odbc,
            Some(2) => ConnectionKind::Dao,
            Some(3) => ConnectionKind::FileDatabase,
            Some(4) => ConnectionKind::WebQuery,
            Some(5) => ConnectionKind::OleDb,
            Some(6) => ConnectionKind::TextFile,
            Some(7) => ConnectionKind::Ado,
            Some(8) => ConnectionKind::Dsp,
            _ if draft.has_web => ConnectionKind::WebQuery,
            _ if draft.has_text => ConnectionKind::TextFile,
            _ => ConnectionKind::Other,
        }
    }
}
