//! JSON Schemas generated from the command table.
//!
//! The schemas are written for the strictest client we know of, so the same
//! schema works everywhere:
//!
//! - the root is a plain object, with no `anyOf`/`oneOf`/`allOf` (Claude);
//! - no `$ref`/`$defs`, no type lists, `items` on every array and only on
//!   arrays (Gemini);
//! - every property is listed in `required`, optional ones are nullable
//!   through a property-level `anyOf` of single types, and every object has
//!   `additionalProperties: false` (OpenAI strict mode);
//! - no `format`, `pattern`, limits or defaults; we validate on our side
//!   ([`validate`]) and defaults are stated in descriptions.
//!
//! [`lint`] checks these rules.

pub mod lint;
pub mod validate;

use serde_json::{Map, Value, json};

use crate::table::{Annotations, Arg, ArgType, Command};

/// The single-type schemas an argument's value may match, without
/// description or nullability. Most types have one; a typed cell value has
/// one per JSON type it accepts.
fn value_schemas(ty: &ArgType) -> Vec<Value> {
    vec![match ty {
        ArgType::Text | ArgType::Path | ArgType::Range => json!({ "type": "string" }),
        ArgType::Integer { .. } => json!({ "type": "integer" }),
        ArgType::Boolean => json!({ "type": "boolean" }),
        ArgType::Enum(values) => json!({ "type": "string", "enum": values }),
        ArgType::TextList => json!({ "type": "array", "items": { "type": "string" } }),
        ArgType::ObjectList(fields) => json!({
            "type": "array",
            "items": object_schema(fields.iter()),
        }),
        ArgType::Scalar => {
            return vec![
                json!({ "type": "string" }),
                json!({ "type": "number" }),
                json!({ "type": "boolean" }),
            ];
        }
    }]
}

/// The schema for one property: its value schema, nullable when optional,
/// with the description on the property.
fn property_schema(arg: &Arg) -> Value {
    let mut branches = value_schemas(&arg.ty);
    let mut schema = if arg.required && branches.len() == 1 {
        branches.remove(0)
    } else {
        // The table test keeps typed values optional, so this always ends
        // in null as the lint requires.
        if !arg.required {
            branches.push(json!({ "type": "null" }));
        }
        json!({ "anyOf": branches })
    };
    if let Value::Object(map) = &mut schema {
        map.insert("description".into(), Value::String(arg.description.into()));
    }
    schema
}

/// A strict object schema: every property required, nothing else allowed.
fn object_schema<'a>(args: impl Iterator<Item = &'a Arg>) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for arg in args {
        properties.insert(arg.name.into(), property_schema(arg));
        required.push(Value::String(arg.name.into()));
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// The input schema of a command: an object with one property per argument
/// (including `dry_run` for commands that change files).
pub fn input_schema(command: &Command) -> Value {
    object_schema(command.all_args().into_iter())
}

/// The four MCP annotations under their protocol names.
pub fn annotations_json(a: &Annotations) -> Value {
    json!({
        "readOnlyHint": a.read_only,
        "destructiveHint": a.destructive,
        "idempotentHint": a.idempotent,
        "openWorldHint": a.open_world,
    })
}

/// The MCP tool definition of a command: name, description, input schema and annotations.
pub fn tool_definition(command: &Command) -> Value {
    json!({
        "name": command.name,
        "description": command.description,
        "inputSchema": input_schema(command),
        "annotations": annotations_json(&command.annotations),
    })
}

/// Everything about a command, for `commands --json`.
pub fn command_json(command: &Command) -> Value {
    let args: Vec<Value> = command
        .all_args()
        .into_iter()
        .map(|a| {
            json!({
                "name": a.name,
                "flag": if a.positional { Value::Null } else { Value::String(a.flag()) },
                "positional": a.positional,
                "required": a.required,
                "type": type_name(&a.ty),
                "description": a.description,
            })
        })
        .collect();
    json!({
        "name": command.name,
        "cli": command.cli_name(),
        "summary": command.summary,
        "description": command.description,
        "changes_files": command.changes_files,
        "mcp_tool": command.mcp_tool,
        "annotations": annotations_json(&command.annotations),
        "args": args,
        "input_schema": input_schema(command),
    })
}

/// A short type name for help and `commands --json`.
pub fn type_name(ty: &ArgType) -> &'static str {
    match ty {
        ArgType::Text => "text",
        ArgType::Path => "path",
        ArgType::Range => "range",
        ArgType::Integer { .. } => "integer",
        ArgType::Boolean => "boolean",
        ArgType::Enum(_) => "enum",
        ArgType::Scalar => "value",
        ArgType::TextList => "text_list",
        ArgType::ObjectList(_) => "object_list",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::{COMMANDS, command};

    #[test]
    fn overview_schema_shape() {
        let s = input_schema(command("overview").unwrap());
        assert_eq!(s["type"], "object");
        assert_eq!(s["additionalProperties"], false);
        assert_eq!(s["required"], json!(["path", "response_format"]));
        assert_eq!(s["properties"]["path"]["type"], "string");
        let rf = &s["properties"]["response_format"];
        assert_eq!(rf["anyOf"][0]["enum"], json!(["concise", "detailed"]));
        assert_eq!(rf["anyOf"][1], json!({ "type": "null" }));
        assert!(rf["description"].is_string());
        assert!(rf.get("type").is_none());
    }

    #[test]
    fn dry_run_is_in_the_schema_of_commands_that_change_files() {
        let s = input_schema(command("convert").unwrap());
        assert!(s["properties"].get("dry_run").is_some());
        let s = input_schema(command("read").unwrap());
        assert!(s["properties"].get("dry_run").is_none());
    }

    #[test]
    fn edit_ops_are_strict_objects() {
        let s = input_schema(command("edit").unwrap());
        let items = &s["properties"]["ops"]["items"];
        assert_eq!(s["properties"]["ops"]["type"], "array");
        assert_eq!(items["type"], "object");
        assert_eq!(items["additionalProperties"], false);
        assert_eq!(items["required"], json!(["op", "target", "value", "count"]));
        assert_eq!(
            items["properties"]["value"]["anyOf"],
            json!([
                { "type": "string" },
                { "type": "number" },
                { "type": "boolean" },
                { "type": "null" }
            ])
        );
    }

    #[test]
    fn tool_definitions_have_all_annotations() {
        for c in COMMANDS {
            let t = tool_definition(c);
            for key in [
                "readOnlyHint",
                "destructiveHint",
                "idempotentHint",
                "openWorldHint",
            ] {
                assert!(t["annotations"][key].is_boolean(), "{} {key}", c.name);
            }
        }
    }

    #[test]
    fn command_json_lists_flags() {
        let c = command_json(command("read").unwrap());
        assert_eq!(c["cli"], "read");
        let flags: Vec<_> = c["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["flag"].clone())
            .collect();
        assert_eq!(flags[0], Value::Null);
        assert!(flags.contains(&json!("--max-rows")));
    }
}
