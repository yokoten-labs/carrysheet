//! One struct per command: the single source of its command-line flags, its
//! MCP tool schema and its help (record 0010, decisions 2 and 6).
//!
//! - A struct's first doc line is the summary for help lists; the rest is
//!   the tool description. A field's doc line describes the argument.
//! - `schemars` writes the schema, `serde` checks what a client sent, and
//!   `clap` (feature `cli`, which only the program turns on) makes the flags.
//!   The program reads the flags as text and checks them here like an MCP
//!   call, so clap's typed result is never used.
//! - An optional argument is an `Option`: absent and `null` mean the same.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::edit::EditKind;
use crate::error::{CommandError, ErrorCode};
use crate::range::SheetRange;
use crate::save::FileFormat;
use crate::view::ReadMode;

/// The most items a list argument may hold.
pub const MAX_LIST_ITEMS: usize = 10_000;

/// The most characters a text argument may hold (Excel limits a formula to
/// 8,192; this leaves room for long cell text).
pub const MAX_TEXT_CHARS: usize = 32_767;

/// The arguments that name a file or folder; `mcp serve` keeps them in its root.
pub const PATHS: &[&str] = &["path", "output", "root"];

/// The largest value of each whole-number argument; the smallest is 1. The
/// schema carries no limits, because not every client accepts them.
const LARGEST: &[(&str, u64)] = &[
    ("max_rows", 10_000),
    ("max_columns", 16_384),
    ("count", 1_048_576),
];

/// On the command line a true/false argument is a switch, such as `--dry-run`.
#[cfg(feature = "cli")]
const SWITCH: clap::ArgAction = clap::ArgAction::SetTrue;

/// Show sheets, used ranges, headers and formula counts of a workbook
///
/// Workbook overview: sheets, used ranges, header rows, formula and error counts, and whether it has macros (or that they could not be checked). Call first on any workbook.
#[derive(Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::Args))]
pub struct Overview {
    /// The workbook: xlsx, xlsm, xlsb, xls or csv.
    pub path: String,
    /// concise (default), or detailed: adds types, formats and formulas.
    #[cfg_attr(feature = "cli", arg(long))]
    pub response_format: Option<ResponseFormat>,
}

/// Read a range as a table of values or formulas, a page at a time
///
/// Read a range as a compact table with A1 addresses, values or formulas, paged. Use after overview. Returns next_cursor when more remains.
#[derive(Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::Args))]
pub struct Read {
    /// The workbook: xlsx, xlsm, xlsb, xls or csv.
    pub path: String,
    /// Like Sheet1!A1:D50 or 'My sheet'!A:C. Default: the used range of the first sheet.
    pub range: Option<String>,
    /// values (default), formulas, or both.
    #[cfg_attr(feature = "cli", arg(long))]
    pub mode: Option<ReadMode>,
    /// Rows per page, 1 to 10000. Default: 200.
    #[cfg_attr(feature = "cli", arg(long))]
    pub max_rows: Option<u32>,
    /// Columns per page, 1 to 16384. Default: 50. A page also stops at about 10000 cells.
    #[cfg_attr(feature = "cli", arg(long))]
    pub max_columns: Option<u32>,
    /// next_cursor from the previous page; range is then ignored. Keep max_rows the same.
    #[cfg_attr(feature = "cli", arg(long))]
    pub cursor: Option<String>,
    /// concise (default), or detailed: adds types, formats and formulas.
    #[cfg_attr(feature = "cli", arg(long))]
    pub response_format: Option<ResponseFormat>,
}

/// Compute a formula against a workbook without saving
///
/// Evaluate a formula against the workbook without changing it. Use to test a formula before writing it with edit.
#[derive(Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::Args))]
pub struct Eval {
    /// The workbook: xlsx, xlsm, xlsb, xls or csv.
    pub path: String,
    /// An en-US formula, with or without =, like SUM(Sheet1!B2:B9).
    pub formula: String,
    /// Where unqualified references point. Default: the first sheet.
    #[cfg_attr(feature = "cli", arg(long))]
    pub sheet: Option<String>,
}

