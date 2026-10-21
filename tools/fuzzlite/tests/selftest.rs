//! The harness catches what it is for: the hidden selftest targets panic,
//! spin, fill memory or run slow on purpose, and each must come back as a
//! minimised finding of the right kind, within the limits.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const EXE: &str = env!("CARGO_BIN_EXE_fuzzlite");

/// A fresh output folder outside the repository.
fn out_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fuzzlite-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Runs fuzzlite and returns (exit code, stdout lines, time taken).
fn fuzzlite(args: &[&str], out: &Path) -> (Option<i32>, Vec<String>, Duration) {
    let start = Instant::now();
    let o = Command::new(EXE)
        .args(args)
        .arg("--out")
        .arg(out)
        .output()
        .expect("fuzzlite runs");
    let lines = String::from_utf8_lossy(&o.stdout)
        .lines()
        .map(String::from)
        .collect();
    (o.status.code(), lines, start.elapsed())
}

fn field<'a>(line: &'a str, key: &str) -> &'a str {
    let at = line.find(&format!("\"{key}\":")).expect(key) + key.len() + 3;
    let rest = &line[at..];
    let end = rest.find([',', '}']).unwrap();
    rest[..end].trim_matches('"')
}

#[test]
fn a_panicking_target_is_caught_and_minimised() {
    let out = out_dir("panic");
    let (code, lines, took) = fuzzlite(
        &[
            "run",
            "--target",
            "selftest-panic",
            "--runs",
            "20",
            "--seed",
            "1",
        ],
        &out,
    );
    assert_eq!(code, Some(1), "{lines:?}");
    assert_eq!(lines.len(), 1, "one signature, one finding: {lines:?}");
    let line = &lines[0];
    assert_eq!(field(line, "kind"), "panic");
    assert!(line.contains("selftest-panic found"), "{line}");
    assert_eq!(field(line, "input_bytes"), "1", "{line}");
    let id = std::fs::read_dir(out.join("selftest-panic"))
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().ends_with(".orig.bin"))
        .expect("the original input is kept");
    let minimised = id.path().to_string_lossy().replace(".orig.bin", ".bin");
    assert_eq!(std::fs::read(minimised).unwrap(), b"!");
    assert!(took < Duration::from_secs(30), "{took:?}");
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_looping_target_times_out_within_the_limit() {
    let out = out_dir("loop");
    let (code, lines, took) = fuzzlite(
        &[
            "run",
            "--target",
            "selftest-loop",
            "--runs",
            "1",
            "--seed",
            "1",
            "--timeout-ms",
            "300",
            "--minimize-runs",
            "40",
        ],
        &out,
    );
    assert_eq!(code, Some(1), "{lines:?}");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(field(&lines[0], "kind"), "timeout");
    assert_eq!(field(&lines[0], "input_bytes"), "1", "{}", lines[0]);
    // 1 case, 1 confirmation and a handful of 300 ms minimising runs.
    assert!(took < Duration::from_secs(20), "{took:?}");
    let _ = std::fs::remove_dir_all(&out);
}

