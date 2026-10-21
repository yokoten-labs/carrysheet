//! What the default report says about the texts it leaves out, and the one
//! exception to leaving them out: SQL command text.
//!
//! SQL is shown by default because the query is often the most useful thing
//! to know about a connection. It is shown whole or not at all, and only
//! when it is a plain read: one `SELECT` or `WITH` query with no comments
//! (`--`, `/* */`, a MySQL `#`), no procedure calls (`EXEC`, `CALL`, `sp_`,
//! `xp_`), no writes, no other connections (`OPENROWSET`), no string
//! concatenation except with spaces or punctuation, no backslash in a quoted
//! value, and no dollar- or q-quoted text. Everything else is hidden whole,
//! with the reason.
//!
//! Credentials are not matched by shape. A statement is hidden when any
//! word or name in it names a secret (`password`, `pwd`, `api_key`, `pin`,
//! ...) and it holds any literal value anywhere (a quoted string of any
//! kind, or a number next to the secret name), or when a literal itself
//! names a secret or looks like one. That hides a little more than needed,
//! such as `LastPasswordChange` next to a date, which is the harmless
//! direction.
//!
//! This is an allow-list on purpose. A check for credential shapes is a
//! deny-list, and every review found a shape it missed. A partial mask would
//! show the text around a secret the patterns did not recognize; hiding the
//! whole command turns every miss into a harmless false positive instead.

use super::safe;
use crate::mask::{is_secret_key, is_secret_word, mask_secrets};
use crate::report::Hidden;

/// The option that shows hidden texts, by its name in the API.
pub(crate) const FLAG: &str = "Options::include_raw";

fn label(what: &str) -> String {
    let mut label = what.to_string();
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    label
}

/// Describes a text left out of the report.
pub(crate) fn hidden(what: &str, text: &str, why: &str) -> Hidden {
    let characters = text.chars().count();
    let label = label(what);
    Hidden {
        what: what.to_string(),
        characters,
        note: format!(
            "{label}: hidden ({characters} characters) {why}. To see it: set {FLAG}; the text is then masked as well as patterns can, and may still contain secrets."
        ),
    }
}

/// Describes a text left out because it is longer than its limit. Such a
/// text is never cut, not even with [`FLAG`]: a cut can end inside a secret
/// and leave the start of it where masking no longer recognizes it.
pub(crate) fn too_long(what: &str, characters: usize, field: &str, max: usize) -> Hidden {
    let label = label(what);
    Hidden {
        what: what.to_string(),
        characters,
        note: format!(
            "{label}: hidden ({characters} characters) because it is longer than {field} ({max}), and a text is never cut, so no part of a secret can show. To see it: raise {field} and set {FLAG}; the text is then masked as well as patterns can, and may still contain secrets."
        ),
    }
}

/// True when `text` has more than `max` characters.
pub(crate) fn longer_than(text: &str, max: usize) -> bool {
    text.chars().nth(max).is_some()
}

const NOT_PLAIN: &str = "because it is not a plain SELECT or WITH query";
const COMMENT: &str = "because it has comments, which can hold anything";
const PROCEDURE: &str = "because it runs a procedure (EXEC, CALL, sp_ or xp_)";
const WRITES: &str = "because it does more than read data";
const OPENS: &str = "because it opens another connection, which takes a connection string";
const CONCAT: &str = "because it joins strings together, which can build a hidden value";
const MANY: &str = "because it holds more than one statement";
const UNCLOSED: &str = "because a quoted part of it is not closed";
const DOLLAR: &str = "because it holds dollar-quoted or q-quoted text";
const BACKSLASH: &str =
    "because a quoted value in it holds a backslash, which some databases read as an escape";
const SECRET: &str = "because it may contain a password or other secret";
const SECRET_COLUMN: &str = "because it mentions a password column and holds a literal value";
const SECRET_IN_VALUE: &str = "because a quoted value in it mentions a password or key";

