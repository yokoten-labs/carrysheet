//! The functions a Power Query formula calls, by an allow-list: only names
//! in the M standard library's namespaces are listed, and only the literal
//! arguments that name a server, database, URL or path are read, through the
//! same field parsers as connection strings. String literals, comments and
//! quoted identifiers are skipped as whole tokens, so nothing inside them
//! can be taken for a function name.

use crate::parts::safe;
use crate::parts::target;
use crate::report::{Call, Target};

/// Namespaces of the M standard library (Power Query M function reference).
const NAMESPACES: &[&str] = &[
    "Access",
    "ActiveDirectory",
    "AdoDotNet",
    "AmazonRedshift",
    "AnalysisServices",
    "AzureStorage",
    "Binary",
    "BinaryFormat",
    "Byte",
    "Character",
    "Combiner",
    "Comparer",
    "Csv",
    "Cube",
    "Currency",
    "DB2",
    "Date",
    "DateTime",
    "DateTimeZone",
    "Decimal",
    "Diagnostics",
    "Double",
    "Duration",
    "Embedded",
    "Error",
    "Excel",
    "Exchange",
    "Expression",
    "Extension",
    "File",
    "Folder",
    "Function",
    "Geography",
    "Geometry",
    "Hdfs",
    "HdInsight",
    "Html",
    "Informix",
    "Int16",
    "Int32",
    "Int64",
    "Int8",
    "Json",
    "Lines",
    "List",
    "Logical",
    "MySQL",
    "Number",
    "OData",
    "Odbc",
    "OleDb",
    "Oracle",
    "Pdf",
    "Percentage",
    "PostgreSQL",
    "RData",
    "Record",
    "Replacer",
    "Salesforce",
    "SapBusinessWarehouse",
    "SapHana",
    "SharePoint",
    "Single",
    "Snowflake",
    "Splitter",
    "Sql",
    "Sybase",
    "Table",
    "Teradata",
    "Text",
    "Time",
    "Type",
    "Uri",
    "Value",
    "Variable",
    "Web",
    "Xml",
];

/// M keywords that a `(` may follow without a call.
const KEYWORDS: &[&str] = &[
    "and",
    "as",
    "each",
    "else",
    "error",
    "if",
    "in",
    "is",
    "let",
    "meta",
    "not",
    "or",
    "otherwise",
    "section",
    "shared",
    "then",
    "try",
    "type",
];

/// What the literal arguments of a function name.
#[derive(Clone, Copy)]
enum Arguments {
    /// The first is a URL.
    Url,
    /// The first is a file or folder path (or URL).
    Path,
    /// A server, then a database.
    ServerDatabase,
    /// The first is a connection string.
    ConnectionString,
    None,
}

fn arguments(function: &str) -> Arguments {
    match function {
        "Web.Contents"
        | "Web.Page"
        | "Web.BrowserContents"
        | "OData.Feed"
        | "SharePoint.Files"
        | "SharePoint.Contents"
        | "SharePoint.Tables"
        | "AzureStorage.Blobs"
        | "AzureStorage.Tables"
        | "AzureStorage.DataLake" => Arguments::Url,
        "File.Contents" | "Folder.Files" | "Folder.Contents" | "Hdfs.Files" | "Hdfs.Contents" => {
            Arguments::Path
        }
        "Sql.Database"
        | "Sql.Databases"
        | "PostgreSQL.Database"
        | "MySQL.Database"
        | "Oracle.Database"
        | "AnalysisServices.Database"
        | "AnalysisServices.Databases"
        | "Teradata.Database"
        | "DB2.Database"
        | "Sybase.Database"
        | "Informix.Database"
        | "Snowflake.Databases"
        | "AmazonRedshift.Database"
        | "SapHana.Database" => Arguments::ServerDatabase,
        "Odbc.DataSource" | "Odbc.Query" | "OleDb.DataSource" | "OleDb.Query" => {
            Arguments::ConnectionString
        }
        _ => Arguments::None,
    }
}

#[derive(Debug, PartialEq)]
enum Token {
    Identifier(String),
    /// A string literal, unescaped.
    Literal(String),
    Punct(u8),
    /// Anything else: numbers, quoted identifiers, operators.
    Other,
}