/// Save a workbook as xlsx, xls or csv, to a new file
///
/// Convert a workbook to xlsx, xls or csv as a new file. Use dry_run first to see what the format cannot hold.
#[derive(Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::Args))]
pub struct Convert {
    /// The workbook: xlsx, xlsm, xlsb, xls or csv.
    pub path: String,
    /// The file to write.
    pub output: String,
    /// Default: taken from the output file's extension.
    #[cfg_attr(feature = "cli", arg(long))]
    pub format: Option<FileFormat>,
    /// For csv, the sheet to write. Default: the first sheet.
    #[cfg_attr(feature = "cli", arg(long))]
    pub sheet: Option<String>,
    /// For csv, write text starting with = + - @ as is. Default: false: a ' before it stops apps running it as a formula.
    #[cfg_attr(feature = "cli", arg(long, action = SWITCH))]
    pub csv_raw: Option<bool>,
    /// Replace an existing file. Default: false.
    #[cfg_attr(feature = "cli", arg(long, action = SWITCH))]
    pub overwrite: Option<bool>,
    /// Must be true to delete sheets, rows or columns or overwrite a file (not with dry_run). Default: false.
    #[cfg_attr(feature = "cli", arg(long, action = SWITCH))]
    pub confirm: Option<bool>,
    /// Must be true to save a file that loses content (macros, charts...), listed by dry_run in save.lost. Default: false.
    #[cfg_attr(feature = "cli", arg(long, action = SWITCH))]
    pub confirm_loss: Option<bool>,
    /// List the changes, write nothing. Default: false.
    #[cfg_attr(feature = "cli", arg(long, action = SWITCH))]
    pub dry_run: Option<bool>,
}

/// Change cells, rows and sheets in one all-or-nothing batch
///
/// Edit a workbook: a batch of operations on values, formulas, rows, columns and sheets, applied all or nothing. Writes a new file unless overwrite is true. Dropping content (macros, charts) needs confirm_loss. Use dry_run first.
#[derive(Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::Args))]
pub struct Edit {
    /// The workbook: xlsx, xlsm, xlsb, xls or csv.
    pub path: String,
    /// The operations, applied in order.
    #[cfg_attr(feature = "cli", arg(long, value_name = "JSON", value_parser = clap::value_parser!(String)))]
    pub ops: Operations,
    /// Default: a new file beside the original.
    #[cfg_attr(feature = "cli", arg(long))]
    pub output: Option<String>,
    /// Replace an existing file. Default: false.
    #[cfg_attr(feature = "cli", arg(long, action = SWITCH))]
    pub overwrite: Option<bool>,
    /// Must be true to delete sheets, rows or columns or overwrite a file (not with dry_run). Default: false.
    #[cfg_attr(feature = "cli", arg(long, action = SWITCH))]
    pub confirm: Option<bool>,
    /// Must be true to save a file that loses content (macros, charts...), listed by dry_run in save.lost. Default: false.
    #[cfg_attr(feature = "cli", arg(long, action = SWITCH))]
    pub confirm_loss: Option<bool>,
    /// List the changes, write nothing. Default: false.
    #[cfg_attr(feature = "cli", arg(long, action = SWITCH))]
    pub dry_run: Option<bool>,
}

// The `ops` of `edit`: on the command line one JSON text, not a repeated
// flag. A doc comment on the types below would add a description to every
// tool list, so they have none.
pub type Operations = Vec<Operation>;

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    /// What to do.
    pub op: EditKind,
    /// A range like Sheet1!B2:B9, or a sheet name for sheet ops.
    pub target: String,
    /// Text, a number or true/false, kept as typed ("3" is text, 3 a number). For set_formula, the formula without =; for sheet ops, the new name.
    pub value: Option<Scalar>,
    /// Rows or columns to insert or delete, from the start of target. Default: as many as target spans.
    pub count: Option<u32>,
}

