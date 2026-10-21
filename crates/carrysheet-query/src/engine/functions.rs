//! Our versions of SQLite functions that are slow on large values, with
//! SQLite's results.
//!
//! The progress handler runs only between virtual-machine steps, so one call
//! of a slow function cannot be stopped. SQLite's `trim(X, Y)`, `ltrim(X, Y)`,
//! `rtrim(X, Y)` compare every character of X with every character of Y, and
//! `instr` and `replace` compare the pattern at every position: with large
//! values that is hours. These versions are linear (Knuth-Morris-Pratt search,
//! a hash set of trim characters). LIKE and GLOB ([`super::pattern`]) keep
//! SQLite's algorithm. All of them read the watchdog flag every few KB and
//! stop with an interrupt once the time limit has passed.

use super::guard::Late;
use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use super::pattern;

use rusqlite::functions::{Context, FunctionFlags};
use rusqlite::types::{ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, Result};

/// Bytes of work between two reads of the watchdog flag.
const CHECK_EVERY: usize = 4096;

/// Registers the replacements on `conn`. `max_bytes` is SQLite's length
/// limit: a longer `replace` result fails as "string or blob too big".
/// `max_pattern` is the longest LIKE or GLOB pattern. Once `stop` is set,
/// a running call ends with an interrupt.
pub(crate) fn register(
    conn: &Connection,
    max_bytes: usize,
    max_pattern: usize,
    stop: &Arc<Late>,
) -> Result<()> {
    let flags = FunctionFlags::SQLITE_UTF8
        | FunctionFlags::SQLITE_DETERMINISTIC
        | FunctionFlags::SQLITE_INNOCUOUS;
    for (name, left, right) in [
        ("trim", true, true),
        ("ltrim", true, false),
        ("rtrim", false, true),
    ] {
        let stop = Arc::clone(stop);
        conn.create_scalar_function(name, 2, flags, move |ctx| trim(ctx, left, right, &stop))?;
    }
    let instr_stop = Arc::clone(stop);
    conn.create_scalar_function("instr", 2, flags, move |ctx| instr(ctx, &instr_stop))?;
    let replace_stop = Arc::clone(stop);
    conn.create_scalar_function("replace", 3, flags, move |ctx| {
        replace(ctx, max_bytes, &replace_stop)
    })?;
    for (name, args, info) in [
        ("like", 2, pattern::LIKE),
        ("like", 3, pattern::LIKE),
        ("glob", 2, pattern::GLOB),
    ] {
        let stop = Arc::clone(stop);
        conn.create_scalar_function(name, args, flags, move |ctx| {
            like(ctx, info, max_pattern, &stop)
        })?;
    }
    Ok(())
}

/// The error a call returns once the time limit has passed. SQLite stops the
/// statement with it, as if the progress handler had.
fn interrupted() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_INTERRUPT),
        None,
    )
}

fn check(stop: &Late) -> Result<()> {
    if stop.passed() {
        Err(interrupted())
    } else {
        Ok(())
    }
}

fn sql_error(message: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(message.to_owned()),
    )
}

/// `like(P, S)`, `like(P, S, E)` and `glob(P, S)`, as SQLite's `likeFunc`.
fn like(ctx: &Context<'_>, info: pattern::Info, max_pattern: usize, stop: &Late) -> Result<Out> {
    check(stop)?;
    let pat = text_arg(ctx, 0)?;
    if pat.as_ref().map_or(0, Vec::len) > max_pattern {
        return Err(sql_error("LIKE or GLOB pattern too complex"));
    }
    let escape = if ctx.len() == 3 {
        let Some(esc) = text_arg(ctx, 2)? else {
            return Ok(Out::Null);
        };
        let esc = pattern::c_text(&esc);
        if chars(esc).count() != 1 {
            return Err(sql_error("ESCAPE expression must be a single character"));
        }
        pattern::read_utf8(esc, &mut 0)
    } else if info.is_glob() {
        u32::from(b'[')
    } else {
        0
    };
    let (Some(pat), Some(text)) = (pat, text_arg(ctx, 1)?) else {
        return Ok(Out::Null);
    };
    match pattern::compare(&pat, &text, info, escape, stop) {
        Ok(found) => Ok(Out::Int(i64::from(found))),
        Err(pattern::Stopped) => Err(interrupted()),
    }
}

