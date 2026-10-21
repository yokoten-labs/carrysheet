//! Parsers for the fields the default report shows. Each one returns a value
//! only when it positively recognizes the text as that kind of field; any
//! other text is not shown at all. Nothing here masks: hosts are what the
//! reader wants to know, so they are always shown whole, and everything that
//! could hold a secret (user information, query strings, fragments, other
//! properties) is left out by construction.
//!
//! Paths are the one field that can be both: most are plain folders and
//! file names, but a share link, a webhook address or a path with
//! `;name=value` parameters carries its credential in the path itself. Such
//! a path is left out whole, with the reason, and the host stays.

use super::hidden::hidden;
use crate::report::{FilePath, Target};

/// Longest field shown, in bytes. Longer text is not a host, path or name.
const MAX_FIELD: usize = 1024;

fn printable(text: &str) -> bool {
    !text.is_empty() && text.len() <= MAX_FIELD && !text.chars().any(char::is_control)
}

/// A URL split per RFC 3986: the authority runs from `//` to the first `/`,
/// `?` or `#`, and the user information is everything before its last `@`.
/// The user information, query and fragment are dropped whatever characters
/// they hold; nothing is percent-decoded first, so an encoded character
/// cannot move a boundary.
pub(crate) struct Url {
    /// Lower-cased.
    pub scheme: String,
    /// Empty for `file:///...`.
    pub host: String,
    pub port: Option<u16>,
    /// As written, without query and fragment.
    pub path: String,
    /// Why the path is left out, when it may carry a credential.
    pub withheld: Option<&'static str>,
}

impl Url {
    /// Scheme, host, port and, unless it is withheld, path.
    pub fn shown(&self) -> String {
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        let path = if self.withheld.is_some() {
            ""
        } else {
            &self.path
        };
        format!("{}://{}{port}{path}", self.scheme, self.host)
    }
}

/// `None` when the text is not a URL with a recognizable host, or when an
/// `@` follows the authority: a password holding an unencoded `/`, `?` or
/// `#` ends the authority early and leaves its tail there, so such a URL is
/// not shown at all.
pub(crate) fn url(text: &str) -> Option<Url> {
    let text = text.trim();
    if !printable(text) {
        return None;
    }
    let colon = text.find(':')?;
    let scheme = &text[..colon];
    let scheme_ok = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    let rest = text[colon + 1..].strip_prefix("//")?;
    if !scheme_ok {
        return None;
    }
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let host_port = authority
        .rfind('@')
        .map_or(authority, |at| &authority[at + 1..]);
    let after = &rest[authority_end..];
    if after.contains('@') {
        return None;
    }
    let path_end = after.find(['?', '#']).unwrap_or(after.len());
    let path = after[..path_end].to_string();
    let scheme = scheme.to_ascii_lowercase();
    if host_port.is_empty() {
        // `file:///C:/x.xlsx` has an empty host.
        return (scheme == "file" && !path.is_empty()).then(|| Url {
            scheme,
            host: String::new(),
            port: None,
            path,
            withheld: None,
        });
    }
    let (host, port) = split_port(host_port)?;
    if !is_host(host) {
        return None;
    }
    let withheld = url_path_credential(host, &path);
    Some(Url {
        scheme,
        host: host.to_string(),
        port,
        path,
        withheld,
    })
}

/// Why a URL path may carry a credential, or `None` when it looks like a
/// plain path. Errs towards hiding: a hidden plain path costs a little, a
/// shown share link gives the file away.
fn url_path_credential(host: &str, path: &str) -> Option<&'static str> {
    let host = host.to_ascii_lowercase();
    let lower = path.to_ascii_lowercase();
    let segments: Vec<&str> = lower.split('/').filter(|s| !s.is_empty()).collect();
    if host.starts_with("hooks.")
        || host.contains("webhook")
        || segments
            .iter()
            .any(|s| s.contains("webhook") || *s == "hooks" || *s == "hook")
    {
        return Some(WEBHOOK);
    }
    let share_host = [
        "1drv.ms",
        "dropbox",
        "box.com",
        "drive.google",
        "docs.google",
    ]
    .iter()
    .any(|h| host.contains(h));
    if share_host
        || segments.iter().any(|s| matches!(*s, "s" | "scl" | "sh"))
        || lower.contains('!')
    {
        return Some(SHARE_LINK);
    }
    if lower.contains(':') || lower.contains("%3a") {
        return Some(COLON);
    }
    segment_credential(&lower)
}