#[derive(Debug, PartialEq)]
enum Tok<'a> {
    Word(&'a str),
    /// A string: `'...'`, `"..."` (a string in Access and MySQL), or an Access
    /// date `#...#`, as written between the quotes.
    Literal(&'a str),
    /// A `[...]` or `` `...` `` name.
    Quoted(&'a str),
    Number,
    Op(&'a str),
    Semicolon,
}

fn word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'@' | b'$') || b >= 0x80
}

/// The end of a quoted part that starts at `from` (after its opening quote)
/// and closes with `close`, where a doubled `close` stands for one.
fn quoted_end(bytes: &[u8], from: usize, close: u8) -> Option<usize> {
    let mut k = from;
    while k < bytes.len() {
        if bytes[k] == close {
            if bytes.get(k + 1) == Some(&close) {
                k += 2;
                continue;
            }
            return Some(k);
        }
        k += 1;
    }
    None
}

/// True when a `#` at this point can start a T-SQL temporary table name:
/// right after `FROM`, `JOIN` or `INTO`, or after `,` or `.`.
fn table_name_may_follow(out: &[Tok<'_>]) -> bool {
    match out.last() {
        Some(Tok::Word(w)) => ["from", "join", "into"].contains(&w.to_ascii_lowercase().as_str()),
        Some(Tok::Op(op)) => matches!(*op, "," | "."),
        _ => false,
    }
}

/// The end of an Access date literal `#...#` that starts at `from` (after
/// its `#`), when one is there.
fn date_end(text: &str, from: usize) -> Option<usize> {
    let end = from + text[from..].find('#')?;
    let inner = &text[from..end];
    (!inner.is_empty()
        && inner.len() <= 32
        && inner.bytes().any(|b| b.is_ascii_digit())
        && inner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" /-:.".contains(&b)))
    .then_some(end)
}

fn sql_tokens(text: &str) -> Result<Vec<Tok<'_>>, &'static str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();
        match b {
            _ if b.is_ascii_whitespace() => i += 1,
            b'-' if next == Some(b'-') => return Err(COMMENT),
            b'/' if next == Some(b'*') => return Err(COMMENT),
            b'#' => {
                // `#temp` where a table can stand, `#2024-01-31#` (Access),
                // and otherwise a MySQL comment to the end of the line.
                if table_name_may_follow(&out) {
                    let start = i;
                    i += 1;
                    while i < bytes.len() && (word_byte(bytes[i]) || bytes[i] == b'#') {
                        i += 1;
                    }
                    if i - start == 1 {
                        return Err(COMMENT);
                    }
                    out.push(Tok::Word(&text[start..i]));
                } else if let Some(end) = date_end(text, i + 1) {
                    out.push(Tok::Literal(&text[i + 1..end]));
                    i = end + 1;
                } else {
                    return Err(COMMENT);
                }
            }
            b'$' if next == Some(b'$')
                || next.is_some_and(|n| n.is_ascii_alphabetic())
                    && text[i + 1..]
                        .find('$')
                        .is_some_and(|e| text[i + 1..i + 1 + e].bytes().all(word_byte)) =>
            {
                return Err(DOLLAR);
            }
            b'\'' | b'"' => {
                let end = quoted_end(bytes, i + 1, b).ok_or(UNCLOSED)?;
                out.push(Tok::Literal(&text[i + 1..end]));
                i = end + 1;
            }
            b'[' | b'`' => {
                let close = if b == b'[' { b']' } else { b'`' };
                let end = quoted_end(bytes, i + 1, close).ok_or(UNCLOSED)?;
                let inner = &text[i + 1..end];
                if b == b'[' && inner.contains(['\'', '"']) {
                    // `ARRAY['x']` or `a['k']`: a subscript, not a name, so
                    // what is inside is read as it stands.
                    out.push(Tok::Op("["));
                    i += 1;
                    continue;
                }
                out.push(Tok::Quoted(inner));
                i = end + 1;
            }
            b'0'..=b'9' => {
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                    i += 1;
                }
                out.push(Tok::Number);
            }
            _ if word_byte(b) => {
                let start = i;
                while i < bytes.len() && word_byte(bytes[i]) {
                    i += 1;
                }
                let word = &text[start..i];
                // Oracle's q'[...]' picks its own closing character, so its
                // end cannot be found the usual way.
                if bytes.get(i) == Some(&b'\'')
                    && (word.eq_ignore_ascii_case("q") || word.eq_ignore_ascii_case("nq"))
                {
                    return Err(DOLLAR);
                }
                out.push(Tok::Word(word));
            }
            b';' => {
                out.push(Tok::Semicolon);
                i += 1;
            }
            b'|' if next == Some(b'|') => {
                out.push(Tok::Op("||"));
                i += 2;
            }
            _ => {
                let len = text[i..].chars().next().map_or(1, char::len_utf8);
                out.push(Tok::Op(&text[i..i + len]));
                i += len;
            }
        }
    }
    Ok(out)
}