// A cell value. The JSON type says whether "3" is text or a number.
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(untagged, expecting = "value must be text, a number, or true or false")]
pub enum Scalar {
    Text(String),
    Number(f64),
    Flag(bool),
}

#[derive(Clone, Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    Concise,
    Detailed,
}

/// Run the MCP server for AI agents, over stdio
///
/// MCP server: serves Carrysheet's tools to an AI client over stdio. Clients start it; people rarely run it by hand.
#[derive(Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::Args))]
pub struct McpServe {
    /// Only files inside this folder can be used. Default: the current folder.
    #[cfg_attr(feature = "cli", arg(long))]
    pub root: Option<String>,
    /// Offer only tools that never change files. Default: false.
    #[cfg_attr(feature = "cli", arg(long, action = SWITCH))]
    pub read_only: Option<bool>,
}

/// List every command; with --json, with its argument schema
///
/// Commands list: every command with its summary, arguments, annotations and JSON schema. Agents read this to learn the CLI.
#[derive(Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::Args))]
pub struct Commands {}

/// One command: its name, what a client is told about it, and its struct.
pub struct Command {
    /// Unique `snake_case` name: the MCP tool name. On the command line a
    /// `_` is a space, as in `mcp serve`.
    pub name: &'static str,
    /// Offered as a tool by the MCP server.
    pub mcp_tool: bool,
    /// The four MCP annotations: read only, destructive, idempotent, open world.
    pub hints: [bool; 4],
    schema: fn() -> Value,
    check: fn(Value) -> Result<(), String>,
    /// Adds the command's flags to a clap command.
    #[cfg(feature = "cli")]
    pub flags: fn(clap::Command) -> clap::Command,
}

macro_rules! command {
    ($name:literal, $args:ty, $mcp_tool:literal, $hints:expr) => {
        Command {
            name: $name,
            mcp_tool: $mcp_tool,
            hints: $hints,
            schema: schema_of::<$args>,
            check: check::<$args>,
            #[cfg(feature = "cli")]
            flags: <$args as clap::Args>::augment_args,
        }
    };
}

/// The names of the four MCP annotations.
const HINTS: &str = "readOnlyHint destructiveHint idempotentHint openWorldHint";

/// Never changes anything, and calling it again gives the same answer.
const READS: [bool; 4] = [true, false, true, false];

/// Every command, in the fixed order the help and the MCP tool list use.
pub const COMMANDS: &[Command] = &[
    command!("overview", Overview, true, READS),
    command!("read", Read, true, READS),
    command!("eval", Eval, true, READS),
    command!("convert", Convert, true, [false, true, true, false]),
    command!("edit", Edit, true, [false, true, false, false]),
    command!("mcp_serve", McpServe, false, [false, true, false, false]),
    command!("commands", Commands, false, READS),
];

/// A command by its name.
pub fn command(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// The commands the MCP server offers, in order.
pub fn mcp_tools() -> impl Iterator<Item = &'static Command> {
    COMMANDS.iter().filter(|c| c.mcp_tool)
}

impl Command {
    /// How the command is typed, such as `mcp serve`.
    pub fn cli_name(&self) -> String {
        self.name.replace('_', " ")
    }

    /// The struct's doc comment: the summary for help lists (one line), then
    /// the tool description (key words first, then when to use it).
    pub fn doc(&self) -> (String, String) {
        let schema = (self.schema)();
        let text = schema["description"].as_str().unwrap_or_default();
        let (summary, description) = text.split_once("\n\n").unwrap_or((text, text));
        (summary.trim().to_owned(), description.trim().to_owned())
    }

    /// The names of the command's arguments, in order.
    pub fn arg_names(&self) -> Vec<String> {
        let schema = input_schema(self);
        let properties = schema["properties"].as_object();
        let names = properties.into_iter().flat_map(Map::keys);
        names.cloned().collect()
    }

