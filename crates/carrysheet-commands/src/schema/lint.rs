//! A lint for tool schemas: the rules Claude, Gemini and OpenAI strict mode
//! each enforce, checked together so a schema that passes works in all of them.
//!
//! [`lint_schema`] checks one input schema; [`lint_tools`] checks a list of
//! MCP tool definitions (names, descriptions, annotations, total size, and
//! every schema).

use std::collections::HashSet;
use std::fmt;

use serde_json::Value;

/// Keywords a schema may use. Anything else is refused, because some client
/// somewhere rejects it or silently ignores it.
pub const ALLOWED_KEYWORDS: &[&str] = &[
    "type",
    "description",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "anyOf",
];

/// Types a schema may name.
pub const ALLOWED_TYPES: &[&str] = &[
    "object", "string", "integer", "number", "boolean", "array", "null",
];

/// The most characters all tool definitions together may take (name,
/// description, schema and annotations as compact JSON).
pub const MAX_TOOLS_CHARS: usize = 10_000;

/// The longest tool description OpenAI accepts.
pub const MAX_DESCRIPTION_CHARS: usize = 1024;

/// A rule a schema or tool list can break.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rule {
    /// The root must be `{"type": "object", "properties": {...}}`.
    RootNotObject,
    /// No `anyOf`/`oneOf`/`allOf` at the root (Claude).
    RootCombinator,
    /// No `oneOf`, `allOf` or `not` anywhere; `anyOf` only for nullable properties.
    Combinator,
    /// `anyOf` must list one or more single-type schemas of different types,
    /// then exactly `{"type": "null"}` last; nothing nested, and nothing but
    /// `description` beside it.
    BadAnyOf,
    /// No `$ref`, `$defs` or `definitions` (Gemini).
    Ref,
    /// `type` must be one string, never a list like `["string","null"]` (Gemini).
    TypeArray,
    /// `type` must be one of [`ALLOWED_TYPES`].
    UnknownType,
    /// Every schema needs a `type` (or a nullable `anyOf`).
    MissingType,
    /// No `format` or `pattern`; we validate on our side.
    FormatOrPattern,
    /// Only keywords in [`ALLOWED_KEYWORDS`].
    UnknownKeyword,
    /// Every array needs `items` (Gemini).
    ArrayWithoutItems,
    /// Only arrays may have `items` (Gemini).
    ItemsOnNonArray,
    /// Only objects may have `properties`, `required` or `additionalProperties`.
    PropertiesOnNonObject,
    /// Property names must match `[a-z0-9_]{1,64}`.
    BadPropertyName,
    /// Every property must be listed in `required`, and nothing else (OpenAI strict).
    NotAllRequired,
    /// Every object must set `additionalProperties: false` (OpenAI strict).
    AdditionalPropertiesNotFalse,
    /// `enum` must be a non-empty list of distinct strings on a string type.
    BadEnum,
    /// `description` must be text.
    BadDescription,
    /// A schema must be a JSON object.
    NotASchema,
    /// Tool names must match `[a-z][a-z0-9_]{0,63}` and be unique.
    BadToolName,
    /// Tool descriptions must be non-empty and at most [`MAX_DESCRIPTION_CHARS`].
    BadToolDescription,
    /// Every tool must set all four annotations to true or false.
    MissingAnnotation,
    /// All tool definitions together must stay under [`MAX_TOOLS_CHARS`].
    ToolsTooLarge,
}

/// One broken rule, with where it was found (a JSON pointer) and details.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub rule: Rule,
    pub path: String,
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let at = if self.path.is_empty() {
            "/"
        } else {
            &self.path
        };
        write!(f, "{:?} at {at}: {}", self.rule, self.detail)
    }
}

/// True for `[a-z0-9_]{1,64}`.
pub fn is_valid_property_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// True for `[a-z][a-z0-9_]{0,63}`.
pub fn is_valid_tool_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_lowercase()) && is_valid_property_name(name)
}

struct Linter {
    violations: Vec<Violation>,
}

impl Linter {
    fn report(&mut self, rule: Rule, path: &str, detail: impl Into<String>) {
        self.violations.push(Violation {
            rule,
            path: path.to_owned(),
            detail: detail.into(),
        });
    }

    fn root(&mut self, schema: &Value) {
        let Some(map) = schema.as_object() else {
            self.report(
                Rule::RootNotObject,
                "",
                "the root schema is not a JSON object",
            );
            return;
        };
        for k in ["anyOf", "oneOf", "allOf"] {
            if map.contains_key(k) {
                self.report(Rule::RootCombinator, "", format!("`{k}` at the root"));
            }
        }
        if map.get("type").and_then(Value::as_str) != Some("object") {
            self.report(
                Rule::RootNotObject,
                "",
                "the root `type` must be \"object\"",
            );
        }
        if !map.get("properties").is_some_and(Value::is_object) {
            self.report(
                Rule::RootNotObject,
                "",
                "the root has no `properties` object",
            );
        }
        self.node(schema, "", true);
    }

