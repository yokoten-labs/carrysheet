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
pub const ALLOWED_TYPES: &[&str] =
    &["object", "string", "integer", "number", "boolean", "array", "null"];

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
        let at = if self.path.is_empty() { "/" } else { &self.path };
        write!(f, "{:?} at {at}: {}", self.rule, self.detail)
    }
}

/// True for `[a-z0-9_]{1,64}`.
pub fn is_valid_property_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
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
        self.violations.push(Violation { rule, path: path.to_owned(), detail: detail.into() });
    }

    fn root(&mut self, schema: &Value) {
        let Some(map) = schema.as_object() else {
            self.report(Rule::RootNotObject, "", "the root schema is not a JSON object");
            return;
        };
        for k in ["anyOf", "oneOf", "allOf"] {
            if map.contains_key(k) {
                self.report(Rule::RootCombinator, "", format!("`{k}` at the root"));
            }
        }
        if map.get("type").and_then(Value::as_str) != Some("object") {
            self.report(Rule::RootNotObject, "", "the root `type` must be \"object\"");
        }
        if !map.get("properties").is_some_and(Value::is_object) {
            self.report(Rule::RootNotObject, "", "the root has no `properties` object");
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
                "format" | "pattern" => {
                    self.report(Rule::FormatOrPattern, path, format!("`{key}` is not allowed"))
                }
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
    let mut l = Linter { violations: Vec::new() };
    l.root(schema);
    l.violations
}

/// Checks MCP tool definitions (`{"name","description","inputSchema","annotations"}`):
/// names, descriptions, annotations, the total size, and every schema.
pub fn lint_tools(tools: &[Value]) -> Vec<Violation> {
    let mut l = Linter { violations: Vec::new() };
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
            l.report(Rule::BadToolName, &path, format!("tool name \"{name}\" twice"));
        }
        match tool.get("description").and_then(Value::as_str) {
            Some(d) if !d.trim().is_empty() && d.chars().count() <= MAX_DESCRIPTION_CHARS => {}
            _ => l.report(
                Rule::BadToolDescription,
                &path,
                "the description is missing, empty or too long",
            ),
        }
        for key in ["readOnlyHint", "destructiveHint", "idempotentHint", "openWorldHint"] {
            if !tool.get("annotations").and_then(|a| a.get(key)).is_some_and(Value::is_boolean) {
                l.report(Rule::MissingAnnotation, &path, format!("`{key}` is not set"));
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
