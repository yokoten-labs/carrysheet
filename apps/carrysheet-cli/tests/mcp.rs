//! Drives the real `carrysheet-cli mcp serve` over stdio, the way an MCP
//! client does: line-delimited JSON-RPC on stdin and stdout. Checks that
//! stdout carries nothing but JSON-RPC, that tools come from the table, that
//! calls fail cleanly with the fix first, that paths stay inside `--root`,
//! and that the server starts fast.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use carrysheet_commands::schema::tool_definition;
use carrysheet_commands::table::mcp_tools;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_carrysheet-cli");

/// How long to wait for one response before failing the test.
const WAIT: Duration = Duration::from_secs(20);

/// Startup bound. The target is well under 1 s; debug builds on a busy
/// shared machine are slower, so the assertion is generous and the measured
/// time is printed.
const STARTUP_LIMIT: Duration = Duration::from_secs(5);

/// A fresh folder holding `home/`, `root/` (with `book.xlsx`) and
/// `outside/` (with `secret.xlsx`). Removed when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Scratch {
    fn new() -> Scratch {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "carrysheet-mcp-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for sub in ["home", "root", "outside"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::write(dir.join("root/book.xlsx"), b"x").unwrap();
        std::fs::write(dir.join("outside/secret.xlsx"), b"x").unwrap();
        Scratch(dir)
    }

    fn path(&self, sub: &str) -> PathBuf {
        self.0.join(sub)
    }
}

/// A running server with its stdout read line by line on a thread.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    /// Every stdout line seen, to check they are all JSON-RPC.
    seen: Vec<String>,
    started: Instant,
}

impl Server {
    fn start(scratch: &Scratch, extra: &[&str]) -> Server {
        let root = scratch.path("root");
        let mut args = vec!["mcp", "serve", "--root", root.to_str().unwrap()];
        args.extend_from_slice(extra);
        let started = Instant::now();
        let mut child = Command::new(BIN)
            .args(&args)
            // Run from elsewhere, so the root comes from --root alone.
            .current_dir(scratch.path("home"))
            .env("HOME", scratch.path("home"))
            .env("USERPROFILE", scratch.path("home"))
            .env("TZ", "UTC")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        Server {
            child,
            stdin,
            lines,
            seen: Vec::new(),
            started,
        }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        let mut line = message.to_string();
        line.push('\n');
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    /// Sends a request and waits for the response with the same id.
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = self
                .lines
                .recv_timeout(WAIT)
                .unwrap_or_else(|_| panic!("no response to {method} within {WAIT:?}"));
            self.seen.push(line.clone());
            let msg = jsonrpc(&line);
            if msg["id"] == id {
                return msg;
            }
        }
    }

    /// Sends a request and returns its `result`, failing on a protocol error.
    fn result(&mut self, id: u64, method: &str, params: Value) -> Value {
        let msg = self.request(id, method, params);
        assert!(msg.get("error").is_none(), "{method}: {msg}");
        msg["result"].clone()
    }

    /// Closes stdin and waits for the process; returns its exit code and
    /// stderr, after checking that every stdout line was JSON-RPC.
    fn finish(mut self) -> (i32, String) {
        drop(self.stdin.take());
        let deadline = Instant::now() + WAIT;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "the server did not exit");
            std::thread::sleep(Duration::from_millis(20));
        };
        while let Ok(line) = self.lines.recv_timeout(Duration::from_millis(200)) {
            self.seen.push(line);
        }
        for line in &self.seen {
            jsonrpc(line);
        }
        let mut stderr = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        (status.code().unwrap_or(-1), stderr)
    }
}

/// Parses one stdout line, checking it is a single JSON-RPC 2.0 message.
fn jsonrpc(line: &str) -> Value {
    assert!(!line.contains('\r'), "CR on stdout: {line:?}");
    let msg: Value =
        serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON ({e}): {line:?}"));
    assert_eq!(msg["jsonrpc"], "2.0", "{line}");
    msg
}

fn initialize(server: &mut Server, version: &str) -> Value {
    let result = server.result(
        1,
        "initialize",
        json!({
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": { "name": "carrysheet-test", "version": "1" },
        }),
    );
    server.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    result
}