    /// True when the command takes an argument of this name.
    pub fn has_arg(&self, name: &str) -> bool {
        self.arg_names().iter().any(|arg| arg == name)
    }

    /// Never changes anything (`readOnlyHint`).
    pub fn is_read_only(&self) -> bool {
        self.hints[0]
    }

    /// Writes or replaces files. Such commands take `dry_run`.
    pub fn changes_files(&self) -> bool {
        self.has_arg("dry_run")
    }

    /// The four MCP annotations under their protocol names.
    pub fn annotations(&self) -> Value {
        HINTS.split(' ').zip(self.hints).collect()
    }
}

/// What schemars writes for a struct, with nothing left as a `$ref`.
fn schema_of<T: JsonSchema>() -> Value {
    let settings = SchemaSettings::draft2020_12().with(|s| s.inline_subschemas = true);
    let generator = settings.into_generator();
    generator.into_root_schema_for::<T>().to_value()
}

/// What serde says when `given` is not a `T`.
fn check<T: DeserializeOwned>(given: Value) -> Result<(), String> {
    T::deserialize(given).map(|_| ()).map_err(|e| e.to_string())
}

/// The single-type schemas a value may match, without null: schemars'
/// schema with `format`, limits, defaults and type lists taken out. An
/// object lists every property as required and allows no others.
fn branches(node: &Value) -> Vec<Value> {
    if let Some(any) = node["anyOf"].as_array() {
        return any.iter().flat_map(branches).collect();
    }
    let mut types = match &node["type"] {
        Value::Array(types) => types.clone(),
        one => vec![one.clone()],
    };
    types.retain(|kind| kind.is_string() && kind != "null");
    let branch = |kind: Value| {
        let mut out = json!({ "type": kind });
        if let Some(values) = node["enum"].as_array() {
            out["enum"] = values.iter().filter(|v| !v.is_null()).cloned().collect();
        }
        if node.get("items").is_some() {
            out["items"] = property(&node["items"], false);
        }
        if out["type"] == "object" {
            let required = node["required"].as_array().cloned().unwrap_or_default();
            let mut properties = Map::new();
            for (name, field) in node["properties"].as_object().into_iter().flatten() {
                let is_optional = !required.contains(&json!(name));
                properties.insert(name.clone(), property(field, is_optional));
            }
            let names: Value = properties.keys().cloned().collect();
            out["properties"] = properties.into();
            out["required"] = names;
            out["additionalProperties"] = json!(false);
        }
        out
    };
    types.into_iter().map(branch).collect()
}

/// One property in the strict form: its description, and for an optional
/// value an `anyOf` of its types with null last.
fn property(node: &Value, is_optional: bool) -> Value {
    let mut any = branches(node);
    let mut out = if is_optional || any.len() != 1 {
        any.extend(is_optional.then(|| json!({ "type": "null" })));
        json!({ "anyOf": any })
    } else {
        any.remove(0)
    };
    if let Some(text) = node.get("description") {
        out["description"] = text.clone();
    }
    out
}

/// The input schema of a command: an object with one property per argument,
/// in the strict form that Claude, Gemini and OpenAI strict mode accept
/// (`docs/agents.md`). The lint in `tests/lint` checks it.
pub fn input_schema(command: &Command) -> Value {
    // The struct's doc comment is the tool description, not part of this.
    branches(&(command.schema)()).pop().unwrap_or_default()
}

/// The MCP tool definition of a command: name, description, input schema and annotations.
pub fn tool_definition(command: &Command) -> Value {
    json!({
        "name": command.name,
        "description": command.doc().1,
        "inputSchema": input_schema(command),
        "annotations": command.annotations(),
    })
}

/// The checked arguments of a call: every argument of the command by name,
/// absent ones as `null`.
pub type Checked = Map<String, Value>;

fn invalid(name: &str, why: impl Into<String>, fix: impl Into<String>) -> CommandError {
    CommandError::invalid_argument(name, why, fix)
}