/// Words that make a query do more than read.
const WRITE_WORDS: &[&str] = &[
    "insert", "update", "delete", "merge", "drop", "create", "alter", "grant", "revoke", "declare",
    "set", "truncate", "use", "backup", "restore", "shutdown", "kill", "deny", "load", "copy",
    "replace", "upsert", "attach", "detach", "pragma",
];

/// True when a word or name mentions a secret, as a whole or in its
/// dotted parts (`dbo.vw_PasswordResets`).
fn names_secret(name: &str) -> bool {
    name.split('.').any(is_secret_word)
}

/// True when a quoted value mentions a secret word.
fn value_names_secret(value: &str) -> bool {
    value
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| !w.is_empty() && w.len() <= 64 && is_secret_word(w))
}

/// `Ok` when SQL may be shown whole; otherwise why it is hidden whole.
///
/// Credentials are not matched by shape: a statement that names a secret
/// column anywhere and holds any literal value anywhere is hidden, whatever
/// the operators, functions or order between them, and so is one with a
/// literal that names a secret itself.
pub(crate) fn sql_verdict(text: &str) -> Result<(), &'static str> {
    let tokens = sql_tokens(text)?;
    let mut secret_name = false;
    let mut literal = false;
    for (i, tok) in tokens.iter().enumerate() {
        match tok {
            Tok::Semicolon if i + 1 < tokens.len() => return Err(MANY),
            Tok::Word(w) => {
                let lower = w.to_ascii_lowercase();
                match lower.as_str() {
                    "exec" | "execute" | "call" => return Err(PROCEDURE),
                    _ if lower.starts_with("sp_") || lower.starts_with("xp_") => {
                        return Err(PROCEDURE);
                    }
                    "openrowset" | "opendatasource" | "openquery" | "dblink" => return Err(OPENS),
                    "concat" | "concat_ws" => return Err(CONCAT),
                    _ if WRITE_WORDS.contains(&lower.as_str()) => return Err(WRITES),
                    _ => {}
                }
                secret_name |= names_secret(w);
            }
            Tok::Quoted(name) => secret_name |= names_secret(name),
            Tok::Op("||") => return Err(CONCAT),
            Tok::Op("+") => {
                // `First + ' ' + Last` only joins with spaces or punctuation.
                let joins_text = |t: Option<&Tok<'_>>| matches!(t, Some(Tok::Literal(v)) if v.chars().any(char::is_alphanumeric));
                if joins_text(i.checked_sub(1).and_then(|p| tokens.get(p)))
                    || joins_text(tokens.get(i + 1))
                {
                    return Err(CONCAT);
                }
            }
            Tok::Literal(value) => {
                literal = true;
                if value.contains('\\') {
                    return Err(BACKSLASH);
                }
                if value_names_secret(value) {
                    return Err(SECRET_IN_VALUE);
                }
                if value_may_be_secret(value) {
                    return Err(SECRET);
                }
            }
            Tok::Number => {
                // A number counts as a literal next to a secret name:
                // `pin = 1234`.
                let near = |t: Option<&Tok<'_>>| matches!(t, Some(Tok::Word(w) | Tok::Quoted(w)) if names_secret(w));
                let before = tokens[..i].iter().rev().find(|t| !matches!(t, Tok::Op(_)));
                let after = tokens[i + 1..].iter().find(|t| !matches!(t, Tok::Op(_)));
                literal |= near(before) || near(after);
            }
            _ => {}
        }
    }
    if secret_name && literal {
        return Err(SECRET_COLUMN);
    }
    // The specific reasons above say more, so this comes last.
    match tokens.iter().find(|t| **t != Tok::Op("(")) {
        Some(Tok::Word(w))
            if w.eq_ignore_ascii_case("select") || w.eq_ignore_ascii_case("with") =>
        {
            Ok(())
        }
        _ => Err(NOT_PLAIN),
    }
}

