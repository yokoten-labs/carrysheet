use crate::commands::schema::*;
use crate::commands::table::{COMMANDS, command};
use serde_json::{Value, json};

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
