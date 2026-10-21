//! Connections, what uses them, and the data model.

mod common;

use carrysheet_connections::{
    Auth, CommandType, Connection, ConnectionKind, Credentials, Limits, Options, Report, UseKind,
    inspect_with,
};
use common::{fixture, report_of};

/// The report with raw texts, masked as well as patterns can.
fn raw_report() -> Report {
    let options = Options { include_raw: true };
    inspect_with(&fixture("connections.parts"), &Limits::default(), &options)
}

/// What a connection leaves out.
fn hidden(c: &Connection) -> Vec<&str> {
    c.hidden.iter().map(|h| h.what.as_str()).collect()
}

fn by_id(report: &Report, id: u32) -> &Connection {
    report
        .connections
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no connection {id}"))
}

#[test]
fn every_kind_is_recognized() {
    let report = report_of("connections.parts");
    assert!(report.problems.is_empty(), "{:#?}", report.problems);
    let kinds: Vec<(u32, ConnectionKind)> =
        report.connections.iter().map(|c| (c.id, c.kind)).collect();
    assert_eq!(
        kinds,
        [
            (1, ConnectionKind::Odbc),
            (2, ConnectionKind::OleDb),
            (3, ConnectionKind::Dao),
            (4, ConnectionKind::WebQuery),
            (5, ConnectionKind::TextFile),
            (6, ConnectionKind::Ado),
            (7, ConnectionKind::Olap),
            (8, ConnectionKind::DataModel),
            (9, ConnectionKind::PowerQuery),
            (10, ConnectionKind::Worksheet),
            (11, ConnectionKind::FileDatabase),
        ]
    );
}

#[test]
fn odbc_target_refresh_and_masking() {
    let report = report_of("connections.parts");
    let odbc = by_id(&report, 1);
    assert_eq!(odbc.name, "Warehouse ODBC");
    assert_eq!(odbc.target.data_source_name.as_deref(), Some("Warehouse"));
    assert_eq!(odbc.target.server.as_deref(), Some("wh01.corp.example"));
    assert_eq!(odbc.target.database.as_deref(), Some("Stock"));
    assert_eq!(
        odbc.target.provider.as_deref(),
        Some("ODBC Driver 18 for SQL Server")
    );
    assert_eq!(odbc.auth, [Auth::User, Auth::Password]);
    // The SQL mentions a password, so all of it is hidden, not part of it.
    assert_eq!(odbc.command_text, None);
    assert_eq!(
        hidden(odbc),
        ["SQL command", "connection string", "description"]
    );
    let sql = &odbc.hidden[0];
    assert_eq!(sql.characters, 63);
    assert_eq!(
        sql.note,
        "SQL command: hidden (63 characters) because a quoted value in it mentions a password or key. To see it: set Options::include_raw; the text is then masked as well as patterns can, and may still contain secrets."
    );
    assert_eq!(odbc.command_type, Some(CommandType::Sql));
    assert!(odbc.raw.is_none());

    let report = raw_report();
    let raw = by_id(&report, 1).raw.as_ref().expect("raw texts");
    assert_eq!(raw.description.as_deref(), Some("Nightly stock"));
    assert_eq!(
        raw.connection_string.as_deref(),
        Some(
            "DSN=Warehouse;DRIVER={ODBC Driver 18 for SQL Server};SERVER=wh01.corp.example;DATABASE=Stock;UID=reporter;PWD=***;APP=Microsoft Office"
        )
    );
    assert_eq!(
        raw.command_text.as_deref(),
        Some("SELECT * FROM stock WHERE note <> 'password = ''***'''")
    );
    assert_eq!(report.warnings.len(), 1);
    let refresh = &odbc.refresh;
    assert!(refresh.on_load && refresh.background && refresh.save_data && refresh.save_password);
    assert_eq!(refresh.interval_minutes, Some(15));
    assert_eq!(refresh.credentials, Credentials::Stored);
    assert!(!refresh.keep_alive);
}

