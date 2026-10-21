//! Secrets: the default report shows parsed fields only, so the review's
//! fuzzer must find no secret in it at all. The best-effort masking used for
//! raw texts (`include_raw`) has its own tables and loops; it cannot promise
//! the same.

mod common;

use carrysheet_connections::{
    Limits, MemoryParts, Options, inspect_with, mask_connection_string, mask_secrets,
};
use common::{Method, fuzz, mashup_item, qdeff, zip};

/// Default output: zero leaks over every shape the fuzzer makes. Release
/// runs of the reviewer's copy went to 40,000 cases; a debug test run keeps
/// to fewer, from two seeds.
#[test]
fn fuzzer_finds_no_secret_in_the_default_report() {
    for seed in [1, 2] {
        let run = fuzz::run(seed, 3_000, &Options::default());
        assert_eq!(run.cases, 3_000);
        assert_eq!(
            run.total(),
            0,
            "seed {seed}: {:?}\n{}",
            run.leaks,
            run.examples.join("\n")
        );
    }
}

/// Raw texts are masked by patterns, which is best effort: this records how
/// far it gets and keeps it from getting worse, but does not ask for zero.
#[test]
fn fuzzer_on_raw_texts_is_best_effort() {
    let run = fuzz::run(1, 1_000, &Options { include_raw: true });
    assert_eq!(run.cases, 1_000);
    assert!(
        run.total() <= RAW_LEAKS_AT_MOST,
        "{} of 1000 raw cases leaked: {:?}\n{}",
        run.total(),
        run.leaks,
        run.examples.join("\n")
    );
}

/// The leaks the raw path had when this test was last measured (seed 1,
/// 1,000 cases); lower it when masking improves. Round 5 added 20 SQL
/// shapes to the generator: they account for 111 of these, and the older
/// kinds went from 129 to 94.
const RAW_LEAKS_AT_MOST: usize = 205;