/// An argument as SQLite's text conversion gives it: `None` for NULL.
fn text_arg(ctx: &Context<'_>, i: usize) -> Result<Option<Vec<u8>>> {
    Ok(match ctx.get_raw(i) {
        ValueRef::Null => None,
        ValueRef::Integer(n) => Some(n.to_string().into_bytes()),
        ValueRef::Real(f) => Some(real_to_text(f)?.into_bytes()),
        ValueRef::Text(t) | ValueRef::Blob(t) => Some(t.to_vec()),
    })
}

thread_local! {
    /// A plain connection that turns real numbers into text exactly the way
    /// SQLite does (its own `%!.17g`), so results match the built-ins.
    static FORMAT: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

fn real_to_text(f: f64) -> Result<String> {
    FORMAT.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
            *slot = Some(Connection::open_in_memory()?);
        }
        match slot.as_ref() {
            Some(conn) => conn.query_row("SELECT CAST(?1 AS TEXT)", [f], |r| r.get(0)),
            None => Ok(f.to_string()),
        }
    })
}

/// A function result. Text is kept as raw bytes, as SQLite does: a blob
/// turned into text keeps its bytes even when they are not valid UTF-8.
#[derive(Debug, PartialEq)]
enum Out {
    Null,
    Int(i64),
    Text(Vec<u8>),
}

impl ToSql for Out {
    fn to_sql(&self) -> Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(match self {
            Out::Null => ValueRef::Null,
            Out::Int(i) => ValueRef::Integer(*i),
            Out::Text(t) => ValueRef::Text(t),
        }))
    }
}

/// True for a UTF-8 continuation byte.
fn is_continuation(b: u8) -> bool {
    b & 0xC0 == 0x80
}

/// Splits bytes into characters the way SQLite does (`SQLITE_SKIP_UTF8`): a
/// byte of 0xC0 or more takes the continuation bytes after it; any other
/// byte is a character on its own.
fn chars(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let len = if rest[0] >= 0xC0 {
            1 + rest[1..]
                .iter()
                .take_while(|&&b| is_continuation(b))
                .count()
        } else {
            1
        };
        let (head, tail) = rest.split_at(len);
        rest = tail;
        Some(head)
    })
}

/// `trim`, `ltrim`, `rtrim` with a set of characters.
fn trim(ctx: &Context<'_>, left: bool, right: bool, stop: &Late) -> Result<Out> {
    check(stop)?;
    let (Some(input), Some(set)) = (text_arg(ctx, 0)?, text_arg(ctx, 1)?) else {
        return Ok(Out::Null);
    };
    // SQLite reads the set as C text: it ends at the first NUL byte.
    let set = match set.iter().position(|&b| b == 0) {
        Some(end) => &set[..end],
        None => &set[..],
    };
    let set_chars: HashSet<&[u8]> = chars(set).collect();
    let mut lengths: Vec<usize> = set_chars.iter().map(|c| c.len()).collect();
    lengths.sort_unstable();
    lengths.dedup();
    let (mut start, mut end) = (0, input.len());
    let mut steps = 0usize;
    if left {
        while let Some(len) = lengths
            .iter()
            .copied()
            .find(|&len| len <= end - start && set_chars.contains(&input[start..start + len]))
        {
            start += len;
            steps += 1;
            if steps.is_multiple_of(CHECK_EVERY) {
                check(stop)?;
            }
        }
    }
    if right {
        while let Some(len) = lengths
            .iter()
            .copied()
            .find(|&len| len <= end - start && set_chars.contains(&input[end - len..end]))
        {
            end -= len;
            steps += 1;
            if steps.is_multiple_of(CHECK_EVERY) {
                check(stop)?;
            }
        }
    }
    Ok(Out::Text(input[start..end].to_vec()))
}