const WEBHOOK: &str = "because it looks like a webhook address, whose path is its key";
const SHARE_LINK: &str = "because it looks like a share link, whose path is its access key";
const COLON: &str = "because it has a `:` in its path, as share links and bot tokens do";
const PARAMETERS: &str =
    "because it has `;` or `name=value` parameters, which can carry a session or a key";
const TOKEN: &str = "because a part of it looks like an access token";

/// The checks file paths and URL paths share: parameters and tokens.
fn segment_credential(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();
    if lower.contains([';', '=']) || lower.contains("%3b") || lower.contains("%3d") {
        return Some(PARAMETERS);
    }
    looks_like_token(path).then_some(TOKEN)
}

/// Why a file path may carry a credential. Like a URL path, except that the
/// file name itself may hold `;` and `=` (`Q1 Sales; Final.csv`) when it
/// has an extension and names no secret: `pwd=x` without one still counts.
fn file_credential(path: &str) -> Option<&'static str> {
    let trimmed = path.trim_end_matches(['\\', '/']);
    let (folder, file) = match trimmed.rfind(['\\', '/']) {
        Some(at) => (&trimmed[..at], &trimmed[at + 1..]),
        None => ("", trimmed),
    };
    if let Some(why) = segment_credential(folder) {
        return Some(why);
    }
    if looks_like_token(file) {
        return Some(TOKEN);
    }
    let has_extension = file.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty()
            && (1..=5).contains(&ext.len())
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
    });
    let names_secret = file
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| !w.is_empty() && crate::mask::is_secret_word(w));
    let parameters = file.contains([';', '=']) || file.to_ascii_lowercase().contains("%3d");
    (parameters && (!has_extension || names_secret)).then_some(PARAMETERS)
}

/// True when a run of letters and digits in `text` looks random: 32 or more
/// hex digits, or 20 or more letters and digits that change between lower
/// case, upper case and digit at least two times in five. Words, years and
/// `CamelCase` names change far less often.
pub(crate) fn looks_like_token(text: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric()).any(|run| {
        let digits = run.bytes().any(|b| b.is_ascii_digit());
        let letters = run.bytes().any(|b| b.is_ascii_alphabetic());
        if !(digits && letters) {
            return false;
        }
        if run.len() >= 32 && run.bytes().all(|b| b.is_ascii_hexdigit()) {
            return true;
        }
        if run.len() < 20 {
            return false;
        }
        let class = |b: u8| match b {
            b'a'..=b'z' => 0,
            b'A'..=b'Z' => 1,
            _ => 2,
        };
        let bytes = run.as_bytes();
        let switches = bytes
            .windows(2)
            .filter(|w| class(w[0]) != class(w[1]))
            .count();
        switches * 5 >= bytes.len() * 2
    })
}

/// Splits `host:port` or `[v6]:port`.
fn split_port(text: &str) -> Option<(&str, Option<u16>)> {
    if text.starts_with('[') {
        let close = text.find(']')?;
        let rest = &text[close + 1..];
        let port = match rest.strip_prefix(':') {
            Some(p) => Some(p.parse().ok()?),
            None if rest.is_empty() => None,
            None => return None,
        };
        return Some((&text[..=close], port));
    }
    match text.split_once(':') {
        Some((host, port)) => Some((host, Some(port.parse().ok()?))),
        None => Some((text, None)),
    }
}

