//! One struct per command: the single source of its command-line flags, its
//! MCP tool schema and its help (record 0010, decisions 2 and 6).
//!
//! - A struct's first doc line is the summary for help lists; the rest is
//!   the tool description. A field's doc line describes the argument.
//! - `schemars` writes the schema, `serde` checks what a client sent, and
//!   `clap` (feature `cli`, which only the program turns on) makes the flags.
//!   The program reads the flags as text and checks them here like an MCP
//!   call, so clap's typed result is never used.
//! - [`input_schema`] is the strict form that Claude, Gemini and OpenAI
//!   strict mode accept (`docs/agents.md`). The lint in `tests/lint` checks it.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::error::{CommandError, ErrorCode};
use crate::range::SheetRange;

/// The most items a list argument may hold.
pub const MAX_LIST_ITEMS: usize = 10_000;

/// The most characters a text argument may hold (Excel limits a formula to
/// 8,192; this leaves room for long cell text).
pub const MAX_TEXT_CHARS: usize = 32_767;

/// The arguments that name a file or folder. `mcp serve` keeps them inside
/// its root.
pub const PATHS: &[&str] = &["path", "output", "root"];

/// Whole-number arguments with their smallest and largest value. The schema
/// does not carry limits, because not every client accepts them.
const RANGES: &[(&str, u64, u64)] = &[
    ("max_rows", 1, 10_000),
    ("max_columns", 1, 16_384),
    ("count", 1, 1_048_576),
];

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
    pub mode: Option<Mode>,
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
    pub format: Option<Format>,
    /// For csv, the sheet to write. Default: the first sheet.
    #[cfg_attr(feature = "cli", arg(long))]
    pub sheet: Option<String>,
    /// For csv, write text starting with = + - @ as is. Default: false: a ' before it stops apps running it as a formula.
    #[cfg_attr(feature = "cli", arg(long))]
    #[serde(default)]
    pub csv_raw: bool,
    /// Replace an existing file. Default: false.
    #[cfg_attr(feature = "cli", arg(long))]
    #[serde(default)]
    pub overwrite: bool,
    /// Must be true to delete sheets, rows or columns or overwrite a file (not with dry_run). Default: false.
    #[cfg_attr(feature = "cli", arg(long))]
    #[serde(default)]
    pub confirm: bool,
    /// Must be true to save a file that loses content (macros, charts...), listed by dry_run in save.lost. Default: false.
    #[cfg_attr(feature = "cli", arg(long))]
    #[serde(default)]
    pub confirm_loss: bool,
    /// List the changes, write nothing. Default: false.
    #[cfg_attr(feature = "cli", arg(long))]
    #[serde(default)]
    pub dry_run: bool,
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
    #[cfg_attr(feature = "cli", arg(long))]
    #[serde(default)]
    pub overwrite: bool,
    /// Must be true to delete sheets, rows or columns or overwrite a file (not with dry_run). Default: false.
    #[cfg_attr(feature = "cli", arg(long))]
    #[serde(default)]
    pub confirm: bool,
    /// Must be true to save a file that loses content (macros, charts...), listed by dry_run in save.lost. Default: false.
    #[cfg_attr(feature = "cli", arg(long))]
    #[serde(default)]
    pub confirm_loss: bool,
    /// List the changes, write nothing. Default: false.
    #[cfg_attr(feature = "cli", arg(long))]
    #[serde(default)]
    pub dry_run: bool,
}

// The `ops` of `edit`: on the command line one JSON text, not a repeated
// flag. A doc comment on the types below would add a description to every
// tool list, so they have none.
pub type Operations = Vec<Operation>;

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    /// What to do.
    pub op: OperationKind,
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
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    SetValue,
    SetFormula,
    Clear,
    InsertRows,
    DeleteRows,
    InsertColumns,
    DeleteColumns,
    AddSheet,
    RenameSheet,
    DeleteSheet,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    Concise,
    Detailed,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Values,
    Formulas,
    Both,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Xlsx,
    Xls,
    Csv,
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
    #[cfg_attr(feature = "cli", arg(long))]
    #[serde(default)]
    pub read_only: bool,
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
    /// The four MCP annotations, all set: `readOnlyHint`, `destructiveHint`,
    /// `idempotentHint`, `openWorldHint`.
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
            schema: || {
                let settings = schemars::generate::SchemaSettings::draft2020_12();
                let generator = settings.with(|s| s.inline_subschemas = true).into_generator();
                generator.into_root_schema_for::<$args>().to_value()
            },
            check: |given| <$args>::deserialize(given).map(|_| ()).map_err(|e| e.to_string()),
            #[cfg(feature = "cli")]
            flags: <$args as clap::Args>::augment_args,
        }
    };
}

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

    /// True when the command takes an argument of this name.
    pub fn has_arg(&self, name: &str) -> bool {
        input_schema(self)["properties"].get(name).is_some()
    }

    /// Writes or replaces files. Such commands take `dry_run`.
    pub fn changes_files(&self) -> bool {
        self.has_arg("dry_run")
    }

    /// The four MCP annotations under their protocol names.
    pub fn annotations(&self) -> Value {
        let [read_only, destructive, idempotent, open_world] = self.hints;
        json!({
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "idempotentHint": idempotent,
            "openWorldHint": open_world,
        })
    }
}