/// `instr(X, Y)`: the 1-based position of Y in X, in characters (in bytes when
/// both are blobs); 0 when it is not there.
fn instr(ctx: &Context<'_>, stop: &Late) -> Result<Out> {
    check(stop)?;
    let (hay, needle) = (ctx.get_raw(0), ctx.get_raw(1));
    if matches!(hay, ValueRef::Null) || matches!(needle, ValueRef::Null) {
        return Ok(Out::Null);
    }
    let as_text = !matches!((hay, needle), (ValueRef::Blob(_), ValueRef::Blob(_)));
    let (Some(hay), Some(needle)) = (text_arg(ctx, 0)?, text_arg(ctx, 1)?) else {
        return Ok(Out::Null);
    };
    if needle.is_empty() {
        return Ok(Out::Int(1));
    }
    let table = kmp_table(&needle);
    // SQLite tries only character starts; skip matches inside a character.
    let mut found = None;
    for pos in matches(&hay, &needle, &table, false, stop) {
        let pos = pos?;
        if !as_text || pos == 0 || !is_continuation(hay[pos]) {
            found = Some(pos);
            break;
        }
    }
    Ok(Out::Int(match found {
        None => 0,
        // SQLite counts a step to each later byte that starts a character.
        Some(pos) if as_text => {
            1 + hay[1..=pos]
                .iter()
                .filter(|&&b| !is_continuation(b))
                .count() as i64
        }
        Some(pos) => 1 + pos as i64,
    }))
}

/// `replace(X, Y, Z)`: every Y in X (left to right, not overlapping) becomes Z.
fn replace(ctx: &Context<'_>, max_bytes: usize, stop: &Late) -> Result<Out> {
    check(stop)?;
    let Some(input) = text_arg(ctx, 0)? else {
        return Ok(Out::Null);
    };
    let Some(pattern) = text_arg(ctx, 1)? else {
        return Ok(Out::Null);
    };
    // SQLite reads the pattern as C text: one that starts with NUL is empty,
    // and X comes back unchanged (as text).
    if pattern.first().is_none_or(|&b| b == 0) {
        return Ok(Out::Text(input));
    }
    let Some(with) = text_arg(ctx, 2)? else {
        return Ok(Out::Null);
    };
    let table = kmp_table(&pattern);
    let mut out = Vec::with_capacity(input.len());
    let mut copied = 0;
    for pos in matches(&input, &pattern, &table, true, stop) {
        let pos = pos?;
        out.extend_from_slice(&input[copied..pos]);
        out.extend_from_slice(&with);
        copied = pos + pattern.len();
        if out.len() > max_bytes {
            return Err(too_big());
        }
    }
    out.extend_from_slice(&input[copied..]);
    if out.len() > max_bytes {
        return Err(too_big());
    }
    Ok(Out::Text(out))
}

fn too_big() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_TOOBIG),
        None,
    )
}

/// The Knuth-Morris-Pratt failure table: for each prefix of `pattern`, the
/// length of its longest proper prefix that is also a suffix.
fn kmp_table(pattern: &[u8]) -> Vec<usize> {
    let mut table = vec![0; pattern.len()];
    let mut k = 0;
    for i in 1..pattern.len() {
        while k > 0 && pattern[i] != pattern[k] {
            k = table[k - 1];
        }
        if pattern[i] == pattern[k] {
            k += 1;
        }
        table[i] = k;
    }
    table
}