    fn node(&mut self, schema: &Value, path: &str, is_root: bool) {
        let Some(map) = schema.as_object() else {
            self.report(Rule::NotASchema, path, "a schema must be a JSON object");
            return;
        };

        for key in map.keys() {
            match key.as_str() {
                "$ref" | "$defs" | "definitions" => {
                    self.report(Rule::Ref, path, format!("`{key}` is not allowed"))
                }
                "format" | "pattern" => self.report(
                    Rule::FormatOrPattern,
                    path,
                    format!("`{key}` is not allowed"),
                ),
                "oneOf" | "allOf" | "not" if !is_root => {
                    self.report(Rule::Combinator, path, format!("`{key}` is not allowed"))
                }
                k if ALLOWED_KEYWORDS.contains(&k) => {}
                "oneOf" | "allOf" | "anyOf" => {} // reported as RootCombinator
                k => self.report(Rule::UnknownKeyword, path, format!("`{k}` is not allowed")),
            }
        }

        if let Some(d) = map.get("description")
            && !d.is_string()
        {
            self.report(Rule::BadDescription, path, "`description` is not text");
        }

        // The root's `anyOf` is already reported; check the rest of it as usual.
        let any_of = map.get("anyOf").filter(|_| !is_root);
        if let Some(branches) = any_of {
            self.any_of(map, branches, path);
            // A well-formed nullable property has nothing else to check.
            // Anything beside the `anyOf` was reported above, and is still
            // checked below so problems inside it are not hidden.
            if map.keys().all(|k| k == "anyOf" || k == "description") {
                return;
            }
        }

        let ty = match map.get("type") {
            None if any_of.is_some() => None,
            None => {
                self.report(Rule::MissingType, path, "no `type`");
                None
            }
            Some(Value::String(t)) => {
                if !ALLOWED_TYPES.contains(&t.as_str()) {
                    self.report(Rule::UnknownType, path, format!("unknown type \"{t}\""));
                }
                Some(t.as_str())
            }
            Some(Value::Array(_)) => {
                self.report(Rule::TypeArray, path, "`type` is a list; use one type");
                None
            }
            Some(other) => {
                self.report(Rule::UnknownType, path, format!("`type` is {other}"));
                None
            }
        };

        match (ty, map.get("items")) {
            (Some("array"), None) => {
                self.report(Rule::ArrayWithoutItems, path, "an array without `items`")
            }
            (Some("array"), Some(items)) => self.node(items, &format!("{path}/items"), false),
            (_, Some(items)) => {
                self.report(
                    Rule::ItemsOnNonArray,
                    path,
                    "`items` on something that is not an array",
                );
                // Still look inside, so nothing hides behind a misplaced key.
                self.node(items, &format!("{path}/items"), false);
            }
            (_, None) => {}
        }

        if ty == Some("object") {
            self.object(map, path);
        } else {
            for k in ["properties", "required", "additionalProperties"] {
                if map.contains_key(k) {
                    self.report(
                        Rule::PropertiesOnNonObject,
                        path,
                        format!("`{k}` on something that is not an object"),
                    );
                }
            }
            if let Some(Value::Object(props)) = map.get("properties") {
                for (name, prop) in props {
                    self.node(prop, &format!("{path}/properties/{name}"), false);
                }
            }
        }

        if let Some(values) = map.get("enum") {
            let ok = ty == Some("string")
                && values.as_array().is_some_and(|vs| {
                    let strings: Vec<_> = vs.iter().filter_map(Value::as_str).collect();
                    let unique: HashSet<_> = strings.iter().collect();
                    !vs.is_empty() && strings.len() == vs.len() && unique.len() == vs.len()
                });
            if !ok {
                self.report(
                    Rule::BadEnum,
                    path,
                    "`enum` must be a non-empty list of distinct strings on a string",
                );
            }
        }
    }

    fn any_of(&mut self, map: &serde_json::Map<String, Value>, branches: &Value, path: &str) {
        for key in map.keys() {
            if key != "anyOf" && key != "description" {
                self.report(
                    Rule::BadAnyOf,
                    path,
                    format!("`{key}` beside `anyOf`; only `description` may be there"),
                );
            }
        }
        let Some(list) = branches.as_array().filter(|l| l.len() >= 2) else {
            self.report(
                Rule::BadAnyOf,
                path,
                "`anyOf` must list at least two schemas: the value's type, then {\"type\": \"null\"}",
            );
            return;
        };
        let null = serde_json::json!({ "type": "null" });
        if list.last() != Some(&null) {
            self.report(
                Rule::BadAnyOf,
                path,
                "the last branch of `anyOf` must be exactly {\"type\": \"null\"}",
            );
        }
        let mut types = HashSet::new();
        for (i, branch) in list.iter().enumerate().take(list.len() - 1) {
            let bpath = format!("{path}/anyOf/{i}");
            let Some(bmap) = branch.as_object() else {
                self.report(Rule::NotASchema, &bpath, "a schema must be a JSON object");
                continue;
            };
            if bmap.contains_key("anyOf") {
                self.report(Rule::BadAnyOf, &bpath, "nested `anyOf`");
                continue;
            }
            match bmap.get("type").and_then(Value::as_str) {
                Some("null") => self.report(
                    Rule::BadAnyOf,
                    &bpath,
                    "only the last branch may be {\"type\": \"null\"}",
                ),
                Some(t) => {
                    if !types.insert(t.to_owned()) {
                        self.report(Rule::BadAnyOf, &bpath, format!("type \"{t}\" twice"));
                    }
                }
                None => self.report(Rule::BadAnyOf, &bpath, "each branch needs one `type`"),
            }
            self.node(branch, &bpath, false);
        }
    }