/// A JSON value without the `null` fields of its objects.
fn without_nulls(value: &Value) -> Value {
    match value {
        Value::Array(items) => items.iter().map(without_nulls).collect(),
        Value::Object(fields) => {
            let given = fields.iter().filter(|(_, v)| !v.is_null());
            given.map(|(k, v)| (k.clone(), without_nulls(v))).collect()
        }
        other => other.clone(),
    }
}

/// Checks `args` (a JSON object) on our side, because a client may skip
/// schema validation.
pub fn validate_args(command: &Command, args: &Value) -> Result<Checked, CommandError> {
    let cli = command.cli_name();
    let help = format!("Run `carrysheet-cli {cli} --help` for the arguments.");
    let Value::Object(mut given) = without_nulls(args) else {
        let why = "the arguments must be a JSON object";
        let fix = format!("Send an object such as {{\"path\": \"book.xlsx\"}}. {help}");
        return Err(invalid("arguments", why, fix));
    };
    let names = command.arg_names();
    if let Some(unknown) = given.keys().find(|key| !names.contains(key)) {
        let why = format!("`{cli}` has no argument named `{unknown}`");
        let fix = format!("Use one of: {}.", names.join(", "));
        return Err(invalid(unknown, why, fix));
    }
    if let Err(why) = (command.check)(given.clone().into()) {
        let name = culprit(command, &given, &why);
        let field = name.split('[').next().unwrap_or_default();
        let schema = input_schema(command);
        let fix = match schema["properties"][field]["description"].as_str() {
            Some(text) => format!("`{field}`: {text}"),
            None => help,
        };
        // serde repeats a wrong value; keep a long one out of the error.
        let why: String = why.chars().take(300).collect();
        return Err(invalid(&name, why, fix));
    }
    for (name, value) in &given {
        check_limits(name, name, value)?;
    }
    let all = names.into_iter().map(|name| {
        let value = given.remove(&name).unwrap_or(Value::Null);
        (name, value)
    });
    Ok(all.collect())
}

/// The argument a serde error is about: the one named as missing, or the
/// first whose removal changes the error; for a list, the item too.
fn culprit(command: &Command, given: &Map<String, Value>, why: &str) -> String {
    let missing = why.strip_prefix("missing field `").unwrap_or_default();
    let missing = missing.split('`').next().unwrap_or_default();
    if !given.contains_key(missing) && command.has_arg(missing) {
        return missing.to_owned();
    }
    let fails_alike =
        |args: Map<String, Value>| (command.check)(args.into()).err().as_deref() == Some(why);
    for (name, value) in given {
        let mut rest = given.clone();
        rest.shift_remove(name);
        if fails_alike(rest.clone()) {
            continue;
        }
        for (i, item) in value.as_array().into_iter().flatten().enumerate() {
            let mut one = rest.clone();
            one.insert(name.clone(), json!([item]));
            if fails_alike(one) {
                return format!("{name}[{i}]");
            }
        }
        return name.clone();
    }
    "arguments".to_owned()
}

