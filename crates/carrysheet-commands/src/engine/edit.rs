//! `edit` operations: parsed and checked once here, applied by the engine.

use carrysheet_model::{SheetRange, validate_sheet_name};
use serde::Serialize;
use serde_json::Value;

use crate::error::{CommandError, ErrorCode};

pub use crate::limits::{MAX_BATCH_CELLS, MAX_FILL_CELLS, MAX_OPS};

/// Most cell changes listed in an edit result; the rest are counted.
pub const MAX_LISTED_CHANGES: usize = 200;

/// What an operation does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EditKind {
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

impl EditKind {
    pub fn parse(s: &str) -> Option<EditKind> {
        Some(match s {
            "set_value" => EditKind::SetValue,
            "set_formula" => EditKind::SetFormula,
            "clear" => EditKind::Clear,
            "insert_rows" => EditKind::InsertRows,
            "delete_rows" => EditKind::DeleteRows,
            "insert_columns" => EditKind::InsertColumns,
            "delete_columns" => EditKind::DeleteColumns,
            "add_sheet" => EditKind::AddSheet,
            "rename_sheet" => EditKind::RenameSheet,
            "delete_sheet" => EditKind::DeleteSheet,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EditKind::SetValue => "set_value",
            EditKind::SetFormula => "set_formula",
            EditKind::Clear => "clear",
            EditKind::InsertRows => "insert_rows",
            EditKind::DeleteRows => "delete_rows",
            EditKind::InsertColumns => "insert_columns",
            EditKind::DeleteColumns => "delete_columns",
            EditKind::AddSheet => "add_sheet",
            EditKind::RenameSheet => "rename_sheet",
            EditKind::DeleteSheet => "delete_sheet",
        }
    }

    /// Works on a sheet by name rather than on a range.
    pub fn is_sheet_op(self) -> bool {
        matches!(
            self,
            EditKind::AddSheet | EditKind::RenameSheet | EditKind::DeleteSheet
        )
    }

    /// Moves cells or sheets around, so cells cannot be compared by address
    /// before and after.
    pub fn is_structural(self) -> bool {
        !matches!(
            self,
            EditKind::SetValue | EditKind::SetFormula | EditKind::Clear
        )
    }
}

/// A value for `set_value`, kept with the type it was sent with.
#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Text(String),
    Number(f64),
    Bool(bool),
}

/// What an operation works on.
#[derive(Debug, Clone, PartialEq)]
pub enum EditTarget {
    /// Cells, rows or columns; no sheet name means the first sheet.
    Range(SheetRange),
    /// A sheet name.
    Sheet(String),
}

/// One checked operation.
#[derive(Debug, Clone, PartialEq)]
pub struct EditOp {
    pub kind: EditKind,
    pub target: EditTarget,
    /// For `set_value`.
    pub value: Option<Scalar>,
    /// For `set_formula` (without `=`), `rename_sheet` (the new name).
    pub text: Option<String>,
    /// For row and column operations; `None` means the target's rows or columns.
    pub count: Option<u32>,
}

impl EditOp {
    /// The target as written back in results, such as `Data!B2:B9`.
    pub fn target_text(&self) -> String {
        match &self.target {
            EditTarget::Range(r) => r.to_string(),
            EditTarget::Sheet(s) => s.clone(),
        }
    }

    /// The range of a cell, row or column operation.
    pub fn range(&self) -> Option<&SheetRange> {
        match &self.target {
            EditTarget::Range(r) => Some(r),
            EditTarget::Sheet(_) => None,
        }
    }
}

fn op_error(i: usize, why: impl Into<String>, fix: impl Into<String>) -> CommandError {
    CommandError::new(
        ErrorCode::InvalidArgument,
        format!("ops[{i}] is not valid"),
        why,
        fix,
    )
}