/// A host name, an IPv4 address or a bracketed IPv6 address.
fn is_host(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return !inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'));
    }
    !host.is_empty()
        && host.len() <= 253
        && host
            .split('.')
            .all(|label| !label.is_empty() && label.len() <= 63)
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        || matches!(host, "." | "(local)")
}

/// A file path, read into its parts, or why it is left out.
pub(crate) enum PathParse {
    Shown(FilePath),
    Withheld(&'static str),
}

/// Characters no Windows path part may hold. `:` is allowed only after a
/// drive letter.
const NOT_IN_PATH: &[char] = &['<', '>', '"', '|', '?', '*', ':'];

/// A Windows or POSIX file path: `C:\Data\a.xlsx`, `\\server\share\a.xlsx`,
/// `//server/share/a.xlsx`, `/srv/a.csv` or a relative `..\a.xlsx`. `None`
/// when the text is not one.
pub(crate) fn file_path(text: &str) -> Option<PathParse> {
    let text = text.trim();
    if !printable(text) {
        return None;
    }
    let mut rest = text;
    for long in ["\\\\?\\UNC\\", "\\\\?\\"] {
        if let Some(r) = rest.strip_prefix(long) {
            rest = r;
            if long.ends_with("UNC\\") {
                return unc(text, r);
            }
        }
    }
    if let Some(r) = rest
        .strip_prefix("\\\\")
        .or_else(|| rest.strip_prefix("//"))
    {
        return unc(text, r);
    }
    let bytes = rest.as_bytes();
    let (drive, after) = if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        if bytes.len() > 2 && !matches!(bytes[2], b'\\' | b'/') {
            return None;
        }
        (Some(rest[..2].to_string()), &rest[2..])
    } else {
        (None, rest)
    };
    let parts = parts(after)?;
    if let Some(why) = file_credential(after) {
        return Some(PathParse::Withheld(why));
    }
    Some(PathParse::Shown(FilePath {
        path: text.to_string(),
        host: None,
        share: None,
        drive,
        folder: parts.folder,
        file_name: parts.file_name,
    }))
}

/// A network path after its leading `\\` or `//`.
fn unc(text: &str, rest: &str) -> Option<PathParse> {
    let mut split = rest.splitn(2, ['\\', '/']);
    let host = split.next()?;
    if !is_host(host) {
        return None;
    }
    let after = split.next().unwrap_or("");
    let mut split = after.splitn(2, ['\\', '/']);
    let share = split.next().filter(|s| !s.is_empty());
    let after_share = split.next().map(|r| format!("\\{r}")).unwrap_or_default();
    if let Some(share) = share {
        parts(share)?;
    }
    let parts = parts(&after_share)?;
    if let Some(why) = file_credential(after) {
        return Some(PathParse::Withheld(why));
    }
    Some(PathParse::Shown(FilePath {
        path: text.to_string(),
        host: Some(host.to_string()),
        share: share.map(str::to_string),
        drive: None,
        folder: parts.folder,
        file_name: parts.file_name,
    }))
}

struct Parts {
    folder: Option<String>,
    file_name: Option<String>,
}

/// Checks each part of a path and splits off its last part.
fn parts(path: &str) -> Option<Parts> {
    let ok = path
        .split(['\\', '/'])
        .all(|part| part.len() <= 255 && !part.contains(NOT_IN_PATH));
    if !ok {
        return None;
    }
    let trimmed = path.trim_end_matches(['\\', '/']);
    let (folder, file_name) = match trimmed.rfind(['\\', '/']) {
        // A file in the root keeps the separator as its folder.
        Some(0) => (Some(trimmed[..1].to_string()), &trimmed[1..]),
        Some(at) => (Some(trimmed[..at].to_string()), &trimmed[at + 1..]),
        None => (None, trimmed),
    };
    Some(Parts {
        folder,
        file_name: Some(file_name.to_string()).filter(|f| !f.is_empty()),
    })
}

/// Fills `t` from a path or a URL; `false` when the text is neither.
pub(crate) fn set_location(t: &mut Target, text: &str) -> bool {
    let text = text.trim();
    if text.contains("://") {
        return set_url(t, text);
    }
    set_file(t, text)
}

