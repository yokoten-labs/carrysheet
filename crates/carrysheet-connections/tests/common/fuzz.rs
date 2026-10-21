//! The planted-secret fuzzer from the round 2 review, kept as a regression
//! test. It writes a secret into a connection string, an M expression, SQL
//! or other free text, or a URL, in many encodings, builds a workbook around
//! it and reports every case where six characters of the secret in a row
//! come out of the report.
//!
//! The generator is the reviewer's, unchanged, so a seed gives the same
//! cases here and in the reviewer's copy.

use std::collections::BTreeMap;

use carrysheet_connections::{Limits, MemoryParts, Options, inspect_with};

use super::{Method, base64, mashup_item, qdeff, zip};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }

    fn pick<'a, T>(&mut self, s: &'a [T]) -> &'a T {
        &s[self.below(s.len())]
    }

    fn chance(&mut self, pct: usize) -> bool {
        self.below(100) < pct
    }
}

const ALNUM: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const UNICODE: &[&str] = &["é", "ß", "密", "码", "Ж", "ü", "€", "ñ"];

fn printable() -> Vec<char> {
    (0x20u8..0x7f).map(char::from).collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Alpha {
    Alnum,
    Print,
    PrintUni,
    Lower,
    Hex,
}

fn secret(rng: &mut Rng) -> String {
    let alpha = *rng.pick(&[
        Alpha::Alnum,
        Alpha::Print,
        Alpha::Print,
        Alpha::PrintUni,
        Alpha::Lower,
        Alpha::Hex,
    ]);
    let len = 20 + rng.below(21);
    let alnum: Vec<char> = ALNUM.chars().collect();
    let print = printable();
    let hex: Vec<char> = "0123456789abcdef".chars().collect();
    let mut s = String::new();
    for i in 0..len {
        let edge = i == 0 || i == len - 1;
        let c = match alpha {
            _ if edge => alnum[rng.below(alnum.len())],
            Alpha::Alnum => alnum[rng.below(alnum.len())],
            Alpha::Lower => char::from(b'a' + u8::try_from(rng.below(26)).unwrap()),
            Alpha::Hex => *rng.pick(&hex),
            Alpha::Print => print[rng.below(print.len())],
            Alpha::PrintUni => {
                if rng.chance(15) {
                    s.push_str(rng.pick::<&str>(UNICODE));
                    continue;
                }
                print[rng.below(print.len())]
            }
        };
        s.push(c);
    }
    s
}

fn m_esc(s: &str) -> String {
    s.replace('"', "\"\"")
}

fn json_esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn xml_esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn pct_strict(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            o.push(char::from(b));
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// Sloppy encoding: only what visibly breaks a URL.
fn pct_lax(s: &str, extra: &[u8]) -> String {
    let mut o = String::new();
    for ch in s.chars() {
        let byte = u8::try_from(u32::from(ch)).ok().filter(u8::is_ascii);
        match byte {
            Some(b) if b" \"<>\\`{}|%#?/@^".contains(&b) || extra.contains(&b) => {
                o.push_str(&format!("%{b:02X}"));
            }
            _ => o.push(ch),
        }
    }
    o
}

fn cs_key(rng: &mut Rng) -> String {
    let k = rng
        .pick(&[
            "Password",
            "PWD",
            "pwd",
            "User Password",
            "Jet OLEDB:Database Password",
            "AccountKey",
            "SharedAccessKey",
            "Token",
            "Client Secret",
            "ApiKey",
            "AccessToken",
            "Pass",
            "Auth",
            "client_secret",
            "ProxyPassword",
        ])
        .to_string();
    match rng.below(3) {
        0 => k.to_ascii_uppercase(),
        1 => k.to_ascii_lowercase(),
        _ => k,
    }
}

/// A connection string value encoding of `s` the way ODBC and OLE DB read it.
fn cs_value(rng: &mut Rng, s: &str) -> (String, &'static str) {
    let mut opts = vec![("dq", 0), ("sq", 1), ("br", 2)];
    if !s.contains(';') && !s.starts_with([' ', '"', '\'', '{']) {
        opts.push(("bare", 3));
        opts.push(("bare", 3));
    }
    let (name, o) = *rng.pick(&opts);
    let v = match o {
        0 => format!("\"{}\"", s.replace('"', "\"\"")),
        1 => format!("'{}'", s.replace('\'', "''")),
        2 => format!("{{{}}}", s.replace('}', "}}")),
        _ => s.to_string(),
    };
    (v, name)
}

fn sep(rng: &mut Rng) -> &'static str {
    rng.pick::<&str>(&["=", "=", " = ", "=\r\n", "=\n  ", " =\t"])
}

fn filler_pairs(rng: &mut Rng) -> String {
    let mut v = vec![
        "Provider=SQLOLEDB.1",
        "Data Source=sql01.corp.example",
        "Initial Catalog=Sales",
        "UID=report",
        "Persist Security Info=True",
        "Driver={ODBC Driver 18 for SQL Server}",
        "Server=tcp:db.example,1433",
        "Encrypt=yes",
    ];
    let n = rng.below(4);
    let mut out = Vec::new();
    for _ in 0..n {
        let i = rng.below(v.len());
        out.push(v.remove(i));
    }
    out.join(";")
}

/// A connection string holding `s`, and its label.
fn connection_string(rng: &mut Rng, s: &str, depth: u32) -> (String, String) {
    let key = cs_key(rng);
    let (val, q) = cs_value(rng, s);
    let pair = format!("{key}{}{val}", sep(rng));
    let before = filler_pairs(rng);
    let after = filler_pairs(rng);
    if depth == 0 && rng.chance(25) {
        // Nested in Extended Properties.
        let (inner, lab) = connection_string(rng, s, 1);
        let (wrapped, how) = if rng.chance(50) {
            (format!("\"{}\"", inner.replace('"', "\"\"")), "ep-dq")
        } else {
            (format!("'{}'", inner.replace('\'', "''")), "ep-sq")
        };
        return (
            format!("{before};Extended Properties={wrapped};{after}"),
            format!("cs/{how}/{lab}"),
        );
    }
    let mut parts = vec![];
    if !before.is_empty() {
        parts.push(before);
    }
    parts.push(pair);
    if !after.is_empty() {
        parts.push(after);
    }
    let end = if rng.chance(50) { ";" } else { "" };
    (parts.join(";") + end, format!("cs/{q}"))
}

fn url_key(rng: &mut Rng) -> &'static str {
    rng.pick::<&str>(&[
        "sig",
        "access_token",
        "code",
        "key",
        "api_key",
        "apikey",
        "token",
        "password",
        "client_secret",
        "X-Amz-Signature",
        "subscription-key",
        "pwd",
        "auth",
        "refresh_token",
        "id_token",
        "AccessKey",
    ])
}