/// Reads the validated `ops` argument of `edit`.
pub fn parse_ops(ops: &Value) -> Result<Vec<EditOp>, CommandError> {
    let Some(items) = ops.as_array() else {
        return Err(CommandError::invalid_argument(
            "ops",
            "ops is not a list",
            "Pass a list like [{\"op\":\"set_value\",\"target\":\"Sheet1!B2\",\"value\":3}].",
        ));
    };
    if items.is_empty() {
        return Err(CommandError::invalid_argument(
            "ops",
            "the list of operations is empty",
            "Add at least one operation, like {\"op\":\"set_value\",\"target\":\"B2\",\"value\":3}.",
        ));
    }
    if items.len() > MAX_OPS {
        return Err(CommandError::new(
            ErrorCode::LimitExceeded,
            format!("{} operations in one call", items.len()),
            format!("one edit call takes at most {MAX_OPS} operations"),
            "Split the operations over several calls, or fill a range with one set_value or set_formula.",
        ));
    }
    let ops: Vec<EditOp> = items
        .iter()
        .enumerate()
        .map(|(i, item)| parse_op(i, item))
        .collect::<Result<_, _>>()?;
    let filled: u64 = ops
        .iter()
        .filter(|op| matches!(op.kind, EditKind::SetValue | EditKind::SetFormula))
        .map(|op| op.range().map_or(0, |r| r.range.cell_count()))
        .sum();
    if filled > MAX_BATCH_CELLS {
        return Err(CommandError::new(
            ErrorCode::LimitExceeded,
            format!("the operations fill {filled} cells in one call"),
            format!(
                "set_value and set_formula fill at most {MAX_BATCH_CELLS} cells in one edit call, all operations together"
            ),
            "Split the operations over several edit calls, each writing to the file the last one wrote.",
        ));
    }
    Ok(ops)
}

fn parse_op(i: usize, item: &Value) -> Result<EditOp, CommandError> {
    let op = item.get("op").and_then(Value::as_str).unwrap_or("");
    let kind = EditKind::parse(op).ok_or_else(|| {
        op_error(
            i,
            "op is not one of the known operations",
            "Use set_value, set_formula, clear, insert_rows, delete_rows, insert_columns, delete_columns, add_sheet, rename_sheet or delete_sheet.",
        )
    })?;
    let target_text = item.get("target").and_then(Value::as_str).unwrap_or("");
    let value = item.get("value").filter(|v| !v.is_null());
    let count = item
        .get("count")
        .and_then(Value::as_u64)
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX));

    let target = if kind.is_sheet_op() {
        let name = target_text.trim();
        if name.is_empty() {
            return Err(op_error(
                i,
                "target is empty; sheet operations name a sheet",
                "Set target to the sheet name, like \"Q1\".",
            ));
        }
        if kind == EditKind::AddSheet {
            validate_sheet_name(name).map_err(|e| {
                op_error(
                    i,
                    format!("the new sheet name is not allowed: {e}"),
                    "Use 1 to 31 characters without : \\ / ? * [ ].",
                )
            })?;
        }
        EditTarget::Sheet(name.to_owned())
    } else {
        let range = SheetRange::parse(target_text).map_err(|e| {
            op_error(
                i,
                format!("target is not a range: {e}"),
                "Write a range like Sheet1!B2:B9, B2, 'My sheet'!A:C or 5:7.",
            )
        })?;
        EditTarget::Range(range)
    };

    let mut parsed = EditOp {
        kind,
        target,
        value: None,
        text: None,
        count,
    };
    match kind {
        EditKind::SetValue => {
            parsed.value = Some(match value {
                Some(Value::String(s)) => Scalar::Text(s.clone()),
                Some(Value::Bool(b)) => Scalar::Bool(*b),
                Some(Value::Number(n)) => match n.as_f64() {
                    Some(f) if f.is_finite() => Scalar::Number(f),
                    _ => {
                        return Err(op_error(
                            i,
                            "value is not a finite number",
                            "Send a number like 3 or 2.5.",
                        ));
                    }
                },
                _ => {
                    return Err(op_error(
                        i,
                        "set_value needs a value",
                        "Add \"value\": text, a number or true/false. To empty cells, use op clear.",
                    ));
                }
            });
            check_fill_size(i, &parsed)?;
        }
        EditKind::SetFormula => {
            let Some(Value::String(f)) = value else {
                return Err(op_error(
                    i,
                    "set_formula needs the formula as text in value",
                    "Add \"value\": \"SUM(B2:B9)\" (with or without =).",
                ));
            };
            let f = f.trim();
            let f = f.strip_prefix('=').unwrap_or(f).trim();
            if f.is_empty() {
                return Err(op_error(
                    i,
                    "the formula is empty",
                    "Add \"value\": \"SUM(B2:B9)\".",
                ));
            }
            if let Some(why) = refused_formula(f) {
                return Err(CommandError::new(
                    ErrorCode::InvalidArgument,
                    format!("ops[{i}] writes a formula Carrysheet does not let agents write"),
                    why,
                    "Write the value instead, or ask a person to add this formula in a spreadsheet app.",
                ));
            }
            parsed.text = Some(f.to_owned());
            check_fill_size(i, &parsed)?;
        }
        EditKind::RenameSheet => {
            let Some(Value::String(name)) = value else {
                return Err(op_error(
                    i,
                    "rename_sheet needs the new name as text in value",
                    "Add \"value\": \"New name\".",
                ));
            };
            let name = name.trim();
            validate_sheet_name(name).map_err(|e| {
                op_error(
                    i,
                    format!("the new sheet name is not allowed: {e}"),
                    "Use 1 to 31 characters without : \\ / ? * [ ].",
                )
            })?;
            parsed.text = Some(name.to_owned());
        }
        _ => {}
    }
    Ok(parsed)
}

