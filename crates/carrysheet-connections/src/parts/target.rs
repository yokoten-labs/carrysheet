//! Reading where a connection points from its connection string, by an
//! allow-list: only properties this file knows (provider, driver, server,
//! database, file, URL, DSN, Power Query location) become fields, and only
//! when their values parse as that kind of field. Every other property is
//! left out; secret ones only say that they are there ([`Auth`]).

use super::safe;
use crate::mask::{is_secret_key, pairs};
use crate::report::{Auth, Target};

/// What a connection string says, ready to show.
#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub target: Target,
    pub auth: Vec<Auth>,
    /// The connection string's provider or driver, lower-cased, to decide
    /// the kind.
    pub provider: String,
    /// It names `$Embedded$` (the data model).
    pub embedded: bool,
}

/// Fills the empty fields of `parsed` from a raw connection string.
pub(crate) fn parse(raw: &str) -> Parsed {
    let mut parsed = Parsed::default();
    fill(&mut parsed, raw, 0);
    parsed.auth.sort_unstable();
    parsed.auth.dedup();
    parsed
}

fn fill(parsed: &mut Parsed, raw: &str, depth: u32) {
    let pairs = pairs(raw);
    if parsed.provider.is_empty() {
        parsed.provider = pairs
            .iter()
            .find(|(key, _)| matches!(normalize(key).as_str(), "provider" | "driver"))
            .map(|(_, value)| value.to_ascii_lowercase())
            .unwrap_or_default();
    }
    let mashup = parsed.provider.contains("mashup");
    for (key, value) in &pairs {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let key = normalize(key);
        if value.eq_ignore_ascii_case("$embedded$") {
            parsed.embedded = true;
        }
        if let Some(auth) = auth_of(&key, value) {
            parsed.auth.push(auth);
            continue;
        }
        let t = &mut parsed.target;
        match key.as_str() {
            "provider" | "driver" => set(&mut t.provider, safe::name(value)),
            "dsn" => set(&mut t.data_source_name, safe::name(value)),
            "server" | "servername" | "address" | "addr" | "networkaddress" | "host"
            | "hostname" => set_server(t, value),
            "port" => {
                if t.port.is_none() {
                    t.port = value.parse().ok();
                }
            }
            "initialcatalog" | "database" | "catalog" | "dbname" => {
                set(&mut t.database, safe::name(value));
            }
            // Oracle's ODBC driver puts the address, or `user/password@tns`,
            // in DBQ: never a file there.
            "dbq" if parsed.provider.contains("oracle") => set_server(t, value),
            "dbq" | "defaultdir" | "sourcedb" => {
                safe::set_location(t, value);
            }
            "url" => {
                safe::set_url(t, value);
            }
            "location" if mashup => set(&mut t.query, safe::name(value)),
            "location" => set_server(t, value),
            "datasource" if mashup => {}
            "datasource" => data_source(t, value, &parsed.provider),
            "extendedproperties" if depth < 2 && value.contains('=') => {
                fill(parsed, value, depth + 1);
            }
            _ => {}
        }
    }
}

fn set(slot: &mut Option<String>, value: Option<String>) {
    if slot.is_none() {
        *slot = value;
    }
}

fn set_server(t: &mut Target, value: &str) {
    if t.server.is_some() {
        return;
    }
    if value.contains("://") {
        safe::set_url(t, value);
    } else if let Some(server) = safe::server(value) {
        t.server = Some(server.host);
        t.port = t.port.or(server.port);
    } else if let Some((server, service)) = safe::oracle(value) {
        t.server = Some(server.host);
        t.port = t.port.or(server.port);
        set(&mut t.database, service);
    }
}

/// What a secret or sign-in property says about the sign-in, if it is one.
fn auth_of(key: &str, value: &str) -> Option<Auth> {
    let yes = matches!(
        value.to_ascii_lowercase().as_str(),
        "sspi" | "true" | "yes" | "1"
    );
    match key {
        "integratedsecurity" | "trustedconnection" if yes => Some(Auth::Integrated),
        "integratedsecurity" | "trustedconnection" => None,
        "uid" | "userid" | "user" | "username" | "uname" => Some(Auth::User),
        _ if !is_secret_key(key) => None,
        _ if key.contains("token") || key.contains("jwt") => Some(Auth::Token),
        _ if key.ends_with("key") || key.contains("signature") || key == "sig" => Some(Auth::Key),
        _ if key.contains("pass") || key.contains("pwd") || key == "pw" => Some(Auth::Password),
        _ => Some(Auth::OtherSecret),
    }
}

