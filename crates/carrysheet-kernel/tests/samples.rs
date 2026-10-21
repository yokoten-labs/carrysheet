//! Apache POI samples with macros and charts: a save keeps them, or says
//! before saving that they would be lost (record 0003, point 3).

use std::io::Read as _;
use std::path::{Path, PathBuf};

use carrysheet_commands::engine::run;
use carrysheet_commands::{COMMANDS, CommandError, ErrorCode, validate_args};
use carrysheet_kernel::Kernel;
use serde_json::{Map, Value, json};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("carrysheet-samples-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn call(name: &str, args: Value) -> Result<Map<String, Value>, CommandError> {
    let command = COMMANDS.iter().find(|c| c.name == name).unwrap();
    let args = validate_args(command, &args)?;
    run::run(&Kernel::new(), command, &args)
}

fn p(path: &Path) -> String {
    path.display().to_string()
}

/// The part names of a saved package, lower case.
fn parts(path: &Path) -> Vec<String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..zip.len())
        .map(|i| {
            zip.by_index(i)
                .unwrap()
                .name()
                .unwrap()
                .to_ascii_lowercase()
        })
        .collect()
}

fn part_text(path: &Path, name: &str) -> String {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    let mut s = String::new();
    zip.by_name(name).unwrap().read_to_string(&mut s).unwrap();
    s
}

fn lost(done: &Map<String, Value>) -> Vec<String> {
    done["save"]["lost"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["feature"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn macros_are_kept_in_xlsm_and_reported_before_xlsx() {
    let dir = scratch("macro");
    let src = data("SimpleMacro.xlsm");
    let ops = json!([{ "op": "set_value", "target": "B2", "value": "checked" }]);

    // Edited as .xlsm, the VBA project comes back byte for byte.
    let out = dir.join("macro.xlsm");
    let done = call(
        "edit",
        json!({ "path": p(&src), "ops": ops, "output": p(&out) }),
    )
    .unwrap();
    assert_eq!(done["written"], true, "{done:?}");
    assert!(lost(&done).is_empty(), "{done:?}");
    assert!(parts(&out).contains(&"xl/vbaproject.bin".to_owned()));
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let mut vba = Vec::new();
    zip.by_name("xl/vbaProject.bin")
        .unwrap()
        .read_to_end(&mut vba)
        .unwrap();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&src).unwrap()).unwrap();
    let mut original = Vec::new();
    zip.by_name("xl/vbaProject.bin")
        .unwrap()
        .read_to_end(&mut original)
        .unwrap();
    assert_eq!(vba, original);

    // As .xlsx the macros cannot stay: refused before writing, with the list.
    let out = dir.join("macro.xlsx");
    let e = call("convert", json!({ "path": p(&src), "output": p(&out) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::ConfirmationRequired);
    assert!(e.what.contains("VBA project"), "{}", e.what);
    assert!(e.fix.contains(".xlsm"), "{}", e.fix);
    assert!(!out.exists());

    // dry_run shows the same report without writing.
    let dry = call(
        "convert",
        json!({ "path": p(&src), "output": p(&out), "dry_run": true }),
    )
    .unwrap();
    assert_eq!(dry["written"], false);
    assert!(lost(&dry).contains(&"VBA project".to_owned()));

    // confirm (for overwriting) does not accept a loss: that has its own
    // confirm_loss, so one yes never covers both (review M1).
    let e = call(
        "convert",
        json!({ "path": p(&src), "output": p(&out), "confirm": true }),
    )
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::ConfirmationRequired);
    assert!(e.fix.contains("confirm_loss"), "{}", e.fix);
    assert!(!out.exists());

    // With confirm_loss it saves and says what was lost.
    let done = call(
        "convert",
        json!({ "path": p(&src), "output": p(&out), "confirm_loss": true }),
    )
    .unwrap();
    assert_eq!(done["written"], true);
    assert!(lost(&done).contains(&"VBA project".to_owned()));
    assert!(!parts(&out).contains(&"xl/vbaproject.bin".to_owned()));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn charts_are_kept_or_reported() {
    let dir = scratch("charts");
    let src = data("WithTwoCharts.xlsx");
    let before = call(
        "read",
        json!({ "path": p(&src), "range": "E1", "mode": "both" }),
    )
    .unwrap();
    assert_eq!(
        before["rows"][0]["cells"][0],
        json!({ "value": 25.0, "formula": "=D1*D1" })
    );

    let out = dir.join("charts.xlsx");
    let ops = json!([{ "op": "set_value", "target": "D1", "value": 7 }]);
    let result = call(
        "edit",
        json!({ "path": p(&src), "ops": ops, "output": p(&out) }),
    );
    let saved = match result {
        Ok(done) => done,
        // Refused: then it must name what would be lost, and write nothing.
        Err(e) => {
            assert_eq!(e.code, ErrorCode::ConfirmationRequired, "{e:?}");
            assert!(!out.exists());
            call(
                "edit",
                json!({ "path": p(&src), "ops": ops, "output": p(&out), "confirm_loss": true }),
            )
            .unwrap()
        }
    };
    assert_eq!(
        saved["recalculated"][0],
        json!({ "sheet": "Sheet1", "cell": "E1", "before": 25.0, "after": 49.0 })
    );
    let charts = parts(&out)
        .iter()
        .filter(|n| n.starts_with("xl/charts/chart") && n.ends_with(".xml"))
        .count();
    let lost = lost(&saved);
    assert!(
        charts == 2 || lost.contains(&"charts".to_owned()),
        "charts kept: {charts}; reported lost: {lost:?}"
    );
    // Each chart still points at its data.
    if charts == 2 {
        assert!(
            part_text(&out, "xl/charts/chart1.xml").contains("Sheet"),
            "chart lost its series"
        );
    }
    // The saved file reopens with the new values.
    let after = call("read", json!({ "path": p(&out), "range": "D1:E1" })).unwrap();
    assert_eq!(after["rows"][0]["cells"], json!([7.0, 49.0]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn csv_output_lists_what_it_cannot_hold() {
    let dir = scratch("csv");
    let src = data("WithTwoCharts.xlsx");
    let out = dir.join("values.csv");
    let e = call("convert", json!({ "path": p(&src), "output": p(&out) })).unwrap_err();
    assert_eq!(e.code, ErrorCode::ConfirmationRequired);
    for word in ["formulas", "charts"] {
        assert!(e.what.contains(word), "{word}: {}", e.what);
    }
    let done = call(
        "convert",
        json!({ "path": p(&src), "output": p(&out), "confirm_loss": true }),
    )
    .unwrap();
    assert_eq!(done["format"], "csv");
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(text.starts_with("1,10,,5,25"), "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}
