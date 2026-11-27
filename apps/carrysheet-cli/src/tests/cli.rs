use crate::cli::*;
use crate::files::read_bounded;
use carrysheet::commands::table::{ArgType, COMMANDS};
use carrysheet::commands::{CommandError, ErrorCode};
use carrysheet::open::ReadFailure;
use serde_json::{Map, Value};

#[test]
fn clap_definition_is_consistent() {
    build().debug_assert();
}

#[test]
fn every_table_command_is_reachable() {
    for c in COMMANDS {
        let mut argv = vec![BIN.to_owned()];
        argv.extend(c.cli_path.iter().map(|s| s.to_string()));
        for a in c.all_args().into_iter().filter(|a| a.required) {
            let value = match a.ty {
                ArgType::Enum(vs) => vs[0].to_owned(),
                ArgType::ObjectList(_) => "[]".to_owned(),
                ArgType::Integer { min, .. } => min.to_string(),
                _ => "x".to_owned(),
            };
            if !a.positional {
                argv.push(format!("--{}", flag_name(a)));
            }
            argv.push(value);
        }
        let m = build().try_get_matches_from(&argv).unwrap();
        let (found, sub) = chosen(&m).unwrap();
        assert_eq!(found.name, c.name);
        let json = args_json(found, sub).unwrap();
        assert_eq!(json.len(), c.all_args().len());
    }
}

#[test]
fn flags_are_kebab_case() {
    let m = build()
        .try_get_matches_from([
            BIN,
            "read",
            "a.xlsx",
            "A1:B2",
            "--max-rows",
            "5",
            "--mode",
            "both",
        ])
        .unwrap();
    let (c, sub) = chosen(&m).unwrap();
    let j = args_json(c, sub).unwrap();
    assert_eq!(j["max_rows"], 5);
    assert_eq!(j["range"], "A1:B2");
    assert_eq!(j["mode"], "both");
    assert_eq!(j["cursor"], Value::Null);
}

#[test]
fn dry_run_only_where_files_change() {
    assert!(
        build()
            .try_get_matches_from([BIN, "convert", "a.xlsx", "b.csv", "--dry-run"])
            .is_ok()
    );
    assert!(
        build()
            .try_get_matches_from([BIN, "read", "a.xlsx", "--dry-run"])
            .is_err()
    );
}

#[test]
fn global_flags_go_anywhere() {
    for argv in [
        vec![BIN, "--json", "commands"],
        vec![BIN, "commands", "--json"],
        vec![BIN, "mcp", "--json", "serve"],
        vec![BIN, "-y", "mcp", "serve", "--read-only"],
    ] {
        assert!(build().try_get_matches_from(&argv).is_ok(), "{argv:?}");
    }
}

#[test]
fn ops_json_is_parsed() {
    let m = build()
        .try_get_matches_from([
            BIN,
            "edit",
            "a.xlsx",
            "--ops",
            r#"[{"op":"clear","target":"A1"}]"#,
        ])
        .unwrap();
    let (c, sub) = chosen(&m).unwrap();
    assert_eq!(args_json(c, sub).unwrap()["ops"][0]["op"], "clear");

    let m = build()
        .try_get_matches_from([BIN, "edit", "a.xlsx", "--ops", "[{"])
        .unwrap();
    let (c, sub) = chosen(&m).unwrap();
    let e = args_json(c, sub).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(e.why.contains("not valid JSON"));

    let m = build()
        .try_get_matches_from([BIN, "edit", "a.xlsx", "--ops", "@/no/such/ops.json"])
        .unwrap();
    let (c, sub) = chosen(&m).unwrap();
    assert_eq!(args_json(c, sub).unwrap_err().code, ErrorCode::FileNotFound);
}

/// A fresh folder for one test, removed when dropped.
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let dir =
            std::env::temp_dir().join(format!("carrysheet-cli-unit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ops_from(at: &str) -> Result<Map<String, Value>, CommandError> {
    let m = build()
        .try_get_matches_from([BIN, "edit", "a.xlsx", "--ops", at])
        .unwrap();
    let (c, sub) = chosen(&m).unwrap();
    args_json(c, sub)
}

#[test]
fn ops_files_are_read_with_a_limit() {
    let dir = TempDir::new("ops-limit");
    let file = dir.0.join("ops.json");
    std::fs::write(&file, br#"[{"op":"clear","target":"A1"}]"#).unwrap();
    let ok = ops_from(&format!("@{}", file.display())).unwrap();
    assert_eq!(ok["ops"][0]["op"], "clear");

    // The limit holds whatever size the file claims.
    let len = std::fs::metadata(&file).unwrap().len();
    assert_eq!(read_bounded(&file, len).unwrap().len() as u64, len);
    assert!(matches!(
        read_bounded(&file, len - 1),
        Err(ReadFailure::TooLarge)
    ));
    assert!(matches!(read_bounded(&file, 0), Err(ReadFailure::TooLarge)));

    // A folder is not a file.
    let e = ops_from(&format!("@{}", dir.0.display())).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(e.why.contains("not a regular file"), "{}", e.why);

    // Not UTF-8.
    std::fs::write(&file, b"[\xff]").unwrap();
    let e = ops_from(&format!("@{}", file.display())).unwrap_err();
    assert!(e.why.contains("UTF-8"), "{}", e.why);
}

#[cfg(unix)]
#[test]
fn endless_devices_and_pipes_are_refused_without_waiting() {
    // /dev/zero reports size 0 and never ends.
    let e = ops_from("@/dev/zero").unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(e.why.contains("not a regular file"), "{}", e.why);

    // Opening a pipe with no writer would wait forever.
    let dir = TempDir::new("ops-fifo");
    let fifo = dir.0.join("ops.fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|s| s.success());
    if made {
        let e = ops_from(&format!("@{}", fifo.display())).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidArgument);
        assert!(e.why.contains("not a regular file"), "{}", e.why);
    } else {
        eprintln!("mkfifo is not available; skipped the pipe case");
    }
}