/// `Data Source` means a server, a file or a URL depending on the provider
/// and the value.
fn data_source(t: &mut Target, value: &str, provider: &str) {
    if value.contains("://") {
        safe::set_url(t, value);
        return;
    }
    if value.starts_with('(') {
        set_server(t, value);
        return;
    }
    let file_provider = ["jet", "ace", "sqlite", "text", "excel"]
        .iter()
        .any(|p| provider.contains(p));
    if file_provider || looks_like_path(value) {
        safe::set_file(t, value);
    } else {
        set_server(t, value);
    }
}

fn looks_like_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    let drive = bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    let lower = value.to_ascii_lowercase();
    drive
        || value.starts_with("\\\\")
        || [
            ".accdb", ".mdb", ".xlsx", ".xlsm", ".xlsb", ".xls", ".csv", ".txt", ".db", ".sqlite",
            ".cub",
        ]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// The normalized name of a connection string key.
pub(crate) fn normalize(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::report::Auth;

    #[test]
    fn common_strings() {
        let p = parse(
            "Provider=SQLOLEDB.1;Data Source=tcp:sql01,1433;Initial Catalog=Sales;User ID=u;Password=x",
        );
        assert_eq!(p.target.server.as_deref(), Some("sql01"));
        assert_eq!(p.target.port, Some(1433));
        assert_eq!(p.target.database.as_deref(), Some("Sales"));
        assert_eq!(p.target.provider.as_deref(), Some("SQLOLEDB.1"));
        assert_eq!(p.auth, [Auth::User, Auth::Password]);

        let p = parse("Provider=Microsoft.ACE.OLEDB.12.0;Data Source=C:\\data\\db.accdb");
        let file = p.target.file.unwrap();
        assert_eq!(file.path, "C:\\data\\db.accdb");
        assert_eq!(file.file_name.as_deref(), Some("db.accdb"));

        let p = parse(
            "Provider=OraOLEDB.Oracle;Data Source=(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=ora01.corp.example)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=FIN)));User ID=u;Password=p",
        );
        assert_eq!(p.target.server.as_deref(), Some("ora01.corp.example"));
        assert_eq!(p.target.port, Some(1521));
        assert_eq!(p.target.database.as_deref(), Some("FIN"));
        let p = parse("Provider=OraOLEDB.Oracle;Data Source=ora02:1522/sales");
        assert_eq!(p.target.server.as_deref(), Some("ora02"));
        assert_eq!(p.target.database.as_deref(), Some("sales"));

        // Oracle `user/password` forms reach no field.
        for raw in [
            "Provider=OraOLEDB.Oracle;Data Source=scott/tiger",
            "Provider=OraOLEDB.Oracle;Data Source=scott/tiger@orcl",
            "Driver={Oracle in OraClient19Home1};DBQ=scott/tiger@orcl",
        ] {
            let p = parse(raw);
            assert!(p.target.server.is_none(), "{raw}");
            assert!(p.target.database.is_none(), "{raw}");
            assert!(p.target.file.is_none(), "{raw}");
            assert!(!format!("{:?}", p.target).contains("tiger"), "{raw}");
        }
        let p = parse("Driver={Oracle in OraClient19Home1};DBQ=//ora04:1521/fin");
        assert_eq!(p.target.server.as_deref(), Some("ora04"));
        assert_eq!(p.target.database.as_deref(), Some("fin"));

        let p = parse("DRIVER={SQL Server};SERVER=s;DATABASE=d;DSN=x;Trusted_Connection=yes");
        assert_eq!(p.target.provider.as_deref(), Some("SQL Server"));
        assert_eq!(p.target.data_source_name.as_deref(), Some("x"));
        assert_eq!(p.auth, [Auth::Integrated]);

        let p =
            parse("Provider=Microsoft.Mashup.OleDb.1;Data Source=$Workbook$;Location=\"Sales Q1\"");
        assert_eq!(p.target.query.as_deref(), Some("Sales Q1"));
        assert_eq!(p.target.server, None);

        let p = parse("Provider=MSDASQL.1;Extended Properties=\"DSN=Warehouse;UID=u;PWD=p\"");
        assert_eq!(p.target.data_source_name.as_deref(), Some("Warehouse"));
        assert_eq!(p.auth, [Auth::User, Auth::Password]);

        let p = parse("Data Source=\"https://u:p@host/x?sig=abc\";AccountKey=k");
        assert_eq!(p.target.url.as_deref(), Some("https://host/x"));
        assert_eq!(p.auth, [Auth::Key]);

        // A value that is not a server is not shown.
        let p = parse("Server=not a host;Database=a=b");
        assert_eq!(p.target.server, None);
        assert_eq!(p.target.database, None);
    }
}