    fn object(&mut self, map: &serde_json::Map<String, Value>, path: &str) {
        if map.get("additionalProperties") != Some(&Value::Bool(false)) {
            self.report(
                Rule::AdditionalPropertiesNotFalse,
                path,
                "objects must set `additionalProperties: false`",
            );
        }
        let empty = serde_json::Map::new();
        let props = match map.get("properties") {
            Some(Value::Object(p)) => p,
            Some(_) => {
                self.report(Rule::NotASchema, path, "`properties` is not an object");
                &empty
            }
            None => &empty,
        };
        let required: Vec<&str> = match map.get("required") {
            Some(Value::Array(r)) => {
                if r.iter().any(|n| !n.is_string()) {
                    self.report(
                        Rule::NotAllRequired,
                        path,
                        "`required` lists something that is not a property name",
                    );
                }
                r.iter().filter_map(Value::as_str).collect()
            }
            Some(_) => {
                self.report(Rule::NotAllRequired, path, "`required` is not a list");
                Vec::new()
            }
            None => Vec::new(),
        };
        let required_set: HashSet<&str> = required.iter().copied().collect();
        for name in props.keys() {
            if !required_set.contains(name.as_str()) {
                self.report(
                    Rule::NotAllRequired,
                    path,
                    format!("property `{name}` is not in `required`; make it nullable instead"),
                );
            }
        }
        for name in &required {
            if !props.contains_key(*name) {
                self.report(
                    Rule::NotAllRequired,
                    path,
                    format!("`required` names `{name}`, which is not a property"),
                );
            }
        }
        if required_set.len() != required.len() {
            self.report(Rule::NotAllRequired, path, "`required` lists a name twice");
        }
        for (name, prop) in props {
            if !is_valid_property_name(name) {
                self.report(
                    Rule::BadPropertyName,
                    path,
                    format!("property name `{name}` must match [a-z0-9_]{{1,64}}"),
                );
            }
            self.node(prop, &format!("{path}/properties/{name}"), false);
        }
    }
}

/// Checks one input schema. Returns every violation; empty means it passes.
pub fn lint_schema(schema: &Value) -> Vec<Violation> {
    let mut l = Linter {
        violations: Vec::new(),
    };
    l.root(schema);
    l.violations
}

/// Checks MCP tool definitions (`{"name","description","inputSchema","annotations"}`):
/// names, descriptions, annotations, the total size, and every schema.
pub fn lint_tools(tools: &[Value]) -> Vec<Violation> {
    let mut l = Linter {
        violations: Vec::new(),
    };
    let mut names = HashSet::new();
    let mut total = 0;
    for (i, tool) in tools.iter().enumerate() {
        let path = format!("/{i}");
        total += tool.to_string().chars().count();
        let name = tool.get("name").and_then(Value::as_str).unwrap_or_default();
        if !is_valid_tool_name(name) {
            l.report(
                Rule::BadToolName,
                &path,
                format!("tool name \"{name}\" must match [a-z][a-z0-9_]{{0,63}}"),
            );
        } else if !names.insert(name.to_owned()) {
            l.report(
                Rule::BadToolName,
                &path,
                format!("tool name \"{name}\" twice"),
            );
        }
        match tool.get("description").and_then(Value::as_str) {
            Some(d) if !d.trim().is_empty() && d.chars().count() <= MAX_DESCRIPTION_CHARS => {}
            _ => l.report(
                Rule::BadToolDescription,
                &path,
                "the description is missing, empty or too long",
            ),
        }
        for key in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            if !tool
                .get("annotations")
                .and_then(|a| a.get(key))
                .is_some_and(Value::is_boolean)
            {
                l.report(
                    Rule::MissingAnnotation,
                    &path,
                    format!("`{key}` is not set"),
                );
            }
        }
        match tool.get("inputSchema") {
            Some(schema) => {
                for mut v in lint_schema(schema) {
                    v.path = format!("{path}/inputSchema{}", v.path);
                    l.violations.push(v);
                }
            }
            None => l.report(Rule::RootNotObject, &path, "no `inputSchema`"),
        }
    }
    if total > MAX_TOOLS_CHARS {
        l.report(
            Rule::ToolsTooLarge,
            "",
            format!("tool definitions take {total} characters; the limit is {MAX_TOOLS_CHARS}"),
        );
    }
    l.violations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{input_schema, tool_definition};
    use crate::table::{COMMANDS, mcp_tools};
    use serde_json::json;

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
}