/// Fills `t` from a URL; a `file:` URL becomes a file path. `false` when the
/// text is not a URL with a recognizable host.
pub(crate) fn set_url(t: &mut Target, text: &str) -> bool {
    if t.url.is_some() || t.file.is_some() {
        return true;
    }
    let Some(url) = url(text) else {
        return false;
    };
    if url.scheme == "file" {
        // `file:///C:/x`, `file:///\\server\x`, `file://server/share/x`.
        let path = if url.host.is_empty() {
            let p = url.path.strip_prefix('/').unwrap_or(&url.path);
            if p.starts_with("\\\\") || p.get(1..2) == Some(":") {
                p.to_string()
            } else {
                url.path.clone()
            }
        } else {
            format!("//{}{}", url.host, url.path)
        };
        return set_file(t, &path);
    }
    t.url = Some(url.shown());
    if let Some(why) = url.withheld {
        t.hidden_path = Some(hidden("URL path", &url.path, why));
    }
    true
}

/// Fills `t` from a file path. `false` when the text is not one.
pub(crate) fn set_file(t: &mut Target, text: &str) -> bool {
    if t.url.is_some() || t.file.is_some() {
        return true;
    }
    match file_path(text) {
        Some(PathParse::Shown(path)) => t.file = Some(path),
        Some(PathParse::Withheld(why)) => t.hidden_path = Some(hidden("file path", text, why)),
        None => return false,
    }
    true
}

/// A database server as connection strings write it: `host`, `host,port`,
/// `host:port`, `host\instance`, `tcp:host,port`, `[v6]:port`.
pub(crate) struct Server {
    pub host: String,
    pub port: Option<u16>,
}

pub(crate) fn server(text: &str) -> Option<Server> {
    let mut text = text.trim();
    for prefix in ["tcp:", "np:", "lpc:", "admin:"] {
        if text.len() > prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
            text = &text[prefix.len()..];
        }
    }
    let (rest, port) = match text.rsplit_once(',') {
        Some((rest, port)) => (rest.trim(), Some(port.trim().parse().ok()?)),
        None => (text, None),
    };
    let (rest, instance) = match rest.split_once('\\') {
        Some((host, instance)) => (host, Some(instance)),
        None => (rest, None),
    };
    let (host, port) = match port {
        Some(port) => (rest, Some(port)),
        None if rest.matches(':').count() == 1 || rest.starts_with('[') => split_port(rest)?,
        None => (rest, None),
    };
    if !is_host(host) {
        return None;
    }
    let host = match instance {
        Some(instance) if is_word(instance) => format!("{host}\\{instance}"),
        Some(_) => return None,
        None => host.to_string(),
    };
    Some(Server { host, port })
}

/// An Oracle address: a connect descriptor
/// `(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=db)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=orcl)))`
/// or an Easy Connect string `//db:1521/orcl`. Only the host, port and
/// service are read; the rest of a descriptor is not shown.
pub(crate) fn oracle(text: &str) -> Option<(Server, Option<String>)> {
    let text = text.trim();
    if text.starts_with('(') {
        let lower = text.to_ascii_lowercase();
        let value = |key: &str| -> Option<&str> {
            let at = lower.find(&format!("({key}"))? + key.len() + 1;
            let rest = lower[at..].trim_start().strip_prefix('=')?;
            let start = text.len() - rest.len();
            let end = start + rest.find(')')?;
            Some(text[start..end].trim())
        };
        let host = value("host")?;
        if !is_host(host) {
            return None;
        }
        let port = match value("port") {
            Some(p) => Some(p.parse().ok()?),
            None => None,
        };
        let service = value("service_name")
            .or_else(|| value("sid"))
            .and_then(name);
        return Some((
            Server {
                host: host.to_string(),
                port,
            },
            service,
        ));
    }
    // `user/password` and `user/password@service` look like `host/service`;
    // only `//host...` or `host:port/...` is surely an address.
    let (rest, sure) = match text.strip_prefix("//") {
        Some(rest) => (rest, true),
        None => (text, false),
    };
    if rest.contains('@') {
        return None;
    }
    let (address, service) = rest.split_once('/')?;
    let (host, port) = split_port(address)?;
    if !(sure || port.is_some()) || !is_host(host) || service.contains(['/', '\\']) {
        return None;
    }
    Some((
        Server {
            host: host.to_string(),
            port,
        },
        name(service),
    ))
}