#[test]
fn ole_db_quoted_and_nested_secrets() {
    let report = report_of("connections.parts");
    let ole = by_id(&report, 2);
    assert_eq!(ole.auth, [Auth::User, Auth::Password, Auth::Token]);
    assert_eq!(
        ole.target.table.as_deref(),
        Some("\"Sales\".\"dbo\".\"Orders\"")
    );
    assert_eq!(hidden(ole), ["connection string"]);
    let report = raw_report();
    assert_eq!(
        by_id(&report, 2)
            .raw
            .as_ref()
            .unwrap()
            .connection_string
            .as_deref(),
        Some(
            "Provider=SQLOLEDB.1;Persist Security Info=True;User ID=sa;Password=***;Initial Catalog=Sales;Data Source=sql01.corp.example;Extended Properties=\"AccessToken=***\""
        )
    );
    assert_eq!(ole.target.server.as_deref(), Some("sql01.corp.example"));
    assert_eq!(ole.target.database.as_deref(), Some("Sales"));
    assert_eq!(ole.command_type, Some(CommandType::Table));
    assert_eq!(ole.refresh.credentials, Credentials::None);
    assert!(ole.refresh.keep_alive && !ole.refresh.on_load);
}

#[test]
fn files_web_text_and_others() {
    let report = report_of("connections.parts");

    let access = by_id(&report, 3);
    let file = access.target.file.as_ref().unwrap();
    assert_eq!(file.path, "C:\\Data\\archive.accdb");
    assert_eq!(file.drive.as_deref(), Some("C:"));
    assert_eq!(file.folder.as_deref(), Some("\\Data"));
    assert_eq!(file.file_name.as_deref(), Some("archive.accdb"));
    assert_eq!(access.auth, [Auth::Password]);
    assert_eq!(access.target.table.as_deref(), Some("Orders"));
    assert_eq!(access.refresh.credentials, Credentials::Prompt);

    let web = by_id(&report, 4);
    assert_eq!(
        web.target.url.as_deref(),
        Some("https://rates.example.com/daily")
    );
    assert_eq!(web.command_text, None);
    assert_eq!(hidden(web), ["POST data", "full URL"]);
    assert_eq!(web.command_type, Some(CommandType::Post));
    assert!(web.notes.iter().any(|n| n.contains("XML")));

    let text = by_id(&report, 5);
    let file = text.target.file.as_ref().unwrap();
    assert_eq!(file.path, "\\\\files\\share\\prices.csv");
    assert_eq!(file.host.as_deref(), Some("files"));
    assert_eq!(file.share.as_deref(), Some("share"));
    assert_eq!(file.file_name.as_deref(), Some("prices.csv"));
    assert!(text.refresh.on_load);
    assert!(text.notes.is_empty(), "{:?}", text.notes);

    let cube = by_id(&report, 7);
    assert_eq!(cube.target.server.as_deref(), Some("olap01"));
    assert_eq!(cube.target.database.as_deref(), Some("Finance"));
    assert_eq!(cube.command_type, Some(CommandType::Cube));

    let query = by_id(&report, 9);
    assert_eq!(query.target.query.as_deref(), Some("Sales"));
    assert_eq!(query.target.server, None);
    // A plain SELECT is shown whole.
    assert_eq!(query.command_text.as_deref(), Some("SELECT * FROM [Sales]"));
    assert!(!hidden(query).contains(&"SQL command"));

    let worksheet = by_id(&report, 10);
    assert_eq!(worksheet.type_number, Some(102));
    assert_eq!(worksheet.target.table.as_deref(), Some("Regions"));
    assert!(worksheet.notes.iter().any(|n| n.contains("Refresh All")));

    let feed = by_id(&report, 11);
    assert_eq!(
        feed.target.file.as_ref().unwrap().path,
        "D:\\feeds\\feed.mdb"
    );
    let odc = feed
        .connection_file
        .as_ref()
        .unwrap()
        .file
        .as_ref()
        .unwrap();
    assert_eq!(odc.file_name.as_deref(), Some("feed.odc"));
    assert_eq!(odc.host.as_deref(), Some("files"));
    assert_eq!(feed.auth, [Auth::Password]);
    assert!(feed.notes.iter().any(|n| n.contains("deleted")));
    assert!(feed.notes.iter().any(|n| n.contains("1 parameters")));
}

