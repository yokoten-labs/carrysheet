//! Power Query: finding the DataMashup and listing its queries.

mod common;

use carrysheet_connections::{Limits, Options, inspect, inspect_with};
use common::{Method, power_query_parts};

fn section() -> String {
    let path = format!("{}/tests/data/Section1.m", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(path).unwrap()
}

const SECRETS: [&str; 6] = [
    "m-sig-secret",
    "m-bearer-secret",
    "m-api-key-secret",
    "m-odbc-secret",
    "m-quoted",
    "secret\"",
];

#[test]
fn lists_queries_and_their_calls_without_formulas() {
    for method in [Method::Stored, Method::Deflated] {
        let parts = power_query_parts(&section(), method);
        let report = inspect(&parts, &Limits::default());
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let power_query = report.power_query.expect("a DataMashup");
        assert_eq!(power_query.part, "customXml/item1.xml");
        assert!(power_query.size_bytes > 0);
        assert!(power_query.package_bytes > 0);
        let names: Vec<&str> = power_query
            .queries
            .iter()
            .map(|q| q.name.as_str())
            .collect();
        assert_eq!(names, ["Sales", "Daily Rates", "Warehouse", "Legacy"]);

        for query in &power_query.queries {
            assert!(query.formula.is_none());
            let hidden = query.hidden.as_ref().expect("the formula is hidden");
            assert_eq!(hidden.what, "M formula");
            assert!(hidden.characters > 0);
            assert!(
                hidden.note.contains("To see it: set Options::include_raw"),
                "{}",
                hidden.note
            );
        }

        let sales = &power_query.queries[0].calls;
        assert_eq!(sales[0].function, "Sql.Database");
        assert_eq!(
            sales[0].target.server.as_deref(),
            Some("sql01.corp.example")
        );
        assert_eq!(sales[0].target.database.as_deref(), Some("Sales"));

        let rates: Vec<&str> = power_query.queries[1]
            .calls
            .iter()
            .map(|c| c.function.as_str())
            .collect();
        assert_eq!(rates, ["Json.Document", "Web.Contents"]);
        assert_eq!(
            power_query.queries[1].calls[1].target.url.as_deref(),
            Some("https://rates.example.com/v1/daily")
        );

        let warehouse = &power_query.queries[2].calls[0];
        assert_eq!(warehouse.function, "Odbc.Query");
        assert_eq!(
            warehouse.target.data_source_name.as_deref(),
            Some("Warehouse")
        );

        let legacy = &power_query.queries[3].calls[0];
        assert_eq!(legacy.function, "OleDb.DataSource");
        assert_eq!(legacy.target.server.as_deref(), Some("old01"));

        let json = serde_json::to_string(&power_query).unwrap();
        for secret in SECRETS {
            assert!(!json.contains(secret), "{secret} leaked: {json}");
        }
    }
}

#[test]
fn raw_formulas_are_masked_on_request() {
    let parts = power_query_parts(&section(), Method::Deflated);
    let options = Options { include_raw: true };
    let report = inspect_with(&parts, &Limits::default(), &options);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    let power_query = report.power_query.expect("a DataMashup");
    let formula = |i: usize| power_query.queries[i].formula.clone().unwrap();
    assert!(power_query.queries[0].hidden.is_none());

    let sales = formula(0);
    assert!(sales.starts_with("let"), "{sales}");
    assert!(sales.contains("Sql.Database(\"sql01.corp.example\", \"Sales\")"));

    let rates = formula(1);
    assert!(rates.contains("region=eu&sig=***"), "{rates}");
    assert!(rates.contains("Authorization=\"***\""), "{rates}");
    assert!(rates.contains("#\"x-api-key\"=\"***\""), "{rates}");
    assert!(formula(2).contains("pwd=***;"), "{}", formula(2));
    assert!(
        formula(3).contains("Password=\"\"***\"\""),
        "{}",
        formula(3)
    );

    let json = serde_json::to_string(&power_query).unwrap();
    for secret in SECRETS {
        assert!(!json.contains(secret), "{secret} leaked: {json}");
    }
}

#[test]
fn other_custom_xml_is_ignored() {
    let mut parts = carrysheet_connections::MemoryParts::new();
    parts.insert(
        "customXml/item1.xml",
        b"<b:Sources xmlns:b=\"x\"/>".to_vec(),
    );
    let report = inspect(&parts, &Limits::default());
    assert!(report.power_query.is_none());
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert!(report.is_empty());
}

#[test]
fn long_formulas_are_never_cut() {
    let long = format!(
        "section Section1;\nshared Big = \"{}\";",
        "x".repeat(50_000)
    );
    let parts = power_query_parts(&long, Method::Deflated);
    let options = Options { include_raw: true };
    let report = inspect_with(&parts, &Limits::default(), &options);
    // Even with raw texts asked for, a formula over the limit is hidden
    // whole: a cut could end inside a secret.
    let query = &report.power_query.unwrap().queries[0];
    assert_eq!(query.formula, None);
    let hidden = query.hidden.as_ref().unwrap();
    assert_eq!(hidden.characters, 50_002);
    assert!(
        hidden
            .note
            .contains("longer than Limits::max_formula_chars (16000)"),
        "{}",
        hidden.note
    );
}