/// Letters, digits, `_`, `$`, `-`.
fn is_word(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && text
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '-'))
}

/// A name: a database, DSN, provider, driver, query or table. Printable,
/// short, and without the characters that would make it a URL, an address
/// or a list of properties.
pub(crate) fn name(text: &str) -> Option<String> {
    let text = text.trim();
    let text = text
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .unwrap_or(text);
    (printable(text)
        && text.len() <= 256
        && !text.contains([
            '@', '/', '\\', '?', '#', '=', ';', '"', '\'', '{', '}', '<', '>', '&',
        ]))
    .then(|| text.to_string())
}

/// A database object name such as `Orders`, `"Sales"."dbo"."Orders"` or
/// `[dbo].[Orders]`: dot-separated parts, each a name or a quoted name.
pub(crate) fn object_name(text: &str) -> Option<String> {
    let text = text.trim();
    if !printable(text) || text.len() > 256 {
        return None;
    }
    let mut rest = text;
    loop {
        let after = match rest.chars().next()? {
            '"' => rest[1..].find('"').map(|e| &rest[e + 2..])?,
            '[' => rest[1..].find(']').map(|e| &rest[e + 2..])?,
            '`' => rest[1..].find('`').map(|e| &rest[e + 2..])?,
            _ => {
                let end = rest.find('.').unwrap_or(rest.len());
                // Unquoted, a part is words split by single spaces.
                if !rest[..end].split(' ').all(is_word) {
                    return None;
                }
                &rest[end..]
            }
        };
        match after.strip_prefix('.') {
            Some(next) => rest = next,
            None if after.is_empty() => return Some(text.to_string()),
            None => return None,
        }
    }
}

const NOT_TEXT: &str =
    "because it is not plain text: it holds characters used in paths, addresses or settings";
const NAMES_SECRET: &str = "because it mentions a password or key";

/// Short text from the file shown as written: a sheet name, a DDE service,
/// topic or item, an OLE program id. Printable, at most 255 characters,
/// without the characters that make text a path, an address, a URL or a
/// list of settings, without a word that names a secret, and not a token.
pub(crate) fn safe_text(text: &str) -> Result<String, &'static str> {
    let text = text.trim();
    let plain = printable(text)
        && text.chars().count() <= 255
        && !text.contains([
            '=', ';', '@', '/', '\\', '?', '#', '"', '{', '}', '<', '>', '|', '`',
        ]);
    if !plain {
        return Err(NOT_TEXT);
    }
    if text
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .any(|w| !w.is_empty() && crate::mask::is_secret_word(w))
    {
        return Err(NAMES_SECRET);
    }
    if looks_like_token(text) {
        return Err(TOKEN);
    }
    Ok(text.to_string())
}

/// True for a drive path (`C:\...`) or a network path (`\\server\...`).
fn is_absolute_path(text: &str) -> bool {
    let b = text.as_bytes();
    text.starts_with("\\\\")
        || text.starts_with("//")
        || b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/')
}