#[test]
fn query_tables_tables_and_pivot_caches() {
    let report = report_of("connections.parts");

    let odbc_uses = &by_id(&report, 1).used_by;
    assert_eq!(odbc_uses.len(), 1);
    let sheet_query = &odbc_uses[0];
    assert_eq!(sheet_query.kind, UseKind::QueryTable);
    assert_eq!(sheet_query.name.as_deref(), Some("ExternalData_1"));
    assert_eq!(sheet_query.sheet.as_deref(), Some("Stock & Prices"));
    assert_eq!(
        sheet_query.range.as_deref(),
        Some("'Stock & Prices'!$A$1:$C$40")
    );
    assert_eq!(sheet_query.part, "xl/queryTables/queryTable2.xml");
    let settings = sheet_query.query_table.as_ref().unwrap();
    assert!(
        settings.refresh_disabled && settings.remove_data_on_save && settings.background_refresh
    );

    let table_query = &by_id(&report, 2).used_by[0];
    assert_eq!(table_query.kind, UseKind::QueryTable);
    assert_eq!(table_query.sheet.as_deref(), Some("Orders"));
    assert_eq!(table_query.table.as_deref(), Some("Orders_Table"));
    assert_eq!(table_query.range.as_deref(), Some("Orders!A1:F120"));
    let settings = table_query.query_table.as_ref().unwrap();
    assert!(settings.refresh_on_load && !settings.background_refresh);

    let pivot = &by_id(&report, 7).used_by[0];
    assert_eq!(pivot.kind, UseKind::PivotCache);
    assert_eq!(pivot.name.as_deref(), Some("pivot cache 7"));

    let table = &by_id(&report, 10).used_by[0];
    assert_eq!(table.kind, UseKind::Table);
    assert_eq!(table.range.as_deref(), Some("Orders!H1:I9"));
}

#[test]
fn data_model_is_found_by_relationship() {
    let report = report_of("connections.parts");
    let model = report.data_model.as_ref().unwrap();
    assert_eq!(model.part, "xl/model/item.data");
    assert_eq!(model.size_bytes, Some(40));
}

#[test]
fn standard_names_without_relationships() {
    let report = report_of("fallback.parts");
    assert_eq!(report.connections.len(), 1);
    let lone = &report.connections[0];
    assert_eq!(lone.kind, ConnectionKind::Odbc);
    assert_eq!(lone.target.server.as_deref(), Some("pg.example"));
    assert!(lone.auth.contains(&Auth::Password), "{:?}", lone.auth);
    assert_eq!(hidden(lone), ["connection string"]);
    assert_eq!(lone.used_by.len(), 1);
    assert_eq!(lone.used_by[0].name.as_deref(), Some("Lone_1"));
    assert_eq!(lone.used_by[0].sheet, None);

    assert_eq!(report.external_links.len(), 1);
    assert_eq!(
        report.data_model.as_ref().unwrap().part,
        "xl/model/item.data"
    );

    // The query table that names a missing connection is a problem.
    assert_eq!(report.problems.len(), 1, "{:#?}", report.problems);
    assert!(report.problems[0].what.contains("connection 99"));
    assert_eq!(
        report.problems[0].part.as_deref(),
        Some("xl/queryTables/queryTable2.xml")
    );
}

#[test]
fn report_serializes_with_whole_word_fields() {
    let report = report_of("connections.parts");
    let json = serde_json::to_value(&report).unwrap();
    let first = &json["connections"][0];
    assert_eq!(first["kind"], "odbc");
    assert_eq!(first["refresh"]["credentials"], "stored");
    assert_eq!(first["used_by"][0]["kind"], "query_table");
    assert!(
        first["target"].get("url").is_none(),
        "absent fields are left out"
    );
}

