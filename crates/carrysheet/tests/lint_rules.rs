//! Tests of the schema lint in `tests/lint`.

mod lint;

use crate::lint::*;
use carrysheet::args::{COMMANDS, input_schema, mcp_tools, tool_definition};
use serde_json::{Value, json};

fn rules(schema: Value) -> Vec<Rule> {
    lint_schema(&schema).into_iter().map(|v| v.rule).collect()
}

/// A valid schema with one required and one optional property.
fn good() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": { "type": "string", "description": "A file." },
            "mode": {
                "anyOf": [
                    { "type": "string", "enum": ["a", "b"] },
                    { "type": "null" }
                ],
                "description": "A mode."
            },
            "ops": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": { "op": { "type": "string" } },
                    "required": ["op"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["path", "mode", "ops"],
        "additionalProperties": false
    })
}

/// `good()` with one change made by `f`, which must break exactly `expect`.
fn breaks(expect: Rule, f: impl FnOnce(&mut Value)) {
    let mut s = good();
    f(&mut s);
    let found = rules(s.clone());
    assert!(
        found.contains(&expect),
        "expected {expect:?}, got {found:?} for {s}"
    );
}

#[test]
fn every_generated_schema_passes() {
    for c in COMMANDS {
        let v = lint_schema(&input_schema(c));
        assert!(v.is_empty(), "{}: {:?}", c.name, v);
    }
}

#[test]
fn the_mcp_tool_list_passes() {
    let tools: Vec<Value> = mcp_tools().map(tool_definition).collect();
    let v = lint_tools(&tools);
    assert!(v.is_empty(), "{v:?}");
}

#[test]
fn every_command_as_a_tool_passes() {
    // The size budget is for the MCP tool list only; CLI-only commands
    // are never sent to a model as tools.
    let tools: Vec<Value> = COMMANDS.iter().map(tool_definition).collect();
    let v: Vec<_> = lint_tools(&tools)
        .into_iter()
        .filter(|v| v.rule != Rule::ToolsTooLarge)
        .collect();
    assert!(v.is_empty(), "{v:?}");
}

#[test]
fn the_mcp_tool_list_leaves_room() {
    let size: usize = mcp_tools()
        .map(|c| tool_definition(c).to_string().chars().count())
        .sum();
    assert!(size <= MAX_TOOLS_CHARS, "{size}");
    eprintln!("MCP tool definitions: {size} characters");
}

#[test]
fn the_good_example_passes() {
    assert_eq!(rules(good()), vec![]);
}

#[test]
fn catches_root_not_object() {
    assert!(rules(json!("string")).contains(&Rule::RootNotObject));
    breaks(Rule::RootNotObject, |s| s["type"] = json!("string"));
    breaks(Rule::RootNotObject, |s| {
        s.as_object_mut().unwrap().remove("properties");
    });
}

#[test]
fn catches_root_combinators() {
    for k in ["anyOf", "oneOf", "allOf"] {
        breaks(Rule::RootCombinator, |s| {
            s[k] = json!([{ "type": "object" }, { "type": "null" }])
        });
    }
}

#[test]
fn catches_nested_one_of_all_of_not() {
    for k in ["oneOf", "allOf", "not"] {
        breaks(Rule::Combinator, |s| {
            s["properties"]["path"][k] = json!([{ "type": "string" }])
        });
    }
}

#[test]
fn catches_refs() {
    breaks(Rule::Ref, |s| {
        s["properties"]["path"] = json!({ "$ref": "#/$defs/p" })
    });
    breaks(Rule::Ref, |s| {
        s["$defs"] = json!({ "p": { "type": "string" } })
    });
    breaks(Rule::Ref, |s| {
        s["definitions"] = json!({ "p": { "type": "string" } })
    });
}

#[test]
fn catches_type_arrays() {
    breaks(Rule::TypeArray, |s| {
        s["properties"]["path"]["type"] = json!(["string", "null"])
    });
}

#[test]
fn catches_unknown_and_missing_types() {
    breaks(Rule::UnknownType, |s| {
        s["properties"]["path"]["type"] = json!("text")
    });
    breaks(Rule::UnknownType, |s| {
        s["properties"]["path"]["type"] = json!(3)
    });
    breaks(Rule::MissingType, |s| {
        s["properties"]["path"] = json!({ "description": "x" })
    });
}

#[test]
fn catches_format_and_pattern() {
    breaks(Rule::FormatOrPattern, |s| {
        s["properties"]["path"]["format"] = json!("uri")
    });
    breaks(Rule::FormatOrPattern, |s| {
        s["properties"]["path"]["pattern"] = json!("^a$")
    });
}

#[test]
fn catches_unknown_keywords() {
    for (k, v) in [
        ("default", json!("x")),
        ("minimum", json!(1)),
        ("minLength", json!(1)),
        ("examples", json!(["x"])),
        (
            "$schema",
            json!("https://json-schema.org/draft/2020-12/schema"),
        ),
    ] {
        breaks(Rule::UnknownKeyword, |s| s["properties"]["path"][k] = v);
    }
}