fn call(server: &mut Server, id: u64, name: &str, arguments: Value) -> Value {
    server.result(
        id,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
}

/// Checks a failed tool call: `isError`, the same JSON in text and
/// `structuredContent`, and the fix within the first 200 characters.
/// Returns the error object.
fn tool_error(result: &Value) -> Value {
    assert_eq!(result["isError"], true, "{result}");
    let text = result["content"][0]["text"].as_str().unwrap();
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed, result["structuredContent"]);
    assert_eq!(
        text,
        result["structuredContent"].to_string(),
        "compact JSON"
    );
    let keys: Vec<&str> = parsed["error"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["code", "what", "fix", "why"], "{text}");
    let fix_at = text.find(r#""fix":""#).unwrap();
    assert!(fix_at < 200, "fix starts after 200 characters: {text}");
    parsed["error"].clone()
}

fn tool_names(tools: &Value) -> Vec<String> {
    tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn stdio_session_lists_and_calls_tools() {
    let scratch = Scratch::new();
    let mut server = Server::start(&scratch, &[]);

    let init = initialize(&mut server, "2025-11-25");
    let startup = server.started.elapsed();
    eprintln!("mcp serve: spawn to initialize response in {startup:?}");
    assert!(startup < STARTUP_LIMIT, "startup took {startup:?}");
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert_eq!(init["serverInfo"]["name"], "carrysheet");
    assert!(init["capabilities"]["tools"].is_object());

    // Tools: the table's MCP tools, in its order, exactly as defined there.
    let tools = server.result(2, "tools/list", json!({}));
    let expected: Vec<Value> = mcp_tools().map(tool_definition).collect();
    assert_eq!(tools["tools"], Value::Array(expected));
    for t in tools["tools"].as_array().unwrap() {
        for key in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(t["annotations"][key].is_boolean(), "{} {key}", t["name"]);
        }
    }
    assert!(!tools.to_string().contains("session_id"));
    // All tool definitions together stay under 10,000 characters.
    let size: usize = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.to_string().chars().count())
        .sum();
    eprintln!("tools/list: {size} characters");
    assert!(size < 10_000, "{size}");

    // A command that exists but is not built: a clean tool error.
    let r = call(&mut server, 3, "overview", json!({ "path": "book.xlsx" }));
    let e = tool_error(&r);
    assert_eq!(e["code"], "not_implemented");

    // A built command: success, same data in text and structuredContent.
    let r = call(
        &mut server,
        4,
        "feedback",
        json!({ "kind": "bug", "message": "Saving as csv drops the second sheet" }),
    );
    assert_eq!(r["isError"], false, "{r}");
    let text = r["content"][0]["text"].as_str().unwrap();
    assert_eq!(text, r["structuredContent"].to_string());
    assert_eq!(r["structuredContent"]["ok"], true);
    assert_eq!(r["structuredContent"]["sent"], false);

    // Paths outside --root are refused: by .., absolutely, and as outputs.
    let secret = scratch.path("outside/secret.xlsx");
    for (id, args) in [
        (5, json!({ "path": "../outside/secret.xlsx" })),
        (6, json!({ "path": secret.to_str().unwrap() })),
        (7, json!({ "path": "missing/../../outside/secret.xlsx" })),
    ] {
        let e = tool_error(&call(&mut server, id, "check", args));
        assert_eq!(e["code"], "outside_root");
    }
    let e = tool_error(&call(
        &mut server,
        12,
        "macros",
        json!({ "path": "../outside/secret.xlsx", "include_source": true }),
    ));
    assert_eq!(e["code"], "outside_root");
    let e = tool_error(&call(
        &mut server,
        8,
        "convert",
        json!({ "path": "book.xlsx", "output": "../outside/new.csv" }),
    ));
    assert_eq!(e["code"], "outside_root");

    // Bad arguments and unknown tools are tool errors too; the session lives on.
    let e = tool_error(&call(
        &mut server,
        9,
        "read",
        json!({ "path": "book.xlsx", "max_rows": 0 }),
    ));
    assert_eq!(e["code"], "invalid_argument");
    let e = tool_error(&call(&mut server, 10, "overveiw", json!({})));
    assert!(e["fix"].as_str().unwrap().contains("overview"));
    let again = server.result(11, "tools/list", json!({}));
    assert_eq!(again, tools);

    let (code, stderr) = server.finish();
    assert_eq!(code, 0, "{stderr}");
    // The log went to stderr, without the home folder.
    assert!(
        stderr.contains("tools/call overview: not_implemented"),
        "{stderr}"
    );
    assert!(stderr.contains("the client closed stdin"), "{stderr}");
    assert!(!stderr.contains(scratch.path("home").to_str().unwrap()));
}

#[cfg(unix)]
#[test]
fn links_out_of_the_root_are_refused() {
    let scratch = Scratch::new();
    let root = scratch.path("root");
    std::os::unix::fs::symlink(scratch.path("outside"), root.join("escape")).unwrap();
    let mut server = Server::start(&scratch, &[]);
    initialize(&mut server, "2025-11-25");
    for (id, path) in [(2, "escape/secret.xlsx"), (3, "escape/new.xlsx")] {
        let e = tool_error(&call(&mut server, id, "overview", json!({ "path": path })));
        assert_eq!(e["code"], "outside_root", "{path}");
    }
    let (code, _) = server.finish();
    assert_eq!(code, 0);
}

