use crate::edit::*;
use crate::error::ErrorCode;
use serde_json::Value;
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