fn check_fill_size(i: usize, op: &EditOp) -> Result<(), CommandError> {
    let cells = op.range().map_or(0, |r| r.range.cell_count());
    if cells > MAX_FILL_CELLS {
        return Err(CommandError::new(
            ErrorCode::LimitExceeded,
            format!("ops[{i}] fills {cells} cells"),
            format!(
                "{} fills at most {MAX_FILL_CELLS} cells per operation",
                op.kind.as_str()
            ),
            "Give a smaller range, like B2:B1000 rather than B:B.",
        ));
    }
    Ok(())
}

/// Functions an agent may not write, with the reason. They reach outside
/// the file (the web, other files or programs, an outside service), or run
/// code. Matched as names, called or not: `MAP(x,WEBSERVICE)` passes the
/// function as a value.
const REFUSED_FUNCTIONS: &[(&str, &str)] = &[
    ("WEBSERVICE", "it fetches a web address"),
    (
        "FILTERXML",
        "it is used with WEBSERVICE to read fetched pages",
    ),
    ("IMPORTXML", "it fetches a web page"),
    ("IMPORTDATA", "it fetches a file from the web"),
    ("STOCKHISTORY", "it fetches data from an online service"),
    ("COPILOT", "it sends cell data to an online AI service"),
    ("PY", "it runs Python code (in Microsoft's cloud)"),
    ("CALL", "it calls a function in a program library (DLL)"),
    (
        "REGISTER",
        "it loads a function from a program library (DLL)",
    ),
    (
        "REGISTER.ID",
        "it loads a function from a program library (DLL)",
    ),
    (
        "EXEC",
        "it starts another program (an Excel 4 macro function)",
    ),
    ("DDE", "it talks to another program over DDE"),
    ("RTD", "it reads live data from another program"),
    ("SQL.REQUEST", "it queries an outside database"),
    (
        "HYPERLINK",
        "it makes a link that can open files or web addresses",
    ),
    ("IMAGE", "it downloads a picture from a web address"),
    (
        "TRANSLATE",
        "it sends cell text to an online translation service",
    ),
    (
        "DETECTLANGUAGE",
        "it sends cell text to an online language service",
    ),
    (
        "IMPORTRANGE",
        "it reads data from another online spreadsheet",
    ),
    ("IMPORTHTML", "it fetches a web page"),
    ("IMPORTFEED", "it fetches a web feed"),
    ("GOOGLEFINANCE", "it fetches data from an online service"),
    ("CUBEVALUE", "it queries an outside data cube (OLAP) server"),
    (
        "CUBEMEMBER",
        "it queries an outside data cube (OLAP) server",
    ),
    (
        "CUBEMEMBERPROPERTY",
        "it queries an outside data cube (OLAP) server",
    ),
    (
        "CUBERANKEDMEMBER",
        "it queries an outside data cube (OLAP) server",
    ),
    ("CUBESET", "it queries an outside data cube (OLAP) server"),
    (
        "CUBESETCOUNT",
        "it queries an outside data cube (OLAP) server",
    ),
    (
        "CUBEKPIMEMBER",
        "it queries an outside data cube (OLAP) server",
    ),
];

/// Prefixes files put before function and parameter names. Any number of
/// them, in any order, is stripped before a name is compared.
const NAME_PREFIXES: &[&str] = &["_XLFN.", "_XLWS.", "_XLUDF.", "_XLPM.", "_XLETA."];