#[test]
fn connection_string_table() {
    let cases: &[(&str, &str)] = &[
        ("Server=s;Password=abc", "Server=s;Password=***"),
        ("Server=s;PWD=abc;", "Server=s;PWD=***;"),
        ("pwd=abc", "pwd=***"),
        ("PaSsWoRd=abc;Server=s", "PaSsWoRd=***;Server=s"),
        ("Password=\"a;b\";Server=s", "Password=***;Server=s"),
        ("Password=\"a\"\"b;c\";Server=s", "Password=***;Server=s"),
        ("Password='a;b''c';Server=s", "Password=***;Server=s"),
        ("PWD={a;b}}c};Server=s", "PWD=***;Server=s"),
        (
            "  Password  =  abc  ;Server=s",
            "  Password  =  ***;Server=s",
        ),
        ("Pwd = abc", "Pwd = ***"),
        (
            "User Password=abc;Data Source=x",
            "User Password=***;Data Source=x",
        ),
        (
            "Jet OLEDB:Database Password=abc;Data Source=x.mdb",
            "Jet OLEDB:Database Password=***;Data Source=x.mdb",
        ),
        (
            "Password=a;Server=s;Password=b",
            "Password=***;Server=s;Password=***",
        ),
        ("AccessToken=eyJ0.x.y;Server=s", "AccessToken=***;Server=s"),
        ("Token=abc;Server=s", "Token=***;Server=s"),
        (
            "Endpoint=sb://ns.example.net/;SharedAccessKeyName=Root;SharedAccessKey=ab/c+d=",
            "Endpoint=sb://ns.example.net/;SharedAccessKeyName=Root;SharedAccessKey=***",
        ),
        (
            "DefaultEndpointsProtocol=https;AccountName=acct;AccountKey=abc/def+==;EndpointSuffix=core.windows.net",
            "DefaultEndpointsProtocol=https;AccountName=acct;AccountKey=***;EndpointSuffix=core.windows.net",
        ),
        ("Data Source=x;Secret=abc", "Data Source=x;Secret=***"),
        (
            "ApiKey=abc;Url=https://x.example",
            "ApiKey=***;Url=https://x.example",
        ),
        (
            "Client Secret=abc;Client Id=x",
            "Client Secret=***;Client Id=x",
        ),
        (
            "Provider=MSDASQL;Extended Properties=\"DSN=x;PWD=abc\"",
            "Provider=MSDASQL;Extended Properties=\"DSN=x;PWD=***\"",
        ),
        (
            "Provider=MSDASQL;Extended Properties='DSN=x;PWD=\"a;b\"'",
            "Provider=MSDASQL;Extended Properties='DSN=x;PWD=***'",
        ),
        (
            "Provider=MSDASQL;Extended Properties=\"DSN=x;PWD=\"\"a;b\"\";UID=u\"",
            "Provider=MSDASQL;Extended Properties=\"DSN=x;PWD=***;UID=u\"",
        ),
        (
            "Data Source=https://u:p@h.example/x",
            "Data Source=https://u:***@h.example/x",
        ),
        ("Password=", "Password="),
        ("Password=\"unclosed;Server=s", "Password=***"),
        ("My==Name=1;Pwd=x", "My==Name=1;Pwd=***"),
        ("Password=abc;;;Server=s", "Password=***;;;Server=s"),
        (
            "Server=s;UID=user;Integrated Security=SSPI",
            "Server=s;UID=user;Integrated Security=SSPI",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(&mask_connection_string(input), expected, "input: {input}");
    }
}

#[test]
fn text_and_url_table() {
    let cases: &[(&str, &str)] = &[
        (
            "https://user:pass@host.example/path",
            "https://user:***@host.example/path",
        ),
        (
            "https://tok123@git.example/x.git",
            "https://***@git.example/x.git",
        ),
        (
            "https://acct.blob.core.windows.net/c/b?sv=2020&sig=abc%2Bdef&se=x",
            "https://acct.blob.core.windows.net/c/b?sv=2020&sig=***&se=x",
        ),
        (
            "https://x.example/api?access_token=abc&x=1",
            "https://x.example/api?access_token=***&x=1",
        ),
        (
            "https://x.example/api?ACCESS_TOKEN=abc",
            "https://x.example/api?ACCESS_TOKEN=***",
        ),
        (
            "https://x.example/cb#access_token=abc&state=1",
            "https://x.example/cb#access_token=***&state=1",
        ),
        (
            "https://x.example/fn?code=abc",
            "https://x.example/fn?code=***",
        ),
        (
            "https://x.example/maps?key=abc;v=3",
            "https://x.example/maps?key=***;v=3",
        ),
        (
            "https://x.example/a?access%5Ftoken=abc",
            "https://x.example/a?access%5Ftoken=***",
        ),
        (
            "see (https://u:p@h.example) now",
            "see (https://u:***@h.example) now",
        ),
        (
            "SELECT * FROM t WHERE pwd = 'abc'",
            "SELECT * FROM t WHERE pwd = '***'",
        ),
        (
            "CREATE LOGIN x WITH PASSWORD = 'a''b'",
            "CREATE LOGIN x WITH PASSWORD = '***'",
        ),
        (
            "CREATE USER x WITH PASSWORD 'abc'",
            "CREATE USER x WITH PASSWORD '***'",
        ),
        (
            "ALTER USER x IDENTIFIED BY secret1",
            "ALTER USER x IDENTIFIED BY ***",
        ),
        ("Authorization: Bearer abc.def.ghi", "Authorization: ***"),
        (
            "curl -H \"Authorization: Bearer abcdefghij\"",
            "curl -H \"Authorization: ***\"",
        ),
        ("send Bearer abcdefghijkl to it", "send Bearer *** to it"),
        (
            "{\"password\": \"abc\", \"user\": \"u\"}",
            "{\"password\": \"***\", \"user\": \"u\"}",
        ),
        (
            "<Password>abc</Password><User>u</User>",
            "<Password>***</Password><User>u</User>",
        ),
        (
            "Odbc.Query(\"dsn=x;pwd=abc;\", \"select 1\")",
            "Odbc.Query(\"dsn=x;pwd=***;\", \"select 1\")",
        ),
        (
            "[Headers=[#\"api-key\"=\"abc\"]]",
            "[Headers=[#\"api-key\"=\"***\"]]",
        ),
        ("Server=s;Password=abc", "Server=s;Password=***"),
        ("token: abc def", "token: ***"),
        ("if a == b then pwd", "if a == b then pwd"),
        (
            "Sql.Database(\"srv\", \"db\")",
            "Sql.Database(\"srv\", \"db\")",
        ),
        (
            "file:///C:\\Users\\x\\Book.xlsx",
            "file:///C:\\Users\\x\\Book.xlsx",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(&mask_secrets(input), expected, "input: {input}");
    }
}

/// Every case from the review probe: none may show the planted secret.
#[test]
fn review_probe_cases_hide_the_secret() {
    let connection_strings = [
        "Driver={SQL Server};Server=s;UID=u;PWD==Secr3tXYZ;",
        "Provider=SQLOLEDB;Password=\r\n'ab;LEAKED=ef';Data Source=s",
    ];
    for input in connection_strings {
        let masked = mask_connection_string(input);
        assert!(
            !masked.contains("LEAKED") && !masked.contains("Secr3t"),
            "{input:?} => {masked:?}"
        );
    }
    let texts = [
        "Odbc.DataSource(\"dsn=x;pwd={ab}}LEAKED;ef}\")",
        "OleDb.DataSource(\"Provider=SQLOLEDB;Password=P@ss&LEAKEDword1;\")",
        "Odbc.DataSource(\"dsn=x;pwd=it's-LEAKED-secret\")",
        "Odbc.DataSource(\"dsn=x;pwd=ab\"\"LEAKED\")",
        "Content=Text.ToBinary(\"{\"\"username\"\":\"\"u\"\",\"\"password\"\":\"\"LEAKEDhunter2\"\"}\")",
        "Headers=[Authorization=\"Basic \" & Binary.ToText(Text.ToBinary(\"alice:LEAKEDhunter2\"), 0)]",
        "Web.Contents(\"https://proxy.corp/fetch?target=ftp://user:LEAKEDhunter2@files.corp/a.csv\")",
        "Web.Contents(\"https://proxy.corp/fetch?target=ftp%3A%2F%2Fuser%3ALEAKED%40files.corp\")",
        "[#\"x-api-key\" =\n    \"LEAKEDhunter2\"]",
        "[Headers=[Authorization=\"Bearer \" & \"LEAKEDeyJhbGciOiJIUzI1NiJ9\"]]",
        "<Password><![CDATA[LEAKEDhunter2]]></Password>",
        "a < b <Password>LEAKEDhunter2</Password>",
        "Authorization: Basic\tLEAKEDdXNlcjpw",
        "x Basic LEAKED y",
        "dbPass = \"LEAKEDhunter2\"",
        "v_password := 'LEAKEDhunter2'",
        "exec p @user='u', password => 'LEAKEDhunter2'",
        "https://user:pa/LEAKED@host/",
        "https://user:pa?LEAKED@host/",
        "{\"password\":\"LEAKEDhunter2\"}",
        "{\"password\":\"ab\\\"LEAKED\"}",
        "pwd=ab<LEAKED>cd",
        "pwd=ab>LEAKED",
        "pwd=ab'LEAKED",
        "pwd=ab&LEAKED",
        "SET PASSWORD FOR 'u'@'h' = PASSWORD('LEAKEDhunter2')",
        "PWD==LEAKED",
    ];
    for input in texts {
        let masked = mask_secrets(input);
        assert!(
            !masked.contains("LEAKED") && !masked.contains("hunter2"),
            "{input:?} => {masked:?}"
        );
    }
}

/// The new rules, with the text around the secret kept readable.
#[test]
fn escapes_separators_and_safety_net() {
    let cases: &[(&str, &str)] = &[
        (
            "Odbc.DataSource(\"dsn=x;pwd=ab\"\"cd;Server=s\")",
            "Odbc.DataSource(\"dsn=x;pwd=***;Server=s\")",
        ),
        (
            "Odbc.DataSource(\"dsn=x;pwd={ab}}cd;ef};Server=s\")",
            "Odbc.DataSource(\"dsn=x;pwd={***};Server=s\")",
        ),
        (
            "Text.ToBinary(\"{\"\"user\"\":\"\"u\"\",\"\"password\"\":\"\"x\"\"}\")",
            "Text.ToBinary(\"{\"\"user\"\":\"\"u\"\",\"\"password\"\":\"\"***\"\"}\")",
        ),
        ("{\"password\":\"x\"}", "{\"password\":\"***\"}"),
        (
            "https://proxy.example/f?target=ftp://user:pw@files.example/a.csv&n=1",
            "https://proxy.example/f?target=ftp://user:***@files.example/a.csv&n=1",
        ),
        ("https://user:pa/ss@host/x", "https://user:***@host/x"),
        (
            "<Password><![CDATA[x]]></Password>",
            "<Password>***</Password>",
        ),
        (
            "a < b <Password>x</Password>",
            "a < b <Password>***</Password>",
        ),
        ("dbPass = \"x\"", "dbPass = \"***\""),
        ("v_password := 'x'", "v_password := '***'"),
        ("f(password => 'x')", "f(password => '***')"),
        ("x Basic dXNlcjpw y", "x Basic *** y"),
        (
            "SET PASSWORD FOR u = PASSWORD('x')",
            "SET PASSWORD FOR u = PASSWORD('***')",
        ),
        (
            "[Headers=[Authorization=\"Bearer \" & \"tok\"], Query=[q=\"sales\"]]",
            "[Headers=[Authorization=\"***\" & \"***\"], Query=[q=\"sales\"]]",
        ),
        (
            "Text.ToBinary(\"Basic \" & Binary.ToText(Text.ToBinary(\"u:p\")))",
            "Text.ToBinary(\"Basic \" & Binary.ToText(Text.ToBinary(\"***\")))",
        ),
        (
            "Web.Contents(url) // aB3dE5gH7jK9mN1pQ3sT5vW7",
            "Web.Contents(url) // ***",
        ),
        (
            "[#\"X-Key\"=k, Accept=\"json\"]\nnext line",
            "[#\"X-Key\"=***\"json\"]\nnext line",
        ),
        ("pwd=ab&cd;Server=s", "pwd=***;Server=s"),
        (
            "https://x.example/a?sig=abc&region=eu",
            "https://x.example/a?sig=***&region=eu",
        ),
        (
            "SELECT name, city FROM t WHERE city = 'Oslo'",
            "SELECT name, city FROM t WHERE city = 'Oslo'",
        ),
        (
            "{3F2504E0-4F89-11D3-9A0C-0305E82C3301}",
            "{3F2504E0-4F89-11D3-9A0C-0305E82C3301}",
        ),
    ];
    for (input, expected) in cases {
        assert_eq!(&mask_secrets(input), expected, "input: {input}");
    }
    assert_eq!(
        mask_connection_string("Driver={SQL Server};UID=u;PWD==Secr3t;Server=s"),
        "Driver={SQL Server};UID=u;PWD=***;Server=s"
    );
    assert_eq!(
        mask_connection_string("Provider=P;Password=\r\n'a;b=c';Data Source=s"),
        "Provider=P;Password=\r\n***;Data Source=s"
    );
}

/// A small deterministic generator (xorshift64*), so failures repeat.
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

    /// A secret of 12 to 28 characters from `alphabet`, starting with a
    /// letter so it never begins like a quote or brace.
    fn secret(&mut self, alphabet: &[u8]) -> String {
        let len = 12 + self.below(17);
        let mut out = String::from("Q");
        for _ in 1..len {
            out.push(char::from(alphabet[self.below(alphabet.len())]));
        }
        out
    }
}

const ALNUM: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
/// Characters a value may hold without quotes in a connection string.
const BARE: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#$%()*+,-./:=?@[]^_`|~ ";
/// Characters for quoted values: also `;`, both quotes and braces.
const QUOTED: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#$%()*+,-./:;=?@[]^_`{|}~ '\"";
/// Characters for a bare value in an M string: everything but `;` and line
/// breaks, so `&`, both quotes, `<`, `>` and braces too.
const M_BARE: &[u8] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789!#$%&'()*+,-./:<=>?@[]^_`{|}~ \"";
/// Characters allowed in a URL user name, password or parameter value.
const URL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~%+";

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A workbook with secrets planted in every place they can appear.
fn planted_workbook(rng: &mut Rng) -> (MemoryParts, Vec<String>) {
    let mut secrets = Vec::new();
    let mut plant = |alphabet: &[u8], rng: &mut Rng| {
        let secret = rng.secret(alphabet);
        secrets.push(secret.clone());
        secret
    };
    let bare_keys = [
        "Password",
        "PWD",
        "pwd",
        "User Password",
        "AccessToken",
        "AccountKey",
        "Client Secret",
        "ApiKey",
    ];
    let bare_key = bare_keys[rng.below(bare_keys.len())];
    let bare = plant(BARE, rng);
    let bare = bare.trim_end().to_string();
    let quoted = plant(QUOTED, rng);
    let braced = plant(QUOTED, rng);
    let nested = plant(BARE, rng);
    let nested = nested.trim_end().to_string();
    let connection = format!(
        "Provider=MSDASQL.1;Data Source=db;{bare_key}={bare};Password=\"{}\";Extended Properties=\"DSN=x;PWD={nested};\";PWD={{{}}};Server=s",
        quoted.replace('"', "\"\""),
        braced.replace('}', "}}"),
    );
    let sql = plant(ALNUM, rng);
    let command = format!("SELECT 1 WHERE password = '{sql}'");
    let user = plant(URL, rng);
    let pass = plant(URL, rng);
    let sig = plant(URL, rng);
    let url = format!("https://{user}@a.example/x?sig={sig}&user=bob");
    let url2 = format!("https://bob:{pass}@b.example/Book.xlsx");
    let post = plant(ALNUM, rng);
    let m_pwd = plant(ALNUM, rng);
    let m_bearer = plant(ALNUM, rng);
    let m_key = plant(URL, rng);
    let m_bare = plant(M_BARE, rng);
    let m_braced = plant(QUOTED, rng);
    let m_json = plant(QUOTED, rng);
    let m_basic = plant(ALNUM, rng);

    let connections = format!(
        r#"<connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<connection id="1" name="A" type="1"><dbPr connection="{}" command="{}" commandType="2"/></connection>
<connection id="2" name="B" type="4"><webPr url="{}" post="{}"/></connection>
</connections>"#,
        xml_escape(&connection),
        xml_escape(&command),
        xml_escape(&url),
        xml_escape(&format!("token={post}")),
    );
    let json = m_json.replace('\\', "\\\\").replace('"', "\\\"");
    let section = format!(
        "section Section1;\nshared Q = Odbc.Query(\"dsn=x;pwd={m_pwd};\", \"select 1\");\nshared R = Web.Contents(\"https://c.example/?key={m_key}\", [Headers=[Authorization=\"Bearer {m_bearer}\"]]);\nshared S = Odbc.DataSource(\"dsn=x;pwd={}\") & Odbc.DataSource(\"dsn=y;pwd={{{}}};uid=u\");\nshared T = Web.Contents(\"https://d.example/\", [Content=Text.ToBinary(\"{{\"\"user\"\":\"\"u\"\",\"\"password\"\":\"\"{}\"\"}}\"), Headers=[Authorization=\"Basic \" & Binary.ToText(Text.ToBinary(\"u:{m_basic}\"))]]);",
        m_bare.trim_end().replace('"', "\"\""),
        m_braced.replace('}', "}}").replace('"', "\"\""),
        json.replace('"', "\"\""),
    );
    let package = zip(&[("Formulas/Section1.m", section.as_bytes(), Method::Deflated)]);

    let mut parts = MemoryParts::new();
    parts.insert("xl/connections.xml", connections.into_bytes());
    parts.insert(
        "xl/externalLinks/externalLink1.xml",
        br#"<externalLink xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><externalBook r:id="rId1"/></externalLink>"#.to_vec(),
    );
    parts.insert(
        "xl/externalLinks/_rels/externalLink1.xml.rels",
        format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="x/externalLinkPath" Target="{}" TargetMode="External"/></Relationships>"#,
            xml_escape(&url2)
        )
        .into_bytes(),
    );
    parts.insert("customXml/item1.xml", mashup_item(&qdeff(&package)));
    (parts, secrets)
}