#[test]
fn command_type_defaults_to_sql_in_db_pr() {
    let mut parts = carrysheet_connections::MemoryParts::new();
    parts.insert(
        "xl/connections.xml",
        r#"<connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<connection id="1" name="a" type="1"><dbPr connection="DSN=x" command="select 1"/></connection>
</connections>"#,
    );
    let report =
        carrysheet_connections::inspect(&parts, &carrysheet_connections::Limits::default());
    assert_eq!(report.connections[0].command_type, Some(CommandType::Sql));
    // A debug print of the parts shows names and sizes, never the bytes.
    let debug = format!("{parts:?}");
    assert!(
        debug.contains("xl/connections.xml") && !debug.contains("DSN=x"),
        "{debug}"
    );
}

/// Builds a workbook with one ODBC connection running `sql`.
fn sql_connection(sql: &str) -> carrysheet_connections::MemoryParts {
    let mut parts = carrysheet_connections::MemoryParts::new();
    let sql = sql
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;");
    parts.insert(
        "xl/connections.xml",
        format!(
            r#"<connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<connection id="1" name="a" type="1"><dbPr connection="DSN=x" command="{sql}" commandType="2"/></connection>
</connections>"#
        ),
    );
    parts
}

#[test]
fn plain_sql_is_shown_whole() {
    let sql = "SELECT region, SUM(amount) AS total FROM sales WHERE year >= 2024 AND note <> 'n/a' GROUP BY region";
    let report = carrysheet_connections::inspect(&sql_connection(sql), &Limits::default());
    let c = &report.connections[0];
    assert_eq!(c.command_text.as_deref(), Some(sql));
    assert_eq!(hidden(c), ["connection string"]);
}

#[test]
fn a_secret_anywhere_in_sql_hides_all_of_it() {
    for sql in [
        "ALTER LOGIN app WITH PASSWORD = N'Zq9;xK2#vB7'",
        "CREATE USER app IDENTIFIED BY \"Zq9xK2vB7\"",
        "EXEC sp_password NULL, 'Zq9xK2vB7', 'app'",
        "SELECT * FROM OPENROWSET('SQLNCLI', 'Server=s;UID=app;PWD=Zq9xK2vB7', 'SELECT 1')",
        "SELECT * FROM t -- https://app:Zq9xK2vB7@db.example/",
        "SELECT * FROM t WHERE token = 'Zq9xK2vB7mW4nP8rT6yH3jL5'",
        "SELECT 1 /* api_key Zq9xK2vB7 */",
    ] {
        let report = carrysheet_connections::inspect(&sql_connection(sql), &Limits::default());
        let c = &report.connections[0];
        assert_eq!(c.command_text, None, "{sql}");
        assert_eq!(c.hidden[0].what, "SQL command", "{sql}");
        assert_eq!(c.hidden[0].characters, sql.chars().count(), "{sql}");
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("Zq9"), "{sql}: {json}");
    }
}

/// Builds a workbook with one connection made of `inner` XML.
fn one_connection(attributes: &str, inner: &str) -> carrysheet_connections::MemoryParts {
    let mut parts = carrysheet_connections::MemoryParts::new();
    parts.insert(
        "xl/connections.xml",
        format!(
            r#"<connections xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><connection id="1" name="a" {attributes}>{inner}</connection></connections>"#
        ),
    );
    parts
}

#[test]
fn sql_that_is_not_a_plain_read_is_hidden_whole() {
    for (sql, why) in [
        ("EXEC dbo.LoadSales @year = 2024", "procedure"),
        ("SELECT * FROM t -- by region", "comments"),
        ("SELECT 'a' + name FROM t", "joins strings"),
        ("SELECT a FROM t; DROP TABLE t", "more than one statement"),
        ("DELETE FROM t", "does more than read"),
        ("VALUES (1)", "not a plain SELECT"),
    ] {
        let report = carrysheet_connections::inspect(&sql_connection(sql), &Limits::default());
        let c = &report.connections[0];
        assert_eq!(c.command_text, None, "{sql}");
        assert_eq!(c.hidden[0].what, "SQL command");
        assert!(
            c.hidden[0].note.contains(why),
            "{sql}: {}",
            c.hidden[0].note
        );
    }
}