#[test]
fn catches_arrays_without_items() {
    breaks(Rule::ArrayWithoutItems, |s| {
        s["properties"]["ops"]
            .as_object_mut()
            .unwrap()
            .remove("items");
    });
}

#[test]
fn catches_items_on_non_arrays() {
    breaks(Rule::ItemsOnNonArray, |s| {
        s["properties"]["path"]["items"] = json!({ "type": "string" })
    });
}

#[test]
fn catches_object_keywords_on_non_objects() {
    breaks(Rule::PropertiesOnNonObject, |s| {
        s["properties"]["path"]["properties"] = json!({})
    });
}

#[test]
fn catches_bad_property_names() {
    for bad in [
        "Path",
        "file-path",
        "a.b",
        "",
        "x".repeat(65).as_str(),
        "名前",
    ] {
        breaks(Rule::BadPropertyName, |s| {
            let props = s["properties"].as_object_mut().unwrap();
            props.insert(bad.to_string(), json!({ "type": "string" }));
            s["required"].as_array_mut().unwrap().push(json!(bad));
        });
    }
    assert!(is_valid_property_name(&"x".repeat(64)));
}

#[test]
fn catches_properties_missing_from_required() {
    breaks(Rule::NotAllRequired, |s| {
        s["required"] = json!(["path", "ops"])
    });
    breaks(Rule::NotAllRequired, |s| {
        s.as_object_mut().unwrap().remove("required");
    });
    breaks(Rule::NotAllRequired, |s| {
        s["required"].as_array_mut().unwrap().push(json!("ghost"))
    });
    breaks(Rule::NotAllRequired, |s| {
        s["required"].as_array_mut().unwrap().push(json!("path"))
    });
    breaks(Rule::NotAllRequired, |s| {
        s["properties"]["ops"]["items"]["required"] = json!([])
    });
}

#[test]
fn catches_additional_properties() {
    breaks(Rule::AdditionalPropertiesNotFalse, |s| {
        s.as_object_mut().unwrap().remove("additionalProperties");
    });
    breaks(Rule::AdditionalPropertiesNotFalse, |s| {
        s["additionalProperties"] = json!(true)
    });
    breaks(Rule::AdditionalPropertiesNotFalse, |s| {
        s["properties"]["ops"]["items"]["additionalProperties"] = json!({})
    });
}

#[test]
fn catches_bad_any_of() {
    breaks(Rule::BadAnyOf, |s| {
        s["properties"]["mode"]["type"] = json!("string")
    });
    breaks(Rule::BadAnyOf, |s| {
        s["properties"]["mode"]["anyOf"] = json!([{ "type": "string" }])
    });
    breaks(Rule::BadAnyOf, |s| {
        s["properties"]["mode"]["anyOf"] = json!([{ "type": "string" }, { "type": "string" }])
    });
    breaks(Rule::BadAnyOf, |s| {
        s["properties"]["mode"]["anyOf"] = json!([
            { "anyOf": [{ "type": "string" }, { "type": "null" }] },
            { "type": "null" }
        ])
    });
    breaks(Rule::BadAnyOf, |s| {
        s["properties"]["mode"]["anyOf"] = json!([{ "enum": ["a"] }, { "type": "null" }])
    });
}

#[test]
fn any_of_is_types_then_null_last() {
    // Several single types before the null are fine (a typed value).
    let mut s = good();
    s["properties"]["mode"]["anyOf"] = json!([
        { "type": "string" },
        { "type": "number" },
        { "type": "boolean" },
        { "type": "null" }
    ]);
    assert_eq!(rules(s), vec![]);
    // No null at all.
    breaks(Rule::BadAnyOf, |s| {
        s["properties"]["mode"]["anyOf"] = json!([{ "type": "string" }, { "type": "integer" }])
    });
    // Null first.
    breaks(Rule::BadAnyOf, |s| {
        s["properties"]["mode"]["anyOf"] = json!([{ "type": "null" }, { "type": "string" }])
    });
    // Null twice.
    breaks(
        Rule::BadAnyOf,
        |s| {
            s["properties"]["mode"]["anyOf"] =
                json!([{ "type": "null" }, { "type": "string" }, { "type": "null" }])
        },
    );
    // The null branch carries something else.
    breaks(
        Rule::BadAnyOf,
        |s| {
            s["properties"]["mode"]["anyOf"] =
                json!([{ "type": "string" }, { "type": "null", "description": "none" }])
        },
    );
    // A type list hiding inside a branch.
    breaks(Rule::TypeArray, |s| {
        s["properties"]["mode"]["anyOf"] =
            json!([{ "type": ["string", "integer"] }, { "type": "null" }])
    });
}