fn strictness(strict: bool) -> &'static str {
    if strict { "strict" } else { "lax" }
}

/// A URL holding `s`, its label and the encoded forms of `s` in it.
fn url(rng: &mut Rng, s: &str, depth: u32) -> (String, String, Vec<String>) {
    let scheme = rng.pick(&[
        "https",
        "http",
        "ftp",
        "sftp",
        "postgres",
        "mongodb+srv",
        "HTTPS",
    ]);
    let host = rng.pick(&[
        "files.example.com",
        "api.example.org:8443",
        "10.0.0.5",
        "[::1]:5432",
    ]);
    let path = rng.pick(&[
        "/",
        "/a/b/report.csv",
        "/v1/data",
        "",
        "/ProjectAlpha2024/x.xlsx",
    ]);
    let strict = rng.chance(50);
    let how = strictness(strict);
    match rng.below(if depth == 0 { 6 } else { 4 }) {
        0 => {
            let e = if strict {
                pct_strict(s)
            } else {
                pct_lax(s, b":")
            };
            (
                format!("{scheme}://bob:{e}@{host}{path}?q=1"),
                format!("url/userinfo-pass/{how}"),
                vec![e],
            )
        }
        1 => {
            let e = if strict {
                pct_strict(s)
            } else {
                pct_lax(s, b":")
            };
            (
                format!("{scheme}://{e}@{host}{path}"),
                format!("url/userinfo-token/{how}"),
                vec![e],
            )
        }
        2 => {
            let e = if strict {
                pct_strict(s)
            } else {
                pct_lax(s, b"&=+;")
            };
            let k = url_key(rng);
            (
                format!("{scheme}://{host}{path}?a=1&{k}={e}&b=2"),
                format!("url/query/{how}"),
                vec![e],
            )
        }
        3 => {
            let e = if strict {
                pct_strict(s)
            } else {
                pct_lax(s, b"&=+;")
            };
            let k = url_key(rng);
            (
                format!("{scheme}://{host}{path}#state=x&{k}={e}"),
                format!("url/fragment/{how}"),
                vec![e],
            )
        }
        _ => {
            let (inner, lab, mut forms) = url(rng, s, depth + 1);
            let enc_inner = rng.chance(50);
            let v = if enc_inner {
                pct_strict(&inner)
            } else {
                inner.clone()
            };
            for f in forms.clone() {
                forms.push(pct_strict(&f));
            }
            (
                format!("https://proxy.example/fetch?target={v}&n=1"),
                format!("url/nested{}/{lab}", if enc_inner { "-enc" } else { "" }),
                forms,
            )
        }
    }
}