/// Start positions of `pattern` in `text`, left to right. With
/// `non_overlapping`, the search restarts after each match. Ends with an
/// interrupt error once `stop` is set.
fn matches<'a>(
    text: &'a [u8],
    pattern: &'a [u8],
    table: &'a [usize],
    non_overlapping: bool,
    stop: &'a Late,
) -> impl Iterator<Item = Result<usize>> + 'a {
    let mut k = 0;
    let mut i = 0;
    let mut done = false;
    std::iter::from_fn(move || {
        if done {
            return None;
        }
        while i < text.len() {
            if i.is_multiple_of(CHECK_EVERY) && stop.passed() {
                done = true;
                return Some(Err(interrupted()));
            }
            while k > 0 && text[i] != pattern[k] {
                k = table[k - 1];
            }
            if text[i] == pattern[k] {
                k += 1;
            }
            i += 1;
            if k == pattern.len() {
                let start = i - k;
                k = if non_overlapping { 0 } else { table[k - 1] };
                return Some(Ok(start));
            }
        }
        None
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kmp_finds_every_match() {
        let go = Late::default();
        let found = |text: &[u8], pat: &[u8], apart: bool| -> Vec<usize> {
            let t = kmp_table(pat);
            matches(text, pat, &t, apart, &go)
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(found(b"aaaa", b"aa", false), [0, 1, 2]);
        assert_eq!(found(b"aaaa", b"aa", true), [0, 2]);
        assert_eq!(found(b"xababab", b"abab", false), [1, 3]);
        let late = Late::default();
        late.set();
        let t = kmp_table(b"b");
        assert!(
            matches(b"aaaa", b"b", &t, false, &late)
                .next()
                .unwrap()
                .is_err()
        );
    }

    /// Every case gives the same value and type as SQLite's own function.
    #[test]
    fn same_results_as_the_built_ins() {
        let builtin = Connection::open_in_memory().unwrap();
        let ours = Connection::open_in_memory().unwrap();
        register(&ours, 1_000_000, 100, &Arc::default()).unwrap();
        let cases = [
            "trim('xxhixx', 'x')",
            "ltrim('xxhixx', 'x')",
            "rtrim('xxhixx', 'x')",
            "trim('abcba', 'ab')",
            "trim('地图地', '地')",
            "trim('', 'x')",
            "trim('abc', '')",
            "trim(NULL, 'x')",
            "trim('x', NULL)",
            "trim(12321, 1)",
            "trim(1.5, '1')",
            "trim(100.0, '0')",
            "trim(x'78616178', 'x')",
            "trim('a' || char(0) || 'a', 'a')",
            "rtrim('abc', 'c' || char(0) || 'b')",
            "instr('hello', 'l')",
            "instr('hello', 'z')",
            "instr('hello', '')",
            "instr('', '')",
            "instr('', 'a')",
            "instr('地图地图', '图')",
            "instr('aé地b', 'b')",
            "instr(x'00010203', x'0203')",
            "instr(x'61', 'a')",
            "instr(12345, 34)",
            "instr(2.5, '.')",
            "instr(NULL, 'a')",
            "instr('a', NULL)",
            "instr('aaab', 'aab')",
            "replace('hello', 'l', 'L')",
            "replace('aaaa', 'aa', 'b')",
            "replace('hello', '', 'x')",
            "replace(5, '', 'x')",
            "replace(NULL, 'a', 'b')",
            "replace('a', NULL, 'b')",
            "replace('a', 'a', NULL)",
            "replace('abc', 'b', '')",
            "replace(1.25, '.', ',')",
            "replace(1e20, 'e', 'E')",
            "replace(x'616263', 'b', 'X')",
            "replace('地图地图', '地', 'di')",
            "replace('a', 'abc', 'x')",
            "replace('abc', char(0) || 'b', 'x')",
            "replace(x'61FF62', 'b', 'c')",
            "trim(x'C3A9C3', x'C3')",
            "trim(x'80C3A9', x'80')",
        ];
        for expr in cases {
            assert_eq!(eval(&ours, expr), eval(&builtin, expr), "{expr}");
        }
    }

    /// A result as (type, bytes), so text that is not UTF-8 compares too.
    fn eval(conn: &Connection, expr: &str) -> (String, Vec<u8>) {
        conn.query_row(&format!("SELECT typeof({expr}), {expr}"), [], |r| {
            let ty: String = r.get(0)?;
            let bytes = match r.get_ref(1)? {
                ValueRef::Null => Vec::new(),
                ValueRef::Integer(i) => i.to_string().into_bytes(),
                ValueRef::Real(f) => f.to_string().into_bytes(),
                ValueRef::Text(t) | ValueRef::Blob(t) => t.to_vec(),
            };
            Ok((ty, bytes))
        })
        .unwrap_or_else(|e| panic!("{expr}: {e}"))
    }

    /// A small fixed-seed generator (xorshift), so the test needs no crate.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> usize {
            (self.next() % n) as usize
        }
    }

    /// A random argument as SQL: text with ASCII, two- and three-byte
    /// characters and NUL, a blob with arbitrary bytes, a number, or NULL.
    fn random_arg(rng: &mut Rng) -> String {
        const PIECES: [&str; 6] = ["a", "b", "ab", "é", "地", "%"];
        match rng.below(10) {
            0 => "NULL".to_owned(),
            1 => format!("{}", rng.below(300) as i64 - 100),
            2 => format!("{}.{}", rng.below(50), rng.below(100)),
            3 | 4 => {
                // A blob: bytes chosen to include 0x00, continuation bytes
                // (0x80..0xBF) and lead bytes (0xC0 and up).
                const BYTES: [u8; 8] = [0x00, 0x61, 0x62, 0x80, 0xA9, 0xC3, 0xE5, 0xFF];
                let n = rng.below(6);
                let hex: String = (0..n)
                    .map(|_| format!("{:02X}", BYTES[rng.below(8)]))
                    .collect();
                format!("x'{hex}'")
            }
            _ => {
                let n = rng.below(6);
                let mut parts = vec!["''".to_owned()];
                for _ in 0..n {
                    if rng.below(8) == 0 {
                        parts.push("char(0)".to_owned());
                    } else {
                        parts.push(format!("'{}'", PIECES[rng.below(6)]));
                    }
                }
                parts.join(" || ")
            }
        }
    }

    #[test]
    fn random_cases_match_the_built_ins() {
        let builtin = Connection::open_in_memory().unwrap();
        let ours = Connection::open_in_memory().unwrap();
        register(&ours, 1_000_000, 100, &Arc::default()).unwrap();
        let mut rng = Rng(0x5EED_CA55_E77E_0001);
        for _ in 0..10_000 {
            let (a, b, c) = (
                random_arg(&mut rng),
                random_arg(&mut rng),
                random_arg(&mut rng),
            );
            let expr = match rng.below(5) {
                0 => format!("trim({a}, {b})"),
                1 => format!("ltrim({a}, {b})"),
                2 => format!("rtrim({a}, {b})"),
                3 => format!("instr({a}, {b})"),
                _ => format!("replace({a}, {b}, {c})"),
            };
            assert_eq!(eval(&ours, &expr), eval(&builtin, &expr), "{expr}");
        }
    }

    /// A result or the error message, so errors compare too.
    fn eval_or_error(
        conn: &Connection,
        expr: &str,
    ) -> std::result::Result<(String, Vec<u8>), String> {
        conn.query_row(&format!("SELECT typeof({expr}), {expr}"), [], |r| {
            let ty: String = r.get(0)?;
            let bytes = match r.get_ref(1)? {
                ValueRef::Null => Vec::new(),
                ValueRef::Integer(i) => i.to_string().into_bytes(),
                ValueRef::Real(f) => f.to_string().into_bytes(),
                ValueRef::Text(t) | ValueRef::Blob(t) => t.to_vec(),
            };
            Ok((ty, bytes))
        })
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(_, Some(m)) => m,
            other => other.to_string(),
        })
    }

    #[test]
    fn like_and_glob_match_the_built_ins() {
        let builtin = Connection::open_in_memory().unwrap();
        let ours = Connection::open_in_memory().unwrap();
        register(&ours, 1_000_000, 50_000, &Arc::default()).unwrap();
        let cases = [
            "'abc' LIKE 'a%'",
            "'ABC' LIKE 'a_c'",
            "'abc' LIKE 'A%C'",
            "'é' LIKE 'É'",
            "'aé' LIKE 'a_'",
            "'地图' LIKE '地%'",
            "'a%c' LIKE 'a\\%c' ESCAPE '\\'",
            "'abc' LIKE 'a\\%c' ESCAPE '\\'",
            "'a_c' LIKE 'a/_c' ESCAPE '/'",
            "'a%' LIKE 'a%%' ESCAPE '%'",
            "'ab' LIKE 'a%' ESCAPE ''",
            "'ab' LIKE 'a%' ESCAPE 'xy'",
            "'ab' LIKE 'a%' ESCAPE NULL",
            "NULL LIKE 'a'",
            "'a' LIKE NULL",
            "12 LIKE '1_'",
            "1.5 LIKE '1.%'",
            "x'616263' LIKE 'a%'",
            "'a' || char(0) || 'b' LIKE 'a'",
            "'abc' LIKE ''",
            "'' LIKE '%'",
            "'abc' GLOB 'a*'",
            "'abc' GLOB 'A*'",
            "'abc' GLOB 'a?c'",
            "'abc' GLOB '[a-c]bc'",
            "'-bc' GLOB '[a-]bc'",
            "']bc' GLOB '[]]bc'",
            "'xbc' GLOB '[^a-c]bc'",
            "'abc' GLOB '*[c]'",
            "'abc' GLOB '*[d]'",
            "'abc' GLOB '[abc'",
            "'地图' GLOB '[地]*'",
            "'aaaaaaaaab' LIKE '%a%a%a%b'",
            "'aaaaaaaaaa' LIKE '%a%a%a%b'",
        ];
        for expr in cases {
            assert_eq!(
                eval_or_error(&ours, expr),
                eval_or_error(&builtin, expr),
                "{expr}"
            );
        }
        let mut rng = Rng(0x11CE_5EED_0000_0002);
        const PAT: [&str; 17] = [
            "a", "A", "b", "%", "_", "*", "?", "[", "]", "^", "-", "é", "É", "地", "\\", "x", "/",
        ];
        const STR: [&str; 9] = ["a", "A", "b", "é", "地", "-", "]", "%", "x"];
        let piece = |rng: &mut Rng, set: &[&str], max: usize| {
            let n = rng.below(max as u64);
            let mut out = String::new();
            for _ in 0..n {
                if rng.below(30) == 0 {
                    out.push_str("' || char(0) || '");
                } else {
                    out.push_str(set[rng.below(set.len() as u64)]);
                }
            }
            format!("'{out}'")
        };
        for _ in 0..10_000 {
            let pat = piece(&mut rng, &PAT, 7);
            let text = piece(&mut rng, &STR, 8);
            let expr = match rng.below(4) {
                0 => format!("{text} LIKE {pat}"),
                1 => format!("{text} GLOB {pat}"),
                2 => {
                    const ESC: [&str; 5] = ["'\\'", "'%'", "'_'", "'a'", "'/'"];
                    format!("{text} LIKE {pat} ESCAPE {}", ESC[rng.below(5)])
                }
                _ => format!("like({pat}, {text})"),
            };
            assert_eq!(
                eval_or_error(&ours, &expr),
                eval_or_error(&builtin, &expr),
                "{expr}"
            );
        }
    }

    #[test]
    fn long_patterns_are_refused_like_sqlite() {
        let ours = Connection::open_in_memory().unwrap();
        register(&ours, 1_000_000, 5, &Arc::default()).unwrap();
        let err = eval_or_error(&ours, "'a' LIKE 'aaaaaa'").unwrap_err();
        assert_eq!(err, "LIKE or GLOB pattern too complex");
        assert!(eval_or_error(&ours, "'a' LIKE 'aaaaa'").is_ok());
    }

    #[test]
    fn calls_stop_once_late() {
        let ours = Connection::open_in_memory().unwrap();
        let late = Arc::new(Late::default());
        late.set();
        register(&ours, 1_000_000, 100, &late).unwrap();
        for expr in [
            "'a' LIKE 'a'",
            "'a' GLOB 'a'",
            "instr('a', 'a')",
            "replace('a', 'a', 'b')",
            "trim('a', 'b')",
        ] {
            let err = ours
                .query_row(&format!("SELECT {expr}"), [], |r| r.get::<_, i64>(0))
                .unwrap_err();
            assert_eq!(
                err.sqlite_error_code(),
                Some(rusqlite::ErrorCode::OperationInterrupted),
                "{expr}: {err}"
            );
        }
    }

    #[test]
    fn replace_respects_the_length_limit() {
        let ours = Connection::open_in_memory().unwrap();
        register(&ours, 100, 100, &Arc::default()).unwrap();
        let err = ours
            .query_row(
                "SELECT replace(printf('%.*c', 60, 'a'), 'a', 'bb')",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap_err();
        assert!(err.to_string().contains("too big"), "{err}");
    }

    #[test]
    fn chars_follow_sqlite() {
        let split: Vec<&[u8]> = chars("a地b".as_bytes()).collect();
        assert_eq!(split, [b"a" as &[u8], "地".as_bytes(), b"b"]);
    }
}