#[test]
fn plain_names_urls_and_oracle_hosts_are_not_over_hidden() {
    for sql in [
        "SELECT CustomerKey, ProductKey, SUM(SalesAmount) FROM FactSales GROUP BY CustomerKey, ProductKey",
        "SELECT name, 'https://www.example.com/products/list' AS link FROM shops",
    ] {
        let report = carrysheet_connections::inspect(&sql_connection(sql), &Limits::default());
        assert_eq!(report.connections[0].command_text.as_deref(), Some(sql));
    }
    let parts = one_connection(
        r#"type="5""#,
        r#"<dbPr connection="Provider=OraOLEDB.Oracle;Data Source=(DESCRIPTION=(ADDRESS=(PROTOCOL=TCP)(HOST=ora01.corp.example)(PORT=1521))(CONNECT_DATA=(SERVICE_NAME=FIN)));User ID=app;Password=p" command="FACT_SALES" commandType="3"/>"#,
    );
    let report = carrysheet_connections::inspect(&parts, &Limits::default());
    let t = &report.connections[0].target;
    assert_eq!(t.server.as_deref(), Some("ora01.corp.example"));
    assert_eq!(t.port, Some(1521));
    assert_eq!(t.database.as_deref(), Some("FIN"));
}

#[test]
fn credential_url_paths_are_hidden_whole_and_the_host_kept() {
    for url in [
        "https://www.dropbox.com/s/q1w2e3r4t5/rates.csv?dl=1",
        "https://app.example.com/export;jsessionid=Q1W2E3R4T5Y6/rates.csv",
        "https://files.example.com/dl/aB3dE5gH7jK9mN1pQ3sT5vW7/rates.csv",
    ] {
        let parts = one_connection(r#"type="4""#, &format!(r#"<webPr url="{url}"/>"#));
        let report = carrysheet_connections::inspect(&parts, &Limits::default());
        let t = &report.connections[0].target;
        let host_only = t.url.as_deref().unwrap();
        assert_eq!(host_only.matches('/').count(), 2, "{url}: {host_only}");
        let path = t.hidden_path.as_ref().expect("the path is hidden");
        assert_eq!(path.what, "URL path");
        assert!(path.note.starts_with("URL path: hidden ("), "{}", path.note);
    }
    let parts = one_connection(
        r#"type="4""#,
        r#"<webPr url="https://rates.example.com/v1/daily/rates.csv"/>"#,
    );
    let report = carrysheet_connections::inspect(&parts, &Limits::default());
    let t = &report.connections[0].target;
    assert_eq!(
        t.url.as_deref(),
        Some("https://rates.example.com/v1/daily/rates.csv")
    );
    assert_eq!(t.hidden_path, None);
}

/// Review round 4: password comparisons in common forms. Each must be hidden
/// whole, with nothing of the value in the report.
#[test]
fn password_comparisons_are_hidden_in_every_form() {
    for sql in [
        "SELECT * FROM users WHERE [Password]=\"Zq9v7K\"",
        "SELECT * FROM users WHERE 'Zq9v7K' = password",
        "SELECT * FROM users WHERE md5('Zq9v7K') = password",
        "SELECT * FROM users WHERE password = LOWER('Zq9v7K')",
        "SELECT * FROM users WHERE CAST(password AS varchar(20)) = 'Zq9v7K'",
        "SELECT * FROM users WHERE password = 'Zq9v7K' COLLATE Latin1_General_CS_AS",
        "SELECT * FROM users WHERE password BETWEEN 'Zq9v7K' AND 'zz'",
        "SELECT * FROM users WHERE password ILIKE 'Zq9v7K'",
        "SELECT * FROM users WHERE password SIMILAR TO 'Zq9v7K'",
        "SELECT * FROM users WHERE password IS NOT DISTINCT FROM 'Zq9v7K'",
        "SELECT * FROM users WHERE password = ANY(ARRAY['Zq9v7K'])",
        "SELECT * FROM users WHERE password IN (SELECT 'Zq9v7K')",
        "SELECT * FROM users WHERE password = _utf8mb4'Zq9v7K'",
        "SELECT * FROM users WHERE password = q'[Zq9v7K]'",
        "SELECT * FROM users WHERE password = U&'Zq9v7K'",
        "SELECT 'Zq9v7K' AS pwd",
        "SELECT * FROM t WHERE x = 'Zq9v7K' AND password = x",
        "SELECT * FROM t WHERE c = 'password Zq9v7K'",
        "SELECT a FROM t #password=Zq9v7K",
        "SELECT * FROM t WHERE a = 'x\\'' AND pwd = 'Zq9v7K' AND b = 'z\\''",
        "SELECT * FROM t WHERE passcode = 'Zq9v7K'",
        "SELECT * FROM t WHERE pin = 975318",
    ] {
        let report = carrysheet_connections::inspect(&sql_connection(sql), &Limits::default());
        let c = &report.connections[0];
        assert_eq!(c.command_text, None, "{sql}");
        assert_eq!(c.hidden[0].what, "SQL command", "{sql}");
        let json = serde_json::to_string(&report).unwrap();
        assert!(
            !json.contains("Zq9v7K") && !json.contains("975318"),
            "{sql}: {json}"
        );
    }
}

#[test]
fn plain_joins_and_ordinary_columns_are_shown() {
    for sql in [
        "SELECT FirstName + ' ' + LastName AS FullName FROM people",
        "SELECT * FROM t WHERE code = 'A1'",
        "SELECT * FROM t WHERE key = 5",
        "SELECT * FROM orders o WHERE o.SessionID = 42",
        "SELECT * FROM #temp",
        "SELECT * FROM t WHERE d > #2024-01-31#",
    ] {
        let report = carrysheet_connections::inspect(&sql_connection(sql), &Limits::default());
        assert_eq!(report.connections[0].command_text.as_deref(), Some(sql));
    }
}

#[test]
fn a_table_named_for_passwords_says_so() {
    let parts = one_connection(
        r#"type="5""#,
        r#"<dbPr connection="Provider=SQLOLEDB;Data Source=s" command="dbo.vw_PasswordResets" commandType="3"/>"#,
    );
    let report = carrysheet_connections::inspect(&parts, &Limits::default());
    let c = &report.connections[0];
    assert_eq!(c.target.table, None);
    assert_eq!(c.hidden[0].what, "table or cube name");
    assert!(
        c.hidden[0].note.contains("mentions a password or key"),
        "{}",
        c.hidden[0].note
    );
}

#[test]
fn source_files_that_are_not_paths_are_listed_as_hidden() {
    for path in [
        "\\\\fs01@SSL@443\\DavWWWRoot\\data.csv",
        "\\\\bob:Zq9v7K@fs01\\share\\x.csv",
    ] {
        let parts = one_connection(r#"type="6""#, &format!(r#"<textPr sourceFile="{path}"/>"#));
        let report = carrysheet_connections::inspect(&parts, &Limits::default());
        let c = &report.connections[0];
        assert_eq!(c.target.file, None, "{path}");
        assert_eq!(c.hidden[0].what, "source file", "{path}");
        assert_eq!(c.hidden[0].characters, path.chars().count());
        assert!(!serde_json::to_string(&report).unwrap().contains("Zq9v7K"));

        let options = Options { include_raw: true };
        let report = inspect_with(&parts, &Limits::default(), &options);
        let raw = report.connections[0].raw.as_ref().unwrap();
        assert!(raw.source_file.is_some(), "{path}");
    }
}