fn m_key(rng: &mut Rng) -> &'static str {
    rng.pick::<&str>(&[
        "Password",
        "password",
        "ApiKey",
        "#\"x-api-key\"",
        "#\"Ocp-Apim-Subscription-Key\"",
        "token",
        "client_secret",
        "#\"X-Auth-Token\"",
        "AccessToken",
        "pwd",
        "secret",
    ])
}

/// An M expression holding `s`, its label and the encoded forms of `s`.
fn m_expr(rng: &mut Rng, s: &str) -> (String, String, Vec<String>) {
    match rng.below(14) {
        0 => (
            format!(
                "Web.Contents(\"https://api.example/v1\", [Headers=[Authorization=\"Bearer \" & \"{}\"]])",
                m_esc(s)
            ),
            "m/bearer-concat".into(),
            vec![],
        ),
        1 if !s.contains([' ', '"', '\'', ';', ',', '&', ')', ']', '}', '<', '>']) => (
            format!(
                "Web.Contents(\"https://api.example/v1\", [Headers=[Authorization=\"Bearer {}\", Accept=\"application/json\"]])",
                m_esc(s)
            ),
            "m/bearer-inline".into(),
            vec![],
        ),
        2 => (
            format!(
                "Web.Contents(u, [Headers=[{}=\"{}\", Accept=\"json\"]])",
                m_key(rng),
                m_esc(s)
            ),
            "m/header-field".into(),
            vec![],
        ),
        3 => (
            format!(
                "let\n    {} = \"{}\",\n    Source = Web.Contents(u)\nin\n    Source",
                rng.pick(&["ApiKey", "Password", "token", "clientSecret", "pwd"]),
                m_esc(s)
            ),
            "m/let-binding".into(),
            vec![],
        ),
        4 => (
            format!(
                "Web.Contents(u, [Content=Text.ToBinary(\"{{\"\"user\"\":\"\"u\"\",{sp}\"\"{k}\"\":{sp}\"\"{v}\"\"}}\")])",
                k = rng.pick(&[
                    "password",
                    "client_secret",
                    "apiKey",
                    "refresh_token",
                    "pwd"
                ]),
                sp = rng.pick(&["", " "]),
                v = m_esc(&json_esc(s))
            ),
            "m/json-body".into(),
            vec![json_esc(s), m_esc(&json_esc(s))],
        ),
        5 => (
            format!(
                "[Headers=[Authorization=\"Basic \" & Binary.ToText(Text.ToBinary(\"alice:{}\"), BinaryEncoding.Base64)]]",
                m_esc(s)
            ),
            "m/basic-totext".into(),
            vec![],
        ),
        6 => {
            let b = base64(format!("alice:{s}").as_bytes());
            if rng.chance(50) {
                (
                    format!("[Headers=[Authorization=\"Basic {b}\"]]"),
                    "m/basic-b64-inline".into(),
                    vec![b],
                )
            } else {
                (
                    format!("[Headers=[Authorization=\"Basic \" & \"{b}\"]]"),
                    "m/basic-b64-concat".into(),
                    vec![b],
                )
            }
        }
        7 => (
            format!(
                "Json.FromValue([user=\"u\", {}=\"{}\"])",
                rng.pick(&["password", "Password", "apiKey", "secret"]),
                m_esc(s)
            ),
            "m/json-fromvalue".into(),
            vec![],
        ),
        8 => {
            let (cs, lab) = connection_string(rng, s, 0);
            (
                format!(
                    "{}(\"{}\", [])",
                    rng.pick(&["Odbc.DataSource", "OleDb.DataSource", "Odbc.Query"]),
                    m_esc(&cs)
                ),
                format!("m/{lab}"),
                vec![],
            )
        }
        9 => {
            let (u, lab, forms) = url(rng, s, 0);
            let mf = forms.iter().map(|f| m_esc(f)).collect();
            (
                format!("Web.Contents(\"{}\")", m_esc(&u)),
                format!("m/{lab}"),
                mf,
            )
        }
        10 => (
            format!(
                "Sql.Database(\"srv\", \"db\", [Query=\"CREATE LOGIN bob WITH PASSWORD = '{}'\"])",
                m_esc(&s.replace('\'', "''"))
            ),
            "m/sql-in-m".into(),
            vec![],
        ),
        11 => (
            format!("Credential = [Username=\"u\", Password=\"{}\"]", m_esc(s)),
            "m/credential-record".into(),
            vec![],
        ),
        12 => {
            let b = base64(s.as_bytes());
            (
                format!(
                    "[{}=\"{}\"]",
                    rng.pick(&["ApiKey", "Password", "#\"x-functions-key\""]),
                    b
                ),
                "m/b64-value".into(),
                vec![b],
            )
        }
        _ => (
            format!(
                "Xml.Document(\"<Auth><Password>{}</Password></Auth>\")",
                m_esc(&xml_esc(s))
            ),
            "m/xml-in-m".into(),
            vec![xml_esc(s), m_esc(&xml_esc(s))],
        ),
    }
}