/// True when a table or cube name may be shown: it names no secret and
/// does not look like a token.
pub(crate) fn plain_object_name(name: &str) -> Result<(), &'static str> {
    if safe::looks_like_token(name) {
        return Err("because it looks like an access token");
    }
    let names = name
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| !w.is_empty() && is_secret_word(w));
    if names {
        return Err("because its name mentions a password or key, so it is treated like one");
    }
    Ok(())
}

/// True when a quoted value in SQL may be a credential: a URL with user
/// information, a credential path or a secret parameter; a `key=value` with
/// a secret key; or a random-looking token.
fn value_may_be_secret(value: &str) -> bool {
    if value.contains("://") {
        return url_may_hold_secret(value.trim());
    }
    safe::looks_like_token(value) || mask_secrets(value) != value
}

fn url_may_hold_secret(value: &str) -> bool {
    let Some(url) = safe::url(value) else {
        return true;
    };
    if url.withheld.is_some() || safe::looks_like_token(&url.path) {
        return true;
    }
    let after_scheme = value.split_once("://").map_or("", |(_, r)| r);
    let authority = &after_scheme[..after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len())];
    if authority.contains('@') {
        return true;
    }
    let tail = &after_scheme[after_scheme.find(['?', '#']).unwrap_or(after_scheme.len())..];
    tail.split(['?', '#', '&', ';'])
        .filter(|p| !p.is_empty())
        .any(|pair| {
            let (key, val) = pair.split_once('=').unwrap_or((pair, ""));
            is_secret_key(key) || safe::looks_like_token(val)
        })
}

#[cfg(test)]
mod tests {
    use super::{hidden, sql_verdict, too_long};

    #[test]
    fn only_plain_reads_are_shown() {
        for sql in [
            "SELECT region, SUM(amount) FROM sales WHERE year = 2024 GROUP BY region",
            "SELECT * FROM \"Sales\".\"dbo\".\"Orders\"",
            "select CustomerKey, ProductKey, DateKey from FactSales where DateKey > 20240101",
            "WITH t AS (SELECT 1 AS a) SELECT a FROM t;",
            "SELECT 'https://www.example.com/products/list' AS link, name FROM shops",
            "SELECT * FROM [Sales] WHERE note <> 'n/a' AND region IN ('EU', 'US')",
            "(SELECT a FROM t) UNION ALL (SELECT b FROM u)",
            "SELECT * FROM #temp",
        ] {
            assert_eq!(sql_verdict(sql), Ok(()), "{sql}");
        }
        for sql in [
            "ALTER USER bob IDENTIFIED BY x1",
            "SELECT * FROM t WHERE pwd = 'x'",
            "SELECT * FROM t WHERE api_key IN ('a', 'b')",
            "SELECT * FROM t WHERE \"Password\" LIKE N'x%'",
            "SELECT * FROM t WHERE pin = 1234 AND password = 5678",
            "EXEC sp_password NULL, 'new', 'bob'",
            "SELECT 1; EXEC xp_cmdshell 'dir'",
            "SELECT 1 -- note",
            "SELECT 1 /* note */",
            "SELECT 1 # note",
            "SELECT 'https://bob:x@host/' AS a",
            "SELECT 'https://host/x?sig=abc' AS a",
            "SELECT 'https://hooks.slack.com/services/T1/B2/abc' AS a",
            "SELECT 'aB3dE5gH7jK9mN1pQ3sT5vW7'",
            "SELECT 'Server=s;PWD=hunter2'",
            "SELECT 'a' || 'b'",
            "SELECT 'a' + col FROM t",
            "SELECT CONCAT('a', col) FROM t",
            "SELECT * FROM OPENROWSET('SQLNCLI', 'x', 'y')",
            "SELECT 'unclosed",
            "SELECT $$x$$",
            "INSERT INTO t VALUES (1)",
            "WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d",
            "UPDATE t SET a = 1",
        ] {
            assert!(sql_verdict(sql).is_err(), "{sql}");
        }
    }