#[test]
fn keys_beside_any_of_are_still_checked() {
    for (k, v) in [
        ("items", json!({ "type": "string" })),
        ("properties", json!({})),
        ("enum", json!(["a"])),
        ("required", json!([])),
    ] {
        breaks(Rule::BadAnyOf, |s| s["properties"]["mode"][k] = v);
    }
    // What sits beside the anyOf is linted too, not skipped.
    breaks(Rule::Ref, |s| {
        s["properties"]["mode"]["items"] = json!({ "$ref": "#/$defs/x" })
    });
    breaks(Rule::FormatOrPattern, |s| {
        s["properties"]["mode"]["format"] = json!("uri")
    });
    // An anyOf at the root does not hide problems in the properties.
    let mut s = good();
    s["anyOf"] = json!([{ "type": "object" }, { "type": "null" }]);
    s["properties"]["path"]["format"] = json!("uri");
    let found = rules(s);
    assert!(found.contains(&Rule::RootCombinator), "{found:?}");
    assert!(found.contains(&Rule::FormatOrPattern), "{found:?}");
}

#[test]
fn required_entries_must_be_names() {
    for bad in [json!(1), json!(null), json!(["path"]), json!({ "a": 1 })] {
        breaks(Rule::NotAllRequired, |s| {
            s["required"].as_array_mut().unwrap().push(bad)
        });
    }
}

#[test]
fn catches_bad_enums() {
    breaks(Rule::BadEnum, |s| {
        s["properties"]["path"]["enum"] = json!([])
    });
    breaks(Rule::BadEnum, |s| {
        s["properties"]["path"]["enum"] = json!(["a", 1])
    });
    breaks(Rule::BadEnum, |s| {
        s["properties"]["path"]["enum"] = json!(["a", "a"])
    });
    breaks(Rule::BadEnum, |s| {
        s["properties"]["ops"]["items"]["properties"]["op"] =
            json!({ "type": "integer", "enum": ["1"] })
    });
}

#[test]
fn catches_bad_descriptions_and_non_schemas() {
    breaks(Rule::BadDescription, |s| {
        s["properties"]["path"]["description"] = json!(42)
    });
    breaks(Rule::NotASchema, |s| s["properties"]["path"] = json!(true));
}

#[test]
fn violations_point_at_the_problem() {
    let mut s = good();
    s["properties"]["ops"]["items"]["properties"]["op"]["format"] = json!("x");
    let v = lint_schema(&s);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].path, "/properties/ops/items/properties/op");
    assert!(
        v[0].to_string()
            .starts_with("FormatOrPattern at /properties/ops")
    );
}

fn tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": "Does a thing. Use when needed.",
        "inputSchema": good(),
        "annotations": {
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        }
    })
}

fn tool_rules(tools: &[Value]) -> Vec<Rule> {
    lint_tools(tools).into_iter().map(|v| v.rule).collect()
}

#[test]
fn tool_list_good_example_passes() {
    assert_eq!(tool_rules(&[tool("a"), tool("b_2")]), vec![]);
}

#[test]
fn catches_bad_tool_names() {
    for bad in ["", "Read", "read-range", "2read", "x".repeat(65).as_str()] {
        assert!(
            tool_rules(&[tool(bad)]).contains(&Rule::BadToolName),
            "{bad}"
        );
    }
    assert!(tool_rules(&[tool("a"), tool("a")]).contains(&Rule::BadToolName));
}

#[test]
fn catches_bad_tool_descriptions() {
    for d in [json!(""), json!(" "), json!("x".repeat(1025)), Value::Null] {
        let mut t = tool("a");
        t["description"] = d;
        assert!(tool_rules(&[t]).contains(&Rule::BadToolDescription));
    }
}

#[test]
fn catches_missing_annotations() {
    let mut t = tool("a");
    t["annotations"]
        .as_object_mut()
        .unwrap()
        .remove("openWorldHint");
    assert!(tool_rules(&[t]).contains(&Rule::MissingAnnotation));
    let mut t = tool("a");
    t["annotations"]["readOnlyHint"] = json!("yes");
    assert!(tool_rules(&[t]).contains(&Rule::MissingAnnotation));
}

#[test]
fn catches_schema_problems_inside_tools() {
    let mut t = tool("a");
    t["inputSchema"]["$defs"] = json!({});
    let v = lint_tools(&[t]);
    assert_eq!(v[0].rule, Rule::Ref);
    assert_eq!(v[0].path, "/0/inputSchema");
    let mut t = tool("a");
    t.as_object_mut().unwrap().remove("inputSchema");
    assert!(tool_rules(&[t]).contains(&Rule::RootNotObject));
}

#[test]
fn catches_tool_lists_that_are_too_large() {
    let tools: Vec<Value> = (0..40).map(|i| tool(&format!("t{i}"))).collect();
    assert!(tool_rules(&tools).contains(&Rule::ToolsTooLarge));
}