/// Free text (SQL command text, XML, JSON) holding `s`.
fn free_text(rng: &mut Rng, s: &str) -> (String, String, Vec<String>) {
    match rng.below(9) {
        0 => (
            format!(
                "SELECT * FROM t WHERE {} = '{}' AND x = 1",
                rng.pick(&["password", "pwd", "api_key"]),
                s.replace('\'', "''")
            ),
            "sql/eq-sq".into(),
            vec![s.replace('\'', "''")],
        ),
        1 => (
            format!(
                "CREATE LOGIN bob WITH PASSWORD = N'{}', CHECK_POLICY = OFF",
                s.replace('\'', "''")
            ),
            "sql/create-login".into(),
            vec![],
        ),
        2 => (
            format!(
                "SET PASSWORD FOR 'bob'@'%' = PASSWORD('{}')",
                s.replace('\'', "''")
            ),
            "sql/set-password".into(),
            vec![],
        ),
        3 if !s.contains([' ', ';', '"', '\'', ',']) => (
            format!("ALTER USER bob IDENTIFIED BY {s};"),
            "sql/identified-bare".into(),
            vec![],
        ),
        4 => (
            format!(
                "ALTER USER bob IDENTIFIED BY \"{}\"",
                s.replace('"', "\"\"")
            ),
            "sql/identified-dq".into(),
            vec![],
        ),
        5 => (
            format!(
                "<config><Password>{}</Password><Server>s</Server></config>",
                xml_esc(s)
            ),
            "xml/element".into(),
            vec![xml_esc(s)],
        ),
        6 if !s.contains("]]>") => (
            format!("<config><password><![CDATA[{s}]]></password></config>"),
            "xml/cdata".into(),
            vec![],
        ),
        7 => (
            format!(
                "{{\"user\": \"u\", \"{}\": \"{}\"}}",
                rng.pick(&["password", "client_secret", "apiKey", "token"]),
                json_esc(s)
            ),
            "json/plain".into(),
            vec![json_esc(s)],
        ),
        _ => {
            let (u, lab, f) = url(rng, s, 0);
            (format!("-- source: {u}\nSELECT 1"), format!("sql/{lab}"), f)
        }
    }
}