/// A DDE topic in one of its documented forms: a name such as `System` or
/// `Sheet1`; a workbook and sheet such as `[Book1]Sheet1`, optionally after
/// a folder (`C:\Prices\[Live.xlsx]Feed`); or a whole drive or network
/// path. Paths follow the same rules as any path.
pub(crate) fn dde_topic(text: &str) -> Result<String, &'static str> {
    const NOT: &str = "because it is not a name, a path or a `[book]sheet` reference";
    let text = text.trim();
    let path_ok = |path: &str| -> Result<(), &'static str> {
        if !is_absolute_path(path) {
            return Err(NOT);
        }
        match file_path(path) {
            Some(PathParse::Shown(_)) => Ok(()),
            Some(PathParse::Withheld(why)) => Err(why),
            None => Err(NOT),
        }
    };
    if let Some(open) = text.find('[') {
        let close = text[open..].find(']').map(|c| open + c).ok_or(NOT)?;
        let (folder, book, sheet) = (&text[..open], &text[open + 1..close], &text[close + 1..]);
        if !book.chars().all(|c| c.is_ascii_digit()) || book.is_empty() {
            safe_text(book)?;
        }
        if !sheet.is_empty() {
            safe_text(sheet)?;
        }
        if !folder.is_empty() {
            if !folder.ends_with(['\\', '/']) {
                return Err(NOT);
            }
            path_ok(folder.trim_end_matches(['\\', '/']))?;
        }
        return Ok(text.to_string());
    }
    if is_absolute_path(text) {
        path_ok(text)?;
        return Ok(text.to_string());
    }
    safe_text(text)
}