/// The single-type schemas a value may match, without null: schemars'
/// schema with `format`, limits, defaults and type lists taken out.
fn branches(node: &Value) -> Vec<Value> {
    if let Some(any) = node.get("anyOf").and_then(Value::as_array) {
        return any.iter().flat_map(branches).collect();
    }
    let types = match node.get("type") {
        Some(Value::Array(types)) => types.clone(),
        Some(one) => vec![one.clone()],
        None => Vec::new(),
    };
    let one = |kind: Value| {
        let mut out = json!({ "type": kind });
        if let Some(values) = node.get("enum").and_then(Value::as_array) {
            out["enum"] = values.iter().filter(|v| !v.is_null()).cloned().collect();
        }
        if let Some(items) = node.get("items") {
            out["items"] = property(items, false);
        }
        if out["type"] == "object" {
            let required = node.get("required").and_then(Value::as_array);
            let is_optional = |name: &String| !required.is_some_and(|r| r.contains(&json!(name)));
            let fields = node.get("properties").and_then(Value::as_object);
            let fields = fields.into_iter().flatten();
            let properties: Map<String, Value> = fields
                .map(|(name, field)| (name.clone(), property(field, is_optional(name))))
                .collect();
            out["required"] = properties.keys().cloned().collect();
            out["properties"] = Value::Object(properties);
            out["additionalProperties"] = json!(false);
        }
        out
    };
    types.into_iter().filter(|t| t != "null").map(one).collect()
}

/// One property in the strict form: every object lists all its properties
/// as required and allows no others, and an optional value is an `anyOf`
/// of its types with null last.
fn property(node: &Value, optional: bool) -> Value {
    let mut any = branches(node);
    let mut out = if optional || any.len() != 1 {
        any.extend(optional.then(|| json!({ "type": "null" })));
        json!({ "anyOf": any })
    } else {
        any.remove(0)
    };
    if let Some(text) = node.get("description") {
        out["description"] = text.clone();
    }
    out
}