/// SQL that compares a secret column with a value holding `s`, in the forms
/// round 4 of the review found shown: values before the column, wrapped in
/// functions, other operators, double-quoted and prefixed strings, a MySQL
/// `#` comment and backslash escapes. Added after the reviewer's generator,
/// so seeds give other cases than in the reviewer's copy.
fn sql_shapes(rng: &mut Rng, s: &str) -> (String, String, Vec<String>) {
    let sq = s.replace('\'', "''");
    let dq = s.replace('"', "\"\"");
    let col = *rng.pick(&[
        "password",
        "pwd",
        "[Password]",
        "`passcode`",
        "api_key",
        "user_pin",
        "u.Password",
    ]);
    let (text, label) = match rng.below(20) {
        0 => (
            format!("SELECT * FROM users WHERE {col}=\"{dq}\""),
            "dq-string",
        ),
        1 => (
            format!("SELECT * FROM users WHERE '{sq}' = {col}"),
            "value-first",
        ),
        2 => (
            format!("SELECT * FROM users WHERE md5('{sq}') = {col}"),
            "function-first",
        ),
        3 => (
            format!("SELECT * FROM users WHERE {col} = LOWER('{sq}')"),
            "wrapped",
        ),
        4 => (
            format!("SELECT * FROM users WHERE CAST({col} AS varchar(64)) = '{sq}'"),
            "cast",
        ),
        5 => (
            format!("SELECT * FROM users WHERE {col} = '{sq}' COLLATE Latin1_General_CS_AS"),
            "collate",
        ),
        6 => (
            format!("SELECT * FROM users WHERE {col} BETWEEN '{sq}' AND 'zz'"),
            "between",
        ),
        7 => (
            format!("SELECT * FROM users WHERE {col} ILIKE '{sq}'"),
            "ilike",
        ),
        8 => (
            format!("SELECT * FROM users WHERE {col} SIMILAR TO '{sq}'"),
            "similar",
        ),
        9 => (
            format!("SELECT * FROM users WHERE {col} IS NOT DISTINCT FROM '{sq}'"),
            "distinct",
        ),
        10 => (
            format!("SELECT * FROM users WHERE {col} = ANY(ARRAY['{sq}'])"),
            "any-array",
        ),
        11 => (
            format!("SELECT * FROM users WHERE {col} IN (SELECT '{sq}')"),
            "in-select",
        ),
        12 => (
            format!("SELECT * FROM users WHERE {col} = _utf8mb4'{sq}'"),
            "charset-prefix",
        ),
        13 if !s.contains("]'") => (
            format!("SELECT * FROM users WHERE {col} = q'[{s}]'"),
            "q-quote",
        ),
        14 => (
            format!("SELECT * FROM users WHERE {col} = U&'{sq}'"),
            "unicode-prefix",
        ),
        15 => (format!("SELECT '{sq}' AS {col} FROM t"), "alias"),
        16 => (
            format!("SELECT * FROM t WHERE x = '{sq}' AND {col} = x"),
            "apart",
        ),
        17 => (
            format!("SELECT * FROM t WHERE c = 'password {sq}'"),
            "word-in-value",
        ),
        18 => (format!("SELECT a FROM t #password={s}"), "hash-comment"),
        _ => (
            format!("SELECT * FROM t WHERE a = 'x\\'' AND {col} = '{sq}' AND b = 'z\\''"),
            "backslash",
        ),
    };
    (text, format!("sql-shape/{label}"), vec![sq, dq])
}

/// Where a case's text goes in the workbook.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    ConnectionString,
    MSection,
    Command,
    Url,
}

pub struct Case {
    text: String,
    label: String,
    secret: String,
    forms: Vec<String>,
    place: Place,
}

fn make_case(rng: &mut Rng) -> Case {
    let s = secret(rng);
    let (text, label, forms, place) = match rng.below(5) {
        0 => {
            let (t, l) = connection_string(rng, &s, 0);
            (t, l, vec![], Place::ConnectionString)
        }
        1 => {
            let (t, l, f) = m_expr(rng, &s);
            (t, l, f, Place::MSection)
        }
        2 => {
            let (t, l, f) = free_text(rng, &s);
            (t, l, f, Place::Command)
        }
        3 => {
            let (t, l, f) = sql_shapes(rng, &s);
            (t, l, f, Place::Command)
        }
        _ => {
            let (t, l, f) = url(rng, &s, 0);
            (t, l, f, Place::Url)
        }
    };
    Case {
        text,
        label,
        secret: s,
        forms,
        place,
    }
}