fn tokens(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            _ if b.is_ascii_whitespace() => i += 1,
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(bytes.len(), |p| i + p);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |p| i + 2 + p + 2);
            }
            b'"' | b'#' if b == b'"' || bytes.get(i + 1) == Some(&b'"') => {
                let start = if b == b'"' { i + 1 } else { i + 2 };
                let (end, value) = literal(text, start);
                out.push(if b == b'"' {
                    Token::Literal(value)
                } else {
                    Token::Other
                });
                i = end;
            }
            _ if b.is_ascii_alphabetic() || b == b'_' || b == b'#' => {
                let start = i;
                i += 1;
                while i < bytes.len()
                    && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b'.'))
                {
                    i += 1;
                }
                out.push(Token::Identifier(
                    text[start..i].trim_end_matches('.').to_string(),
                ));
            }
            b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' => {
                out.push(Token::Punct(b));
                i += 1;
            }
            _ => {
                out.push(Token::Other);
                // Skip a whole multi-byte character.
                i += text[i..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    out
}

/// The end of a string literal whose content starts at `from` (after its
/// closing quote), and its unescaped content.
fn literal(text: &str, from: usize) -> (usize, String) {
    let bytes = text.as_bytes();
    let mut value = String::new();
    let mut k = from;
    let mut piece = from;
    while k < bytes.len() {
        if bytes[k] == b'"' {
            value.push_str(&text[piece..k]);
            if bytes.get(k + 1) == Some(&b'"') {
                value.push('"');
                k += 2;
                piece = k;
                continue;
            }
            return (k + 1, value);
        }
        k += 1;
    }
    value.push_str(&text[piece..]);
    (bytes.len(), value)
}

/// The library calls in `formula`, and how many other calls there were.
pub(crate) fn calls(formula: &str, max_calls: usize) -> (Vec<Call>, u32) {
    let tokens = tokens(formula);
    let mut out: Vec<Call> = Vec::new();
    let mut other = 0u32;
    for (i, token) in tokens.iter().enumerate() {
        let Token::Identifier(name) = token else {
            continue;
        };
        if tokens.get(i + 1) != Some(&Token::Punct(b'(')) || KEYWORDS.contains(&name.as_str()) {
            continue;
        }
        let library = name.len() <= 64
            && name
                .split_once('.')
                .is_some_and(|(ns, rest)| NAMESPACES.contains(&ns) && !rest.is_empty());
        if !library {
            other = other.saturating_add(1);
            continue;
        }
        let literals = literal_arguments(&tokens[i + 2..]);
        let target = target_of(name, &literals);
        let call = Call {
            function: name.clone(),
            target,
        };
        if out.len() < max_calls && !out.contains(&call) {
            out.push(call);
        }
    }
    (out, other)
}

/// The arguments of a call whose tokens start after its `(`: each one's
/// value when it is a single string literal.
fn single<'t>(arg: &[&'t Token]) -> Option<&'t str> {
    match arg {
        [Token::Literal(value)] => Some(value.as_str()),
        _ => None,
    }
}

fn literal_arguments(tokens: &[Token]) -> Vec<Option<&str>> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current: Vec<&Token> = Vec::new();
    for token in tokens {
        match token {
            Token::Punct(b'(' | b'[' | b'{') => depth += 1,
            Token::Punct(b')' | b']' | b'}') if depth == 0 => {
                out.push(single(&current));
                return out;
            }
            Token::Punct(b')' | b']' | b'}') => depth -= 1,
            Token::Punct(b',') if depth == 0 => {
                out.push(single(&current));
                current.clear();
                if out.len() >= 4 {
                    return out;
                }
                continue;
            }
            _ => {}
        }
        current.push(token);
    }
    out
}

fn target_of(function: &str, args: &[Option<&str>]) -> Target {
    let mut t = Target::default();
    let first = args.first().copied().flatten();
    match (arguments(function), first) {
        (Arguments::Url, Some(value)) => {
            safe::set_url(&mut t, value);
        }
        (Arguments::Path, Some(value)) => {
            safe::set_location(&mut t, value);
        }
        (Arguments::ServerDatabase, Some(value)) => {
            if value.contains("://") {
                safe::set_url(&mut t, value);
            } else if let Some(server) = safe::server(value) {
                t.server = Some(server.host);
                t.port = server.port;
            } else if let Some((server, service)) = safe::oracle(value) {
                t.server = Some(server.host);
                t.port = server.port;
                t.database = service;
            }
            if let Some(database) = args.get(1).copied().flatten().and_then(safe::name) {
                t.database = Some(database);
            }
        }
        (Arguments::ConnectionString, Some(value)) => t = target::parse(value).target,
        _ => {}
    }
    t
}

#[cfg(test)]
mod tests {
    use super::calls;

    #[test]
    fn lists_library_calls_with_hosts() {
        let formula = r#"let
    // Web.Contents("https://comment.example")
    Source = Sql.Database("tcp:sql01,1433", "Sales", [Query="select ""Text.Foo(""x"")"" "]),
    Web = Json.Document(Web.Contents("https://bob:pw@api.example.com/v1?key=abc", [Headers=[Authorization="Bearer x"]])),
    Odbc = Odbc.Query("dsn=Warehouse;pwd=secret", "select 1"),
    Mine = fnHelper(Source),
    #"Changed Type" = Table.TransformColumnTypes(Source, {})
in
    #"Changed Type""#;
        let (found, other) = calls(formula, 100);
        let names: Vec<&str> = found.iter().map(|c| c.function.as_str()).collect();
        assert_eq!(
            names,
            [
                "Sql.Database",
                "Json.Document",
                "Web.Contents",
                "Odbc.Query",
                "Table.TransformColumnTypes"
            ]
        );
        assert_eq!(other, 1);
        assert_eq!(found[0].target.server.as_deref(), Some("sql01"));
        assert_eq!(found[0].target.port, Some(1433));
        assert_eq!(found[0].target.database.as_deref(), Some("Sales"));
        assert_eq!(
            found[2].target.url.as_deref(),
            Some("https://api.example.com/v1")
        );
        assert_eq!(
            found[3].target.data_source_name.as_deref(),
            Some("Warehouse")
        );
        let json = format!("{found:?}");
        assert!(!json.contains("secret") && !json.contains("pw@") && !json.contains("abc"));
    }

    #[test]
    fn oracle_user_and_password_is_not_an_address() {
        let (found, _) = calls(
            r#"Oracle.Database("scott/tiger") & Oracle.Database("ora05:1521/fin")"#,
            10,
        );
        assert!(found[0].target.is_empty(), "{:?}", found[0].target);
        assert_eq!(found[1].target.server.as_deref(), Some("ora05"));
        assert_eq!(found[1].target.database.as_deref(), Some("fin"));
    }
}