#[test]
fn read_only_hides_write_tools_and_logs_to_a_file() {
    let scratch = Scratch::new();
    let log = scratch.path("home/server.log");
    let mut server = Server::start(
        &scratch,
        &["--read-only", "--log-file", log.to_str().unwrap()],
    );
    initialize(&mut server, "2025-11-25");
    let tools = server.result(2, "tools/list", json!({}));
    assert_eq!(
        tool_names(&tools),
        ["overview", "read", "eval", "check", "macros", "feedback"]
    );
    let e = tool_error(&call(
        &mut server,
        3,
        "edit",
        json!({ "path": "book.xlsx", "ops": [] }),
    ));
    assert!(e["fix"].as_str().unwrap().contains("without --read-only"));

    let (code, stderr) = server.finish();
    assert_eq!(code, 0);
    assert!(stderr.is_empty(), "{stderr}");
    let log = std::fs::read_to_string(&log).unwrap();
    assert!(log.contains("(read-only)"), "{log}");
    assert!(log.contains("tools/call edit: invalid_argument"), "{log}");
}

#[test]
fn discover_and_calls_without_initialize() {
    let scratch = Scratch::new();
    let mut server = Server::start(&scratch, &[]);
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": "carrysheet-test", "version": "1" },
    });
    let found = server.result(1, "server/discover", json!({ "_meta": meta }));
    let versions = found["supportedVersions"].as_array().unwrap();
    assert!(versions.contains(&json!("2025-11-25")), "{found}");
    assert!(versions.contains(&json!("2026-07-28")), "{found}");
    assert!(found["capabilities"]["tools"].is_object());

    let tools = server.result(2, "tools/list", json!({ "_meta": meta }));
    assert_eq!(
        tools["tools"].as_array().unwrap().len(),
        mcp_tools().count()
    );
    let r = server.result(
        3,
        "tools/call",
        json!({ "_meta": meta, "name": "eval", "arguments": { "path": "book.xlsx", "formula": "1+1" } }),
    );
    assert_eq!(tool_error(&r)["code"], "not_implemented");

    let (code, stderr) = server.finish();
    assert_eq!(code, 0, "{stderr}");
}

#[test]
fn bad_options_fail_before_serving() {
    let scratch = Scratch::new();
    let run = |args: &[&str]| {
        Command::new(BIN)
            .args(args)
            .env("HOME", scratch.path("home"))
            .env("USERPROFILE", scratch.path("home"))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let missing = scratch.path("no-such-folder");
    let o = run(&["mcp", "serve", "--root", missing.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    assert!(o.stdout.is_empty());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--root"));

    let o = run(&["mcp", "serve", "--http", "127.0.0.1:9", "--json"]);
    assert_eq!(o.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["error"]["code"], "not_implemented");

    // stdin closed at once: no session, nothing on stdout, a clean exit.
    let o = run(&[
        "mcp",
        "serve",
        "--root",
        scratch.path("root").to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stdout.is_empty());
}

#[test]
fn a_whole_drive_or_the_home_folder_needs_an_explicit_root() {
    let scratch = Scratch::new();
    let home = scratch.path("home");
    let top = if cfg!(windows) { r"C:\" } else { "/" };
    let serve = |cwd: &std::path::Path, args: &[&str]| {
        Command::new(BIN)
            .args(["mcp", "serve"])
            .args(args)
            .current_dir(cwd)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    for cwd in [home.as_path(), std::path::Path::new(top)] {
        let o = serve(cwd, &[]);
        assert_eq!(o.status.code(), Some(2), "{}", cwd.display());
        assert!(o.stdout.is_empty());
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(err.contains("too wide to serve without --root"), "{err}");
        assert!(err.contains("fix: Start it with --root"), "{err}");
    }
    // Asked for explicitly, it starts, with a warning in the log.
    let o = serve(&home, &["--root", top]);
    assert_eq!(o.status.code(), Some(0));
    assert!(o.stdout.is_empty());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("warning: --root"), "{err}");
}

#[test]
fn startup_is_fast() {
    // A second, separate measurement with nothing else in the session.
    let scratch = Scratch::new();
    let mut server = Server::start(&scratch, &[]);
    initialize(&mut server, "2025-06-18");
    let startup = server.started.elapsed();
    eprintln!("mcp serve: spawn to initialize response in {startup:?}");
    assert!(startup < STARTUP_LIMIT, "startup took {startup:?}");
    let (code, _) = server.finish();
    assert_eq!(code, 0);
}