#[cfg(target_os = "linux")]
#[test]
fn a_memory_hog_hits_the_memory_limit() {
    let out = out_dir("alloc");
    let (code, lines, took) = fuzzlite(
        &[
            "run",
            "--target",
            "selftest-alloc",
            "--runs",
            "1",
            "--memory-mib",
            "128",
            "--no-minimize",
        ],
        &out,
    );
    assert_eq!(code, Some(1), "{lines:?}");
    assert_eq!(field(&lines[0], "kind"), "memory_limit", "{}", lines[0]);
    assert!(took < Duration::from_secs(20), "{took:?}");
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_slow_finish_is_an_amplification() {
    let out = out_dir("slow");
    let (code, lines, _) = fuzzlite(
        &[
            "run",
            "--target",
            "selftest-slow",
            "--runs",
            "1",
            "--amp-ms",
            "1000",
            "--no-minimize",
        ],
        &out,
    );
    assert_eq!(code, Some(1), "{lines:?}");
    assert_eq!(field(&lines[0], "kind"), "amplification", "{}", lines[0]);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_quiet_target_finds_nothing_and_repro_agrees() {
    let out = out_dir("ok");
    let (code, lines, _) = fuzzlite(
        &[
            "run",
            "--target",
            "selftest-ok",
            "--runs",
            "50",
            "--seed",
            "3",
        ],
        &out,
    );
    assert_eq!(code, Some(0), "{lines:?}");
    assert!(lines.is_empty(), "{lines:?}");

    std::fs::create_dir_all(&out).unwrap();
    let input = out.join("input.bin");
    std::fs::write(&input, b"a ! b").unwrap();
    let o = Command::new(EXE)
        .args(["repro", "--target", "selftest-panic"])
        .arg(&input)
        .output()
        .unwrap();
    let line = String::from_utf8_lossy(&o.stdout);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(field(&line, "kind"), "panic", "{line}");
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn refuses_to_write_into_the_repository() {
    let inside = Path::new(env!("CARGO_MANIFEST_DIR")).join("findings-here");
    let (code, lines, _) = fuzzlite(&["run", "--target", "selftest-ok", "--runs", "1"], &inside);
    assert_eq!(code, Some(1), "{lines:?}");
    assert!(!inside.exists());
}

#[test]
fn usage_errors_exit_2() {
    for args in [
        &["run"][..],
        &["run", "--target", "nope"],
        &["frobnicate"],
        &[],
    ] {
        let o = Command::new(EXE).args(args).output().unwrap();
        assert_eq!(o.status.code(), Some(2), "{args:?}");
        assert!(o.stdout.is_empty());
    }
}

// Peak memory is read from /proc, so only Linux measures it; elsewhere
// fuzzlite says at the start that memory is not measured (MEMORY_MEASURED).
#[cfg(target_os = "linux")]
#[test]
fn a_memory_hungry_finish_is_an_amplification() {
    let out = out_dir("big");
    let (code, lines, _) = fuzzlite(
        &[
            "run",
            "--target",
            "selftest-big",
            "--runs",
            "1",
            "--amp-mib",
            "64",
            "--memory-mib",
            "512",
            "--no-minimize",
        ],
        &out,
    );
    assert_eq!(code, Some(1), "{lines:?}");
    assert_eq!(field(&lines[0], "kind"), "amplification", "{}", lines[0]);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_panic_the_target_catches_itself_is_reported() {
    let out = out_dir("caught");
    let (code, lines, _) = fuzzlite(
        &[
            "run",
            "--target",
            "selftest-caught",
            "--runs",
            "5",
            "--seed",
            "1",
        ],
        &out,
    );
    assert_eq!(code, Some(1), "{lines:?}");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(field(&lines[0], "kind"), "caught_panic", "{}", lines[0]);
    assert!(lines[0].contains("selftest-panic found"), "{}", lines[0]);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn panics_are_never_dropped_at_the_cap() {
    let out = out_dir("cap");
    let (code, lines, _) = fuzzlite(
        &[
            "run",
            "--target",
            "selftest-panic",
            "--runs",
            "3",
            "--max-resource-findings",
            "0",
        ],
        &out,
    );
    assert_eq!(code, Some(1), "{lines:?}");
    assert_eq!(field(&lines[0], "kind"), "panic", "{lines:?}");
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_child_whose_parent_is_gone_stops_itself() {
    use std::io::Write;
    use std::process::Stdio;
    // The parent would kill it at 300 ms; without a parent the watchdog
    // ends it 2 s later with exit code 124.
    let start = Instant::now();
    let mut child = Command::new(EXE)
        .args(["__child", "selftest-loop", "300"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"~").unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(124));
    let took = start.elapsed();
    assert!(took < Duration::from_secs(10), "{took:?}");
}

#[test]
fn the_old_cap_flag_name_still_works() {
    let out = out_dir("alias");
    let (code, lines, _) = fuzzlite(
        &[
            "run",
            "--target",
            "selftest-ok",
            "--runs",
            "2",
            "--max-per-kind",
            "0",
        ],
        &out,
    );
    assert_eq!(code, Some(0), "{lines:?}");
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn a_missing_repro_file_does_not_show_the_home_folder() {
    let home = out_dir("home");
    std::fs::create_dir_all(&home).unwrap();
    let missing = home.join("missing.bin");
    let o = Command::new(EXE)
        .args(["repro", "--target", "selftest-ok"])
        .arg(&missing)
        .env("HOME", &home)
        .env_remove("USERPROFILE")
        .output()
        .expect("fuzzlite runs");
    let err = String::from_utf8_lossy(&o.stderr);
    let home_text = home.to_string_lossy();
    assert_eq!(o.status.code(), Some(1), "{err}");
    assert!(!err.contains(&*home_text), "{err}");
    let shown = Path::new("~").join("missing.bin");
    assert!(err.contains(&*shown.to_string_lossy()), "{err}");
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn a_symlinked_home_is_hidden_in_its_real_form_too() {
    let base = out_dir("linkhome");
    let real = base.join("real");
    let link = base.join("link");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let out = real.join("out");
    let o = Command::new(EXE)
        .args([
            "run",
            "--target",
            "selftest-panic",
            "--runs",
            "1",
            "--seed",
            "1",
        ])
        .arg("--out")
        .arg(&out)
        .env("HOME", &link)
        .env_remove("USERPROFILE")
        .output()
        .expect("fuzzlite runs");
    let stdout = String::from_utf8_lossy(&o.stdout);
    let stderr = String::from_utf8_lossy(&o.stderr);
    assert_eq!(o.status.code(), Some(1), "{stdout}{stderr}");
    let real_text = std::fs::canonicalize(&real).unwrap();
    let real_text = real_text.to_string_lossy();
    assert!(!stdout.contains(&*real_text), "{stdout}");
    assert!(!stderr.contains(&*real_text), "{stderr}");
    assert!(stdout.contains("~/out"), "{stdout}");
    let jsons: Vec<_> = std::fs::read_dir(out.join("selftest-panic"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".json"))
        .collect();
    assert_eq!(jsons.len(), 1);
    let json = std::fs::read_to_string(jsons[0].path()).unwrap();
    assert!(!json.contains(&*real_text), "{json}");
    let _ = std::fs::remove_dir_all(&base);
}