/// One piece of a formula, as [`refused_formula`] reads it.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// A name, a cell reference or a number: `SUM`, `_xlfn.XLOOKUP`, `$A$1`, `3.5`.
    Word(String),
    /// `'...'`: a quoted sheet name, or a path to another file.
    Quoted(String),
    /// `[...]` with what is inside (nested brackets included).
    Bracket(String),
    /// Any other character outside strings: `(`, `:`, `!`, `|`, `,` and so on.
    Symbol(char),
}

/// Splits a formula into tokens. String literals and white space are dropped.
fn tokens(formula: &str) -> Vec<Token> {
    let chars: Vec<char> = formula.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let is_word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '.' | '\\' | '$' | '?');
    while let Some(&c) = chars.get(i) {
        match c {
            '"' => {
                // "" inside a string is a quote.
                i += 1;
                while let Some(&d) = chars.get(i) {
                    i += 1;
                    if d == '"' {
                        if chars.get(i) == Some(&'"') {
                            i += 1;
                        } else {
                            break;
                        }
                    }
                }
            }
            '\'' => {
                let mut text = String::new();
                i += 1;
                while let Some(&d) = chars.get(i) {
                    i += 1;
                    if d == '\'' {
                        if chars.get(i) == Some(&'\'') {
                            text.push('\'');
                            i += 1;
                        } else {
                            break;
                        }
                    } else {
                        text.push(d);
                    }
                }
                out.push(Token::Quoted(text));
            }
            '[' => {
                let mut depth = 0usize;
                let mut text = String::new();
                while let Some(&d) = chars.get(i) {
                    i += 1;
                    match d {
                        '[' => depth += 1,
                        ']' => depth = depth.saturating_sub(1),
                        // `'` escapes a bracket in a column name: `[a']b]`.
                        '\'' => {
                            if let Some(&e) = chars.get(i) {
                                text.push(d);
                                text.push(e);
                                i += 1;
                            }
                            continue;
                        }
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                    if !(d == '[' && depth == 1) {
                        text.push(d);
                    }
                }
                out.push(Token::Bracket(text));
            }
            c if c.is_whitespace() => i += 1,
            c if is_word(c) => {
                let mut word = String::new();
                while let Some(&d) = chars.get(i).filter(|d| is_word(**d)) {
                    word.push(d);
                    i += 1;
                }
                out.push(Token::Word(word));
            }
            c => {
                out.push(Token::Symbol(c));
                i += 1;
            }
        }
    }
    out
}

/// A name with every known prefix stripped, upper case: `_xlws._xlfn.webservice`
/// gives `WEBSERVICE`.
fn bare_name(word: &str) -> String {
    let mut name = word.to_ascii_uppercase();
    while let Some(p) = NAME_PREFIXES.iter().find(|p| name.starts_with(**p)) {
        name.drain(..p.len());
    }
    name
}

/// Why a formula is refused when an agent writes it (docs/agents.md,
/// "Dangerous formulas"), or `None`. Reads the formula as tokens, so text in
/// string literals never counts, and checks for: refused functions, called or
/// passed as a value, with any `_xlfn.`-style prefixes; links to other
/// workbooks (`[1]Sheet!A1`, `[Book.xlsx]Sheet!A1`, `'C:\x\[a.xlsx]S'!A1`);
/// and DDE (`app|topic!item`). Table references such as `Table1[Amount]` and
/// `[@Amount]` are allowed.
pub fn refused_formula(formula: &str) -> Option<String> {
    let toks = tokens(formula);
    for (i, tok) in toks.iter().enumerate() {
        let next = toks.get(i + 1);
        let prev = i.checked_sub(1).and_then(|p| toks.get(p));
        match tok {
            Token::Symbol('|') => {
                return Some("it uses DDE (app|topic), which talks to other programs".into());
            }
            Token::Quoted(q) if q.contains(['[', ':', '\\', '/']) => {
                return Some("it reads another file through a quoted reference".into());
            }
            // A bracket followed by a sheet or a name (`[1]Sheet1!A1`, `[1]!Name`)
            // names another workbook; a table reference is followed by an
            // operator, a comma, a parenthesis or nothing.
            Token::Bracket(_) => {
                if matches!(
                    next,
                    Some(Token::Word(_) | Token::Quoted(_) | Token::Symbol('!'))
                ) {
                    return Some("it links to another workbook".into());
                }
            }
            Token::Word(w) => {
                // `RTD:RTD` and `PY:PY` are columns, not functions. A name
                // followed by `(` is a call even next to `:`
                // (`A1:WEBSERVICE("…")`), so it is always checked.
                let call = matches!(next, Some(Token::Symbol('(')));
                let in_range = matches!(next, Some(Token::Symbol(':')))
                    || matches!(prev, Some(Token::Symbol(':')));
                if !call && (in_range || w.contains('$')) {
                    continue;
                }
                let name = bare_name(w);
                if let Some((name, why)) = REFUSED_FUNCTIONS.iter().find(|(n, _)| *n == name) {
                    return Some(format!("{name} is refused: {why}"));
                }
            }
            _ => {}
        }
    }
    None
}