fn workbook(case: &Case) -> MemoryParts {
    let mut parts = MemoryParts::new();
    let text = &case.text;
    let (conn, section) = match case.place {
        Place::ConnectionString => (
            format!(
                r#"<connection id="1" name="A" type="5"><dbPr connection="{}" command="t" commandType="3"/></connection>"#,
                xml_esc(text)
            ),
            format!(
                "section Section1;\nshared Q = OleDb.DataSource(\"{}\");",
                m_esc(text)
            ),
        ),
        Place::MSection => (
            String::new(),
            format!("section Section1;\nshared Q = {text};"),
        ),
        Place::Command => (
            format!(
                r#"<connection id="1" name="A" type="1"><dbPr connection="DSN=x" command="{}" commandType="2"/></connection>"#,
                xml_esc(text)
            ),
            format!(
                "section Section1;\nshared Q = Odbc.Query(\"dsn=x\", \"{}\");",
                m_esc(text)
            ),
        ),
        Place::Url => (
            format!(
                r#"<connection id="1" name="A" type="4"><webPr url="{}"/></connection><connection id="2" name="B" type="5"><dbPr connection="Provider=SQLOLEDB;Data Source=&quot;{}&quot;;"/></connection>"#,
                xml_esc(text),
                xml_esc(&text.replace('"', "\"\""))
            ),
            format!(
                "section Section1;\nshared Q = Web.Contents(\"{}\");",
                m_esc(text)
            ),
        ),
    };
    if !conn.is_empty() {
        parts.insert(
            "xl/connections.xml",
            format!(
                r#"<connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">{conn}</connections>"#
            )
            .into_bytes(),
        );
    }
    if case.place == Place::Url {
        parts.insert(
            "xl/externalLinks/externalLink1.xml",
            br#"<externalLink xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><externalBook r:id="rId1"/></externalLink>"#.to_vec(),
        );
        parts.insert(
            "xl/externalLinks/_rels/externalLink1.xml.rels",
            format!(
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="x/externalLinkPath" Target="{}" TargetMode="External"/></Relationships>"#,
                xml_esc(text)
            )
            .into_bytes(),
        );
    }
    let z = zip(&[("Formulas/Section1.m", section.as_bytes(), Method::Deflated)]);
    parts.insert("customXml/item1.xml", mashup_item(&qdeff(&z)));
    parts
}

fn strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        serde_json::Value::Object(m) => m.values().for_each(|x| strings(x, out)),
        _ => {}
    }
}

/// The first six characters of the secret (or of an encoded form of it)
/// found in `outputs`, unless they also occur in the case's text outside
/// the secret.
fn leak(case: &Case, outputs: &[String]) -> Option<String> {
    let mut forms = vec![case.secret.clone()];
    forms.extend(case.forms.iter().cloned());
    for form in &forms {
        let chars: Vec<char> = form.chars().collect();
        if chars.len() < 6 {
            continue;
        }
        let context = case.text.replacen(form.as_str(), "\u{0}", usize::MAX);
        for w in chars.windows(6) {
            let piece: String = w.iter().collect();
            if context.contains(&piece) || piece.trim().len() < 6 {
                continue;
            }
            if piece.chars().all(|c| !c.is_alphanumeric()) {
                continue;
            }
            if outputs.iter().any(|o| o.contains(&piece)) {
                return Some(piece);
            }
        }
    }
    None
}

/// What a run found.
pub struct Run {
    pub cases: usize,
    /// Leaks by case label.
    pub leaks: BTreeMap<String, usize>,
    /// The first few leaks, described for a failure message.
    pub examples: Vec<String>,
}

impl Run {
    pub fn total(&self) -> usize {
        self.leaks.values().sum()
    }
}

/// Runs `rounds` cases from `seed` through `inspect_with(.., options)`.
pub fn run(seed: u64, rounds: usize, options: &Options) -> Run {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let mut out = Run {
        cases: 0,
        leaks: BTreeMap::new(),
        examples: Vec::new(),
    };
    for _ in 0..rounds {
        let case = make_case(&mut rng);
        let report = inspect_with(&workbook(&case), &Limits::default(), options);
        let value = serde_json::to_value(&report).unwrap();
        let mut texts = Vec::new();
        strings(&value, &mut texts);
        out.cases += 1;
        if let Some(piece) = leak(&case, &texts) {
            *out.leaks.entry(case.label.clone()).or_default() += 1;
            if out.examples.len() < 5 {
                let found = texts.iter().find(|s| s.contains(&piece)).cloned();
                out.examples.push(format!(
                    "[{}] piece {piece:?} of {:?}\n  in  {:?}\n  out {found:?}",
                    case.label, case.secret, case.text
                ));
            }
        }
    }
    out
}