#[test]
fn planted_secrets_never_come_out() {
    for include_raw in [false, true] {
        planted_secrets_never_come_out_of(&Options { include_raw });
    }
}

fn planted_secrets_never_come_out_of(options: &Options) {
    let mut rng = Rng(0x5EED_CAFE_F00D_1234);
    for round in 0..300 {
        let (parts, secrets) = planted_workbook(&mut rng);
        let report = inspect_with(&parts, &Limits::default(), options);
        assert!(
            report.problems.is_empty(),
            "round {round}: {:#?}",
            report.problems
        );
        assert_eq!(report.connections.len(), 2);
        assert_eq!(
            report.power_query.as_ref().map(|p| p.queries.len()),
            Some(4)
        );
        let json = serde_json::to_string(&report).unwrap();
        // Read the JSON back so escapes (\" and \\) do not hide a leak.
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let mut text = String::new();
        collect_strings(&value, &mut text);
        for secret in &secrets {
            // Any eight characters of the secret in a row count as a leak.
            let chars: Vec<char> = secret.chars().collect();
            for window in chars.windows(8) {
                let piece: String = window.iter().collect();
                assert!(
                    !text.contains(&piece),
                    "round {round}: `{piece}` of `{secret}` leaked in {json}"
                );
            }
        }
    }
}

fn collect_strings(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::String(s) => {
            out.push_str(s);
            out.push('\n');
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}