/// The input schema of a command: an object with one property per argument.
pub fn input_schema(command: &Command) -> Value {
    let mut schema = property(&(command.schema)(), false);
    // The struct's doc comment is the tool description, not part of this.
    schema.as_object_mut().and_then(|s| s.remove("description"));
    schema
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

fn invalid(name: &str, why: String, fix: impl Into<String>) -> CommandError {
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
/// schema validation. Missing and `null` both mean "not given". Returns
/// every argument of the command by name, absent ones as `null`.
pub fn validate_args(command: &Command, args: &Value) -> Result<Map<String, Value>, CommandError> {
    let cli = command.cli_name();
    let Value::Object(mut given) = without_nulls(args) else {
        return Err(invalid(
            "arguments",
            "the arguments must be a JSON object".to_owned(),
            format!(
                "Send an object such as {{\"path\": \"book.xlsx\"}}. Run `carrysheet-cli {cli} --help` for the arguments."
            ),
        ));
    };
    let schema = input_schema(command);
    let names: Vec<&str> = schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if let Some(unknown) = given.keys().find(|k| !names.contains(&k.as_str())) {
        let why = format!("`{cli}` has no argument named `{unknown}`");
        return Err(invalid(unknown, why, format!("Use one of: {}.", names.join(", "))));
    }
    if let Err(why) = (command.check)(Value::Object(given.clone())) {
        let name = culprit(command, &given, &why);
        let field = name.split('[').next().unwrap_or_default();
        let fix = match schema["properties"][field]["description"].as_str() {
            Some(text) => format!("`{field}`: {text}"),
            None => format!("Run `carrysheet-cli {cli} --help` for the arguments."),
        };
        // serde repeats a wrong value; keep a long one out of the error.
        return Err(invalid(&name, why.chars().take(200).collect(), fix));
    }
    for (name, value) in &given {
        check_limits(name, name, value)?;
    }
    let mut all = Map::new();
    for name in names {
        all.insert(name.to_owned(), given.remove(name).unwrap_or(Value::Null));
    }
    Ok(all)
}

/// The argument a serde error is about: the one named as missing, or the
/// first whose removal changes the error; for a list, the item too.
fn culprit(command: &Command, given: &Map<String, Value>, why: &str) -> String {
    let missing = why.strip_prefix("missing field `").and_then(|rest| rest.split('`').next());
    if let Some(name) = missing.filter(|name| !given.contains_key(*name) && command.has_arg(name)) {
        return name.to_owned();
    }
    let fails_the_same =
        |args: Map<String, Value>| (command.check)(Value::Object(args)).err().as_deref() == Some(why);
    for (name, value) in given {
        let mut rest = given.clone();
        rest.shift_remove(name);
        if fails_the_same(rest.clone()) {
            continue;
        }
        for (i, item) in value.as_array().into_iter().flatten().enumerate() {
            let mut one = rest.clone();
            one.insert(name.clone(), json!([item]));
            if fails_the_same(one) {
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
                let why = format!("`{name}` is not a usable path");
                return Err(invalid(name, why, "Give a file path like book.xlsx or reports/q1.xlsx."));
            }
            if key == "range" {
                SheetRange::parse(text)?;
            }
        }
        Value::Number(number) => {
            if let Some((_, min, max)) = RANGES.iter().find(|r| r.0 == key)
                && !number.as_u64().is_some_and(|n| (*min..=*max).contains(&n))
            {
                let expected = format!("a whole number from {min} to {max}");
                let why = format!("`{name}` must be {expected}, but it is {number}");
                return Err(invalid(name, why, format!("Give `{name}` as {expected}.")));
            }
        }
        Value::Array(items) => {
            if items.len() > MAX_LIST_ITEMS {
                let why = format!("`{name}` has {} items; the limit is {MAX_LIST_ITEMS}", items.len());
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
pub fn check_confirmation(command: &Command, args: &Map<String, Value>) -> Result<(), CommandError> {
    let is_true = |name: &str| args.get(name) == Some(&Value::Bool(true));
    if !args.contains_key("confirm") || is_true("confirm") || is_true("dry_run") {
        return Ok(());
    }
    let ops = args.get("ops").and_then(Value::as_array).into_iter().flatten();
    let deletes = |(i, op): (usize, &Value)| {
        let kind = op.get("op")?.as_str()?.strip_prefix("delete_")?;
        Some(format!("delete {kind} (ops[{i}])"))
    };
    let mut actions: Vec<String> = ops.enumerate().filter_map(deletes).collect();
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
    for (name, value) in args.iter().filter(|(name, value)| !value.is_null() && *name != "confirm") {
        let shown = match value {
            Value::Array(items) if value.to_string().chars().count() > 160 => {
                format!("[...the same {} items...]", items.len())
            }
            Value::String(text) if text.chars().count() > 80 => "\"...\"".to_owned(),
            _ => value.to_string(),
        };
        call.push(format!("\"{name}\":{shown}"));
    }
    call.push("\"confirm\":true".to_owned());
    Err(CommandError::new(
        ErrorCode::ConfirmationRequired,
        format!("`{}` would {first}{more}; confirm is not true", command.cli_name()),
        "deleting sheets, rows or columns and overwriting files cannot be undone, so they need confirm: true. dry_run lists the changes without it.",
        format!(
            "Call again with confirm set to true: {{{}}}. On the command line, add --confirm.",
            call.join(",")
        ),
    ))
}

// Reading the checked arguments of a command.

pub(crate) fn text<'a>(args: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

pub(crate) fn flag(args: &Map<String, Value>, name: &str) -> bool {
    args.get(name).and_then(Value::as_bool) == Some(true)
}

pub(crate) fn number(args: &Map<String, Value>, name: &str) -> Option<u32> {
    args.get(name)
        .and_then(Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
}

pub(crate) fn detailed(args: &Map<String, Value>) -> bool {
    text(args, "response_format") == Some("detailed")
}

/// The workbook path; validation makes sure it is there.
pub(crate) fn input(args: &Map<String, Value>) -> Result<PathBuf, CommandError> {
    text(args, "path").map(PathBuf::from).ok_or_else(|| {
        invalid(
            "path",
            "no workbook was given".to_owned(),
            "Name the workbook file, like book.xlsx.",
        )
    })
}

/// The program shows it with `~` for the home folder.
pub(crate) fn shown(path: &Path) -> Value {
    json!(path.display().to_string())
}