    /// Review round 4: password comparisons in common forms. Each is hidden
    /// whole, whatever the shape between the column and the value.
    pub(crate) const PASSWORD_COMPARISONS: &[&str] = &[
        "SELECT * FROM users WHERE [Password]=\"x\"",
        "SELECT * FROM users WHERE 'x' = password",
        "SELECT * FROM users WHERE md5('x') = password",
        "SELECT * FROM users WHERE password = LOWER('x')",
        "SELECT * FROM users WHERE CAST(password AS varchar(20)) = 'x'",
        "SELECT * FROM users WHERE password = 'x' COLLATE Latin1_General_CS_AS",
        "SELECT * FROM users WHERE password BETWEEN 'a' AND 'x'",
        "SELECT * FROM users WHERE password ILIKE 'x%'",
        "SELECT * FROM users WHERE password SIMILAR TO 'x'",
        "SELECT * FROM users WHERE password IS NOT DISTINCT FROM 'x'",
        "SELECT * FROM users WHERE password = ANY(ARRAY['x'])",
        "SELECT * FROM users WHERE password = (ARRAY['a', 'x'])[2]",
        "SELECT * FROM users WHERE password IN (SELECT 'x')",
        "SELECT * FROM users WHERE password = _utf8mb4'x'",
        "SELECT * FROM users WHERE password = q'[x]'",
        "SELECT * FROM users WHERE password = U&'x'",
        "SELECT 'x' AS pwd",
        "SELECT * FROM t WHERE x = 'x' AND password = x",
        "SELECT * FROM t WHERE c = 'password x'",
        "SELECT * FROM t WHERE pin = 1234",
        "SELECT * FROM t WHERE passcode = 'x'",
        "SELECT LastPasswordChange FROM t WHERE d > '2024-01-01'",
    ];

    #[test]
    fn password_comparisons_are_hidden_in_every_form() {
        for sql in PASSWORD_COMPARISONS {
            assert!(sql_verdict(sql).is_err(), "{sql}");
        }
        assert_eq!(
            sql_verdict("SELECT * FROM users WHERE 'x' = password"),
            Err("because it mentions a password column and holds a literal value")
        );
    }

    #[test]
    fn hash_and_backslash() {
        // A MySQL comment, not a temporary table.
        assert_eq!(
            sql_verdict("SELECT a FROM t #password=x"),
            Err("because it has comments, which can hold anything")
        );
        for sql in [
            "SELECT * FROM #temp",
            "SELECT * FROM a JOIN #b ON a.id = b.id",
            "SELECT * FROM a, ##global",
            "SELECT * FROM t WHERE d > #2024-01-31#",
            "SELECT * FROM t WHERE d BETWEEN #1/1/2024# AND #12/31/2024 11:59 PM#",
        ] {
            assert_eq!(sql_verdict(sql), Ok(()), "{sql}");
        }
        assert_eq!(
            sql_verdict(r"SELECT * FROM t WHERE a = 'x\'' AND pwd = 'y' AND b = 'z\''"),
            Err(
                "because a quoted value in it holds a backslash, which some databases read as an escape"
            )
        );
        assert!(sql_verdict(r"SELECT 'C:\data' AS p").is_err());
    }

    #[test]
    fn plain_columns_and_joins_are_not_over_hidden() {
        for sql in [
            "SELECT FirstName + ' ' + LastName AS FullName FROM people",
            "SELECT City + ', ' + Country FROM places",
            "SELECT * FROM t WHERE code = 'A1'",
            "SELECT * FROM t WHERE key = 5",
            "SELECT * FROM orders o WHERE o.SessionID = 42",
            "SELECT Spinner, Shipping FROM t WHERE a = 'x'",
            "SELECT CustomerKey FROM t WHERE region = 'EU'",
        ] {
            assert_eq!(sql_verdict(sql), Ok(()), "{sql}");
        }
        assert!(sql_verdict("SELECT FirstName + 'x' FROM people").is_err());
    }

    #[test]
    fn notes_say_size_and_flag() {
        let h = hidden("SQL command", "abc", "because it may contain a password");
        assert_eq!(h.characters, 3);
        assert!(h.note.starts_with(
            "SQL command: hidden (3 characters) because it may contain a password. To see it: set Options::include_raw"
        ));
        let h = too_long("M formula", 20, "Limits::max_formula_chars", 10);
        assert!(
            h.note
                .contains("longer than Limits::max_formula_chars (10)"),
            "{}",
            h.note
        );
        assert!(
            h.note
                .contains("raise Limits::max_formula_chars and set Options::include_raw")
        );
    }
}