/// A cell reference as a defined name holds it: `Sheet1!$A$1:$D$20`,
/// `'My Sheet'!$A$1`, `#REF!`.
pub(crate) fn reference(text: &str) -> Option<String> {
    let text = text.trim().trim_start_matches('=');
    if !printable(text) || text.len() > 512 {
        return None;
    }
    let rest = match text.strip_prefix('\'') {
        Some(quoted) => {
            let mut k = 0;
            let bytes = quoted.as_bytes();
            loop {
                let at = k + quoted[k..].find('\'')?;
                if bytes.get(at + 1) == Some(&b'\'') {
                    k = at + 2;
                } else {
                    break &quoted[at + 1..];
                }
            }
        }
        None => text,
    };
    rest.chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '$' | '!' | ':' | '.' | '_' | '#' | ' '))
        .then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        PathParse, dde_topic, file_path, looks_like_token, name, object_name, oracle, reference,
        server, set_location, url,
    };
    use crate::report::Target;

    fn shown(text: &str) -> Option<String> {
        url(text).map(|u| u.shown())
    }

    #[test]
    fn urls_keep_host_and_path_only() {
        let cases = [
            (
                "https://bob:p%40ss@files.example.com/a/b.csv?sig=x#t=y",
                Some("https://files.example.com/a/b.csv"),
            ),
            // A raw `/` in a password ends the authority; the rest is not a path.
            ("https://bob:p@ss/w0rd@files.example.com/a/b.csv", None),
            ("https://bob:12?ss@files.example.com/", None),
            (
                "HTTPS://api.example.org:8443/v1",
                Some("https://api.example.org:8443/v1"),
            ),
            (
                "postgres://u:'\"<>`{}|@[::1]:5432/db",
                Some("postgres://[::1]:5432/db"),
            ),
            (
                "mongodb+srv://tok%20en@10.0.0.5",
                Some("mongodb+srv://10.0.0.5"),
            ),
            (
                "https://FinanceDb2023Prod.database.windows.net/",
                Some("https://FinanceDb2023Prod.database.windows.net/"),
            ),
            ("https://bad host/x", None),
            ("https://host:notaport/x", None),
            ("not a url", None),
        ];
        for (input, expected) in cases {
            assert_eq!(shown(input).as_deref(), expected, "{input}");
        }
    }

    #[test]
    fn credential_paths_are_withheld_and_the_host_kept() {
        for input in [
            "https://hooks.slack.com/services/T0001/B0002/abcdEFGH1234ijklMNOP5678",
            "https://contoso.webhook.office.com/webhookb2/x",
            "https://discord.com/api/webhooks/123/abc",
            "https://1drv.ms/x/s!AkL2mN3oP4qR5sT6uV",
            "https://contoso.sharepoint.com/:x:/g/personal/ann/EaBc",
            "https://www.dropbox.com/s/a1b2c3d4e5/report.xlsx",
            "https://drive.google.com/file/d/1AbC2dEf3GhI4jKl5MnO6pQr7StU8vWx/view",
            "https://app.example.com/data;jsessionid=0123ABCD/report",
            "https://app.example.com/data/key=abc/report",
            "https://api.telegram.org/bot123456:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw/x",
            "https://files.example.com/dl/aB3dE5gH7jK9mN1pQ3sT5vW7/report.csv",
        ] {
            let u = url(input).unwrap();
            assert!(u.withheld.is_some(), "{input}");
            assert!(!u.shown().contains('/') || u.shown().matches('/').count() == 2);
        }
        for input in [
            "https://files.example.com/ProjectAlpha2024/Sales_Report_2024_Final_v2.xlsx",
            "https://contoso.sharepoint.com/sites/Finance/Shared%20Documents/Budget.xlsx",
            "https://api.example.org/v1/data/{3F2504E0-4F89-11D3-9A0C-0305E82C3301}",
        ] {
            assert!(url(input).unwrap().withheld.is_none(), "{input}");
        }
    }

    #[test]
    fn tokens() {
        assert!(looks_like_token("aB3dE5gH7jK9mN1pQ3sT5vW7"));
        assert!(looks_like_token("0123456789abcdef0123456789abcdef"));
        assert!(!looks_like_token("ProjectAlpha2024"));
        assert!(!looks_like_token("Quarterly_Report_2024_Final_v2"));
        assert!(!looks_like_token("FinanceDb2023Prod"));
        assert!(!looks_like_token("abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn paths_are_read_into_parts() {
        let Some(PathParse::Shown(p)) = file_path("\\\\files\\finance\\2026\\Budget.xlsx") else {
            panic!()
        };
        assert_eq!(p.host.as_deref(), Some("files"));
        assert_eq!(p.share.as_deref(), Some("finance"));
        assert_eq!(p.folder.as_deref(), Some("\\2026"));
        assert_eq!(p.file_name.as_deref(), Some("Budget.xlsx"));
        let Some(PathParse::Shown(p)) = file_path("C:\\Data\\archive.accdb") else {
            panic!()
        };
        assert_eq!(p.drive.as_deref(), Some("C:"));
        assert_eq!(p.folder.as_deref(), Some("\\Data"));
        assert_eq!(p.file_name.as_deref(), Some("archive.accdb"));
        let Some(PathParse::Shown(p)) = file_path("Old.xlsx") else {
            panic!()
        };
        assert_eq!((p.folder, p.file_name.as_deref()), (None, Some("Old.xlsx")));
        assert!(
            file_path("Password=abc;Server=x").is_some_and(|p| matches!(p, PathParse::Withheld(_)))
        );
        assert!(matches!(
            file_path("D:\\keys\\aB3dE5gH7jK9mN1pQ3sT5vW7.txt"),
            Some(PathParse::Withheld(_))
        ));
        assert!(file_path("a|b").is_none());
        // A file name may hold `;` and `=` when it has an extension and
        // names no secret; a folder may not.
        assert!(matches!(
            file_path("\\\\fs\\share\\Q1 Sales; Final.csv"),
            Some(PathParse::Shown(_))
        ));
        assert!(matches!(file_path("a=b.xlsx"), Some(PathParse::Shown(_))));
        for path in [
            "\\\\fs\\share;token=x\\a.csv",
            "C:\\a=b\\c.csv",
            "pwd=hunter2",
            "pwd=hunter2.csv",
        ] {
            assert!(
                matches!(file_path(path), Some(PathParse::Withheld(_))),
                "{path}"
            );
        }
        assert!(file_path("C:x").is_none());
        assert!(file_path("\\\\bad host\\x").is_none());

        let mut t = Target::default();
        assert!(set_location(&mut t, "file:///C:\\Docs\\Report.docx"));
        assert_eq!(t.file.unwrap().path, "C:\\Docs\\Report.docx");
        let mut t = Target::default();
        assert!(set_location(
            &mut t,
            "https://www.dropbox.com/s/a1b2c3/x.xlsx?dl=0"
        ));
        assert_eq!(t.url.as_deref(), Some("https://www.dropbox.com"));
        let hidden = t.hidden_path.unwrap();
        assert_eq!(hidden.what, "URL path");
        assert!(hidden.note.contains("share link"), "{}", hidden.note);
    }

    #[test]
    fn servers() {
        let s = server("tcp:db.example,1433").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("db.example", Some(1433)));
        let s = server("sql01\\SQLEXPRESS").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("sql01\\SQLEXPRESS", None));
        let s = server("pg.example:5432").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("pg.example", Some(5432)));
        assert!(server("a b;c").is_none());
        assert!(server("user:pass@host").is_none());

        let (s, service) = oracle(
            "(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=ora01.corp.example)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=FIN)))",
        )
        .unwrap();
        assert_eq!(
            (s.host.as_str(), s.port),
            ("ora01.corp.example", Some(1521))
        );
        assert_eq!(service.as_deref(), Some("FIN"));
        let (s, service) = oracle("//ora02:1522/sales.example").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("ora02", Some(1522)));
        assert_eq!(service.as_deref(), Some("sales.example"));
        let (s, service) = oracle("//ora03/sales").unwrap();
        assert_eq!(
            (s.host.as_str(), service.as_deref()),
            ("ora03", Some("sales"))
        );
        // `user/password` forms are not addresses.
        for text in [
            "scott/tiger",
            "scott/tiger@orcl",
            "//scott:1/x@orcl",
            "ora:1521/x@y",
        ] {
            assert!(oracle(text).is_none(), "{text}");
        }
    }

    #[test]
    fn names_references_topics() {
        assert_eq!(
            name("{ODBC Driver 18 for SQL Server}").as_deref(),
            Some("ODBC Driver 18 for SQL Server")
        );
        assert!(name("a;b=c").is_none());
        assert_eq!(
            object_name("\"Sales\".\"dbo\".\"Orders\"").as_deref(),
            Some("\"Sales\".\"dbo\".\"Orders\"")
        );
        assert_eq!(
            object_name("[dbo].[Orders]").as_deref(),
            Some("[dbo].[Orders]")
        );
        assert_eq!(object_name("Finance Cube").as_deref(), Some("Finance Cube"));
        assert!(object_name("SELECT * FROM t").is_none());
        assert!(object_name("a  b").is_none());
        assert_eq!(
            reference("Sheet1!$A$1:$D$20").as_deref(),
            Some("Sheet1!$A$1:$D$20")
        );
        assert_eq!(reference("'It''s'!$A$1").as_deref(), Some("'It''s'!$A$1"));
        assert!(reference("\"secret\"").is_none());

        assert_eq!(dde_topic("System"), Ok("System".to_string()));
        assert_eq!(
            dde_topic("C:\\Prices\\[Live.xlsx]Feed"),
            Ok("C:\\Prices\\[Live.xlsx]Feed".to_string())
        );
        assert!(dde_topic("pwd=aB3dE5gH7jK9mN1pQ3sT5vW7").is_err());
        assert!(dde_topic("aB3dE5gH7jK9mN1pQ3sT5vW7").is_err());
        assert!(dde_topic("[x]a=b").is_err());
        // Only the documented forms: a relative path or free text is not one.
        assert_eq!(dde_topic("[Book1]Sheet1"), Ok("[Book1]Sheet1".to_string()));
        assert_eq!(dde_topic("[1]Feed"), Ok("[1]Feed".to_string()));
        for topic in [
            "Prices\\Feed",
            "a=b",
            "x@host",
            "My password list",
            "C:Prices[Live.xlsx]Feed",
            "notes/[Live.xlsx]Feed",
        ] {
            assert!(dde_topic(topic).is_err(), "{topic}");
        }
    }
}