/// What one cell held before and after an edit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CellChange {
    pub sheet: String,
    pub cell: String,
    /// The value, or `{"formula", "value"}` for a formula; `null` when empty.
    pub before: Value,
    pub after: Value,
}

/// What one operation did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpResult {
    pub op: EditKind,
    pub target: String,
    /// A short note, like `3 rows inserted before row 5`.
    pub done: String,
}

/// What an edit did, for the change list and the review log.
#[derive(Debug, Clone, PartialEq, Serialize, Default)]
pub struct EditResult {
    pub ops: Vec<OpResult>,
    /// Cells the operations wrote, up to [`MAX_LISTED_CHANGES`].
    pub changes: Vec<CellChange>,
    /// All cells the operations wrote.
    pub changes_total: u64,
    /// Formula cells whose result changed because of the edit (not
    /// counted when rows, columns or sheets moved), up to
    /// [`MAX_LISTED_CHANGES`].
    pub recalculated: Vec<CellChange>,
    pub recalculated_total: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ops_parse_with_their_types() {
        let ops = parse_ops(&json!([
            { "op": "set_value", "target": "Data!B2", "value": "3" },
            { "op": "set_value", "target": "B3", "value": 3 },
            { "op": "set_formula", "target": "B4:B6", "value": "=SUM(A1:A3)" },
            { "op": "insert_rows", "target": "5:7", "count": null },
            { "op": "rename_sheet", "target": "Data", "value": "Q1" },
        ]))
        .unwrap();
        assert_eq!(ops[0].value, Some(Scalar::Text("3".into())));
        assert_eq!(ops[1].value, Some(Scalar::Number(3.0)));
        assert_eq!(ops[2].text.as_deref(), Some("SUM(A1:A3)"));
        assert_eq!(ops[3].count, None);
        assert_eq!(ops[3].target_text(), "5:7");
        assert_eq!(ops[4].text.as_deref(), Some("Q1"));
    }