/// What the schema does not carry: sizes of text and lists, number ranges,
/// usable paths and A1 ranges. `name` is the full name, like `ops[2].count`.
fn check_limits(name: &str, key: &str, value: &Value) -> Result<(), CommandError> {
    match value {
        Value::String(text) => {
            if text.chars().count() > MAX_TEXT_CHARS {
                let why = format!("`{name}` is longer than {MAX_TEXT_CHARS} characters");
                return Err(invalid(name, why, "Shorten it."));
            }
            if PATHS.contains(&key) && (text.is_empty() || text.contains('\0')) {
                let fix = "Give a file path like book.xlsx or reports/q1.xlsx.";
                return Err(invalid(name, format!("`{name}` is not a usable path"), fix));
            }
            if key == "range" {
                SheetRange::parse(text)?;
            }
        }
        Value::Number(number) => {
            let largest = LARGEST.iter().find(|(arg, _)| *arg == key);
            if let Some((_, max)) = largest
                && !number.as_u64().is_some_and(|n| (1..=*max).contains(&n))
            {
                let expected = format!("a whole number from 1 to {max}");
                let why = format!("`{name}` must be {expected}, but it is {number}");
                return Err(invalid(name, why, format!("Give `{name}` as {expected}.")));
            }
        }
        Value::Array(items) => {
            if items.len() > MAX_LIST_ITEMS {
                let n = items.len();
                let why = format!("`{name}` has {n} items; the limit is {MAX_LIST_ITEMS}");
                return Err(invalid(name, why, "Split the work into several calls."));
            }
            for (i, item) in items.iter().enumerate() {
                check_limits(&format!("{name}[{i}]"), key, item)?;
            }
        }
        Value::Object(fields) => {
            for (key, field) in fields {
                check_limits(&format!("{name}.{key}"), key, field)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Checks that a call which deletes data or overwrites a file says
/// `confirm: true`. `args` come from [`validate_args`]. `dry_run: true`
/// never needs it, because nothing is written.
pub fn check_confirmation(command: &Command, args: &Checked) -> Result<(), CommandError> {
    let is_true = |name: &str| args.get(name) == Some(&Value::Bool(true));
    if !args.contains_key("confirm") || is_true("confirm") || is_true("dry_run") {
        return Ok(());
    }
    let ops = args.get("ops").and_then(Value::as_array);
    let deletes = |(i, op): (usize, &Value)| {
        let kind = op.get("op")?.as_str()?.strip_prefix("delete_")?;
        Some(format!("delete {kind} (ops[{i}])"))
    };
    let ops = ops.into_iter().flatten().enumerate();
    let mut actions: Vec<String> = ops.filter_map(deletes).collect();
    if is_true("overwrite") {
        actions.push("overwrite a file".to_owned());
    }
    let Some(first) = actions.first() else {
        return Ok(());
    };
    let more = match actions.len() {
        1 => String::new(),
        n => format!(" and {} more", n - 1),
    };
    // The same call with `confirm: true`; long lists and text are summed up.
    let mut call = Vec::new();
    for (name, value) in args {
        let shown = match value {
            Value::Null => continue,
            _ if name == "confirm" => continue,
            Value::Array(items) if value.to_string().chars().count() > 160 => {
                format!("[...the same {} items...]", items.len())
            }
            Value::String(text) if text.chars().count() > 80 => "\"...\"".to_owned(),
            _ => value.to_string(),
        };
        call.push(format!("\"{name}\":{shown}"));
    }
    let call = call.join(",");
    let cli = command.cli_name();
    Err(CommandError::new(
        ErrorCode::ConfirmationRequired,
        format!("`{cli}` would {first}{more}; confirm is not true"),
        "deleting sheets, rows or columns and overwriting files cannot be undone, so they need confirm: true. dry_run lists the changes without it.",
        format!(
            "Call again with confirm set to true: {{{call},\"confirm\":true}}. On the command line, add --confirm."
        ),
    ))
}

// Reading the checked arguments of a command.

pub(crate) fn text<'a>(args: &'a Checked, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

pub(crate) fn flag(args: &Checked, name: &str) -> bool {
    args.get(name).and_then(Value::as_bool) == Some(true)
}

pub(crate) fn number(args: &Checked, name: &str) -> Option<u32> {
    let number = args.get(name)?.as_u64()?;
    Some(u32::try_from(number).unwrap_or(u32::MAX))
}

pub(crate) fn detailed(args: &Checked) -> bool {
    text(args, "response_format") == Some("detailed")
}

/// The workbook path; validation makes sure it is there.
pub(crate) fn input(args: &Checked) -> Result<PathBuf, CommandError> {
    let fix = "Name the workbook file, like book.xlsx.";
    let missing = || invalid("path", "no workbook was given", fix);
    text(args, "path").map(PathBuf::from).ok_or_else(missing)
}

/// The program shows it with `~` for the home folder.
pub(crate) fn shown(path: &Path) -> Value {
    json!(path.display().to_string())
}