    #[test]
    fn bad_ops_name_their_index() {
        for (bad, code) in [
            (
                json!([{ "op": "set_value", "target": "B2" }]),
                ErrorCode::InvalidArgument,
            ),
            (
                json!([{ "op": "set_value", "target": "nope!!", "value": 1 }]),
                ErrorCode::InvalidArgument,
            ),
            (
                json!([{ "op": "set_formula", "target": "B2", "value": 1 }]),
                ErrorCode::InvalidArgument,
            ),
            (
                json!([{ "op": "add_sheet", "target": "a/b" }]),
                ErrorCode::InvalidArgument,
            ),
            (
                json!([{ "op": "set_value", "target": "A:Z", "value": 1 }]),
                ErrorCode::LimitExceeded,
            ),
        ] {
            let e = parse_ops(&bad).unwrap_err();
            assert_eq!(e.code, code, "{bad}");
            assert!(e.what.contains("ops[0]"), "{}", e.what);
        }
        assert_eq!(
            parse_ops(&json!([])).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn a_batch_has_a_cell_limit_of_its_own() {
        // Columns A, B, ..., Z, AA, ...
        let col = |i: u8| {
            if i < 26 {
                char::from(b'A' + i).to_string()
            } else {
                format!("A{}", char::from(b'A' + i - 26))
            }
        };
        // 30 operations of 100,000 cells: each is allowed, the batch is not.
        let ops: Vec<Value> = (0..30u8)
            .map(|i| {
                let c = col(i);
                json!({ "op": "set_value", "target": format!("{c}1:{c}100000"), "value": 1 })
            })
            .collect();
        let e = parse_ops(&Value::Array(ops)).unwrap_err();
        assert_eq!(e.code, ErrorCode::LimitExceeded);
        assert!(e.what.contains("3000000"), "{}", e.what);
        assert!(!e.fix.is_empty() && !e.why.is_empty());
        let ok: Vec<Value> = (0..10u8)
            .map(|i| {
                let c = col(i);
                json!({ "op": "set_value", "target": format!("{c}1:{c}100000"), "value": 1 })
            })
            .collect();
        assert!(parse_ops(&Value::Array(ok)).is_ok());
    }

    #[test]
    fn dangerous_formulas_are_refused() {
        for f in [
            "WEBSERVICE(\"http://x\")",
            "_xlfn.webservice(A1)",
            "_xlws._xlfn.WEBSERVICE(\"http://x\")",
            "_xlfn._xlws._xlfn.WEBSERVICE(A1)",
            "WEBSERVICE (\"http://x\")",
            "WEBSERVICE\t(\"http://x\")",
            "MAP({\"http://x\"},WEBSERVICE)",
            "BYROW(A1:A3,_xleta.WEBSERVICE)",
            "LET(f,WEBSERVICE,f(\"http://x\"))",
            "[1]Sheet1!A1",
            "[Book2.xlsx]Sheet1!A1",
            "[1]!SalesTotal",
            "SUM([1]Sheet1!A1:A3)",
            "'C:\\x\\[a.xlsx]S'!A1",
            "cmd|' /c calc'!A0",
            "SUM(A1, hyperlink(\"file:///x\"))",
            "COPILOT(\"sum up\",A1:A9)",
            "STOCKHISTORY(\"MSFT\",TODAY())",
            "_xlfn._xlws.PY(\"1+1\",0)",
            "IMPORTXML(\"http://x\",\"//a\")",
            "IMPORTDATA(\"http://x\")",
            "DDE(\"app\",\"topic\",\"item\")",
            "EXEC(\"calc.exe\")",
            // A name next to `:` is a column only when no `(` follows (round 2, N1).
            "A1:WEBSERVICE(\"http://x\")",
            "LET(r,A1:WEBSERVICE(\"http://x\"),1)",
            "INDEX(A:A,1):HYPERLINK(\"http://x\")",
            "WEBSERVICE (\"http://x\"):A1",
            // Round 2, N7.
            "TRANSLATE(\"hi\",\"en\",\"de\")",
            "DETECTLANGUAGE(A1)",
            "IMPORTRANGE(\"x\",\"A1\")",
            "IMPORTHTML(\"http://x\",\"table\",1)",
            "IMPORTFEED(\"http://x\")",
            "GOOGLEFINANCE(\"GOOG\")",
            "CUBEVALUE(\"c\",\"m\")",
            "CUBEMEMBER(\"c\",\"m\")",
            "CUBESET(\"c\",\"s\")",
            "CUBESETCOUNT(A1)",
            "CUBERANKEDMEMBER(\"c\",A1,1)",
            "CUBEMEMBERPROPERTY(\"c\",\"m\",\"p\")",
            "CUBEKPIMEMBER(\"c\",\"k\",1)",
        ] {
            assert!(refused_formula(f).is_some(), "{f}");
        }
        for f in [
            "SUM(A1:A3)",
            "\"[x]|y\"&A1",
            "'My sheet'!A1*2",
            "'It''s'!A1",
            "IF(A1>0,\"WEBSERVICE(\",0)",
            "\"say \"\"WEBSERVICE\"\"\"",
            "SUM(Table1[Amount])",
            "Table1[[#This Row],[Amount]]*2",
            "[@Amount]+[@[Unit Price]]",
            "SUM(Sales[[#Headers],[Q1]]:Sales[[#Totals],[Q4]])",
            "COUNTA(Table1[#Data])",
            "SUM(PY:PY)+SUM(RTD:RTD)+SUM($DDE$1:$DDE$9)",
            "XLOOKUP(1,A:A,B:B)",
        ] {
            assert_eq!(refused_formula(f), None, "{f}");
        }
        assert!(
            refused_formula("_xlws._xlfn.WEBSERVICE(A1)")
                .unwrap()
                .starts_with("WEBSERVICE")
        );
    }
}
