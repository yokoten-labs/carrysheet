//! Real HTTP requests against the server, run in this process. Requests are
//! written by hand on a TCP socket, so tests can send what no well-behaved
//! client would: a foreign Host, an evil Origin, a wrong token, a huge body.

use std::io::Read;
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use carrysheet_commands::root::Root;
use serde_json::Value;

use super::*;
use crate::mcp::Transport;

/// How long a test waits for any answer.
const WAIT: Duration = Duration::from_secs(20);

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"carrysheet-test","version":"1"}}}"#;

/// A server on 127.0.0.1 with a scratch root, running on its own thread
/// until the test ends.
struct TestServer {
    port: u16,
    token: String,
    dir: PathBuf,
    /// The MCP server, for its test hooks.
    server: Server,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(read_only: bool, limits: Limits) -> TestServer {
    start_slow(read_only, limits, 0)
}

/// A server whose every tool call takes `delay_ms` on its worker thread.
fn start_slow(read_only: bool, limits: Limits, delay_ms: u64) -> TestServer {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "carrysheet-http-unit-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("root")).unwrap();
    std::fs::write(dir.join("root/book.xlsx"), b"x").unwrap();
    let log = Arc::new(Log::file(&dir.join("server.log")).unwrap());
    let server = Server::new(
        Root::new(&dir.join("root")).unwrap(),
        read_only,
        Transport::Http,
        log.clone(),
    );
    server
        .test_hooks()
        .delay_ms
        .store(delay_ms, Ordering::SeqCst);
    let kept = server.clone();
    let token = Token::generate().unwrap();
    let shown = token.reveal().to_owned();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(async move {
                let listener = TcpListener::from_std(listener)?;
                let shutdown = async move {
                    let _ = stopped.await;
                };
                serve(listener, server, token, limits, log, shutdown).await
            })
            .unwrap();
        runtime.shutdown_background();
    });
    TestServer {
        port,
        token: shown,
        dir,
        server: kept,
        stop: Some(stop),
        thread: Some(thread),
    }
}

/// One HTTP answer.
#[derive(Debug)]
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", String::from_utf8_lossy(&self.body)))
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The JSON error of a refusal, after checking its shape.
    fn refusal(&self, status: u16, code: &str) -> Value {
        assert_eq!(self.status, status, "{self:?} {}", self.text());
        assert_eq!(self.header("content-type"), Some("application/json"));
        assert_eq!(self.header("connection"), Some("close"), "{self:?}");
        let v = self.json();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], code, "{v}");
        let keys: Vec<_> = v["error"].as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["code", "what", "fix", "why"]);
        assert!(self.body.len() < 600, "short: {}", self.text());
        v
    }
}

/// Sends raw bytes and reads the whole answer (the request should close
/// the connection).
fn exchange(port: u16, raw: &[u8]) -> Answer {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    // The server may answer and close before reading a large body.
    let _ = stream.write_all(raw);
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    parse_answer(&bytes)
}

fn parse_answer(bytes: &[u8]) -> Answer {
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("no answer head: {:?}", String::from_utf8_lossy(bytes)));
    let head = std::str::from_utf8(&bytes[..split]).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers: Vec<(String, String)> = lines
        .map(|l| {
            let (k, v) = l.split_once(':').unwrap();
            (k.trim().to_owned(), v.trim().to_owned())
        })
        .collect();
    let mut body = bytes[split + 4..].to_vec();
    let chunked = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("transfer-encoding") && v.eq_ignore_ascii_case("chunked")
    });
    if chunked {
        body = dechunk(&body);
    }
    Answer {
        status,
        headers,
        body,
    }
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let end = data.windows(2).position(|w| w == b"\r\n").unwrap();
        let size =
            usize::from_str_radix(std::str::from_utf8(&data[..end]).unwrap().trim(), 16).unwrap();
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&data[end + 2..end + 2 + size]);
        data = &data[end + 2 + size + 2..];
    }
}

/// A POST with the given headers (each a whole `Name: value` line).
fn post_raw(port: u16, path: &str, headers: &[String], body: &str) -> Answer {
    let mut raw = format!("POST {path} HTTP/1.1\r\n");
    for h in headers {
        raw.push_str(h);
        raw.push_str("\r\n");
    }
    raw.push_str(&format!(
        "Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    exchange(port, raw.as_bytes())
}

impl TestServer {
    fn host(&self) -> String {
        format!("Host: 127.0.0.1:{}", self.port)
    }

    fn bearer(&self) -> String {
        format!("Authorization: Bearer {}", self.token)
    }

    /// A well-formed MCP POST, with extra header lines.
    fn post_with(&self, extra: &[String], body: &str) -> Answer {
        let mut headers = vec![self.host(), self.bearer()];
        headers.extend_from_slice(extra);
        post_raw(self.port, PATH, &headers, body)
    }

    fn post(&self, body: &str) -> Answer {
        self.post_with(&[], body)
    }

    /// A JSON-RPC request; returns its `result`, failing on anything else.
    fn rpc(&self, extra: &[String], id: u64, method: &str, params: Value) -> Value {
        let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        // The 2026-07-28 transport names the method in a header too.
        let mut headers = extra.to_vec();
        headers.push(format!("Mcp-Method: {method}"));
        if let Some(name) = params["name"].as_str() {
            headers.push(format!("Mcp-Name: {name}"));
        }
        let a = self.post_with(&headers, &body.to_string());
        assert_eq!(a.status, 200, "{method}: {}", a.text());
        assert_no_cors(&a);
        let v = a.json();
        assert_eq!(v["id"], id, "{v}");
        assert!(v.get("error").is_none(), "{method}: {v}");
        v["result"].clone()
    }
}

fn assert_no_cors(a: &Answer) {
    for (k, _) in &a.headers {
        assert!(
            !k.to_ascii_lowercase().starts_with("access-control-"),
            "CORS header {k}"
        );
    }
}

fn names(tools: &Value) -> Vec<String> {
    tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn initialize_list_and_call_round_trip() {
    let s = start(true, Limits::default());
    let a = s.post(INIT);
    assert_eq!(a.status, 200, "{}", a.text());
    assert_eq!(a.header("content-type"), Some("application/json"));
    assert_eq!(a.header("cache-control"), Some("no-store"));
    // Stateless: no session to remember.
    assert_eq!(a.header("mcp-session-id"), None);
    let init = a.json();
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(init["result"]["serverInfo"]["name"], "carrysheet");

    let version = "MCP-Protocol-Version: 2025-11-25".to_owned();
    let a = s.post_with(
        std::slice::from_ref(&version),
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
    );
    assert_eq!(a.status, 202, "{}", a.text());

    let tools = s.rpc(std::slice::from_ref(&version), 2, "tools/list", json!({}));
    assert_eq!(
        names(&tools),
        ["overview", "read", "eval", "check", "macros", "feedback"]
    );
    let r = s.rpc(
        std::slice::from_ref(&version),
        3,
        "tools/call",
        json!({ "name": "feedback", "arguments": { "kind": "idea", "message": "Sort by two columns" } }),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(r["structuredContent"]["ok"], true);
    assert_eq!(
        r["content"][0]["text"].as_str().unwrap(),
        r["structuredContent"].to_string()
    );
    // Paths stay inside the root over HTTP too.
    let r = s.rpc(
        std::slice::from_ref(&version),
        4,
        "tools/call",
        json!({ "name": "check", "arguments": { "path": "../outside.xlsx" } }),
    );
    assert_eq!(r["structuredContent"]["error"]["code"], "outside_root");
}

#[test]
fn discover_lifecycle_works_too() {
    let s = start(true, Limits::default());
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": { "name": "carrysheet-test", "version": "1" },
    });
    let version = ["MCP-Protocol-Version: 2026-07-28".to_owned()];
    let found = s.rpc(&version, 1, "server/discover", json!({ "_meta": meta }));
    let versions = found["supportedVersions"].as_array().unwrap();
    assert!(versions.contains(&json!("2025-11-25")), "{found}");
    assert!(versions.contains(&json!("2026-07-28")), "{found}");
    let tools = s.rpc(&version, 2, "tools/list", json!({ "_meta": meta }));
    assert_eq!(names(&tools).len(), 6);
    let r = s.rpc(
        &version,
        3,
        "tools/call",
        json!({ "_meta": meta, "name": "feedback", "arguments": { "kind": "bug", "message": "Dates show as numbers" } }),
    );
    assert_eq!(r["structuredContent"]["ok"], true, "{r}");
}

#[test]
fn a_foreign_host_is_forbidden() {
    let s = start(true, Limits::default());
    let p = s.port;
    let bearer = s.bearer();
    for host in [
        vec![format!("Host: evil.example:{p}")],
        vec!["Host: evil.example".to_owned()],
        vec![format!("Host: 127.0.0.1:{}", p.wrapping_add(1))],
        vec!["Host: 127.0.0.1".to_owned()],
        vec![format!("Host: localhost.evil.example:{p}")],
        vec![
            format!("Host: 127.0.0.1:{p}"),
            format!("Host: evil.example:{p}"),
        ],
        vec![],
    ] {
        let mut headers = host.clone();
        headers.push(bearer.clone());
        let a = post_raw(p, PATH, &headers, INIT);
        a.refusal(403, "forbidden");
        assert!(!a.text().contains("evil"), "{}", a.text());
        assert_no_cors(&a);
    }
    // Absolute-form targets must name this server too.
    let a = post_raw(
        p,
        &format!("http://evil.example:{p}{PATH}"),
        &[s.host(), bearer.clone()],
        INIT,
    );
    a.refusal(403, "forbidden");
    // Each allowed name works, in any case.
    for host in [
        format!("Host: localhost:{p}"),
        format!("Host: LocalHost:{p}"),
        format!("Host: [::1]:{p}"),
    ] {
        let a = post_raw(p, PATH, &[host.clone(), bearer.clone()], INIT);
        assert_eq!(a.status, 200, "{host}: {}", a.text());
    }
}

#[test]
fn an_evil_origin_is_forbidden() {
    let s = start(true, Limits::default());
    let p = s.port;
    for origin in [
        "http://evil.example".to_owned(),
        format!("http://evil.example:{p}"),
        format!("https://127.0.0.1:{p}"),
        format!("http://127.0.0.1:{}", p.wrapping_add(1)),
        "http://127.0.0.1".to_owned(),
        "null".to_owned(),
        format!("http://127.0.0.1:{p}/"),
    ] {
        let a = s.post_with(&[format!("Origin: {origin}")], INIT);
        a.refusal(403, "forbidden");
        assert!(!a.text().contains("evil"), "{}", a.text());
        assert_no_cors(&a);
    }
    // Two Origin headers, even good ones, are refused.
    let good = format!("Origin: http://localhost:{p}");
    let a = s.post_with(&[good.clone(), good.clone()], INIT);
    a.refusal(403, "forbidden");
    // A browser preflight from another site gets no CORS answer.
    let raw = format!(
        "OPTIONS {PATH} HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nOrigin: http://evil.example\r\n\
         Access-Control-Request-Method: POST\r\nConnection: close\r\n\r\n"
    );
    let a = exchange(p, raw.as_bytes());
    a.refusal(403, "forbidden");
    assert_no_cors(&a);
    // Pages on this server's own origins are allowed.
    for origin in [
        format!("http://127.0.0.1:{p}"),
        format!("http://localhost:{p}"),
        format!("http://[::1]:{p}"),
    ] {
        let a = s.post_with(&[format!("Origin: {origin}")], INIT);
        assert_eq!(a.status, 200, "{origin}: {}", a.text());
        assert_no_cors(&a);
    }
}

#[test]
fn a_missing_or_wrong_token_is_unauthorized() {
    let s = start(true, Limits::default());
    let p = s.port;
    let mut wrong = s.token.clone().into_bytes();
    wrong[63] = if wrong[63] == b'0' { b'1' } else { b'0' };
    let wrong = String::from_utf8(wrong).unwrap();
    for auth in [
        vec![],
        vec![format!("Authorization: Bearer {wrong}")],
        vec![format!("Authorization: Bearer {}", &s.token[..63])],
        vec![format!("Authorization: Bearer {}0", s.token)],
        vec![format!("Authorization: Bearer {}", s.token.to_uppercase())],
        vec![format!("Authorization: {}", s.token)],
        vec![format!("Authorization: Basic {}", s.token)],
        vec!["Authorization: Bearer ".to_owned()],
        vec![s.bearer(), format!("Authorization: Bearer {wrong}")],
    ] {
        let mut headers = vec![s.host()];
        headers.extend(auth.iter().cloned());
        let a = post_raw(p, PATH, &headers, INIT);
        a.refusal(401, "unauthorized");
        assert_eq!(a.header("www-authenticate"), Some("Bearer"), "{auth:?}");
        assert!(!a.text().contains(&s.token[..16]), "{}", a.text());
    }
    // The scheme is case-insensitive.
    let a = post_raw(
        p,
        PATH,
        &[s.host(), format!("Authorization: bearer {}", s.token)],
        INIT,
    );
    assert_eq!(a.status, 200, "{}", a.text());
    // Host and Origin are checked first: a foreign page learns nothing more.
    let a = post_raw(p, PATH, &[format!("Host: evil.example:{p}")], INIT);
    a.refusal(403, "forbidden");
}

#[test]
fn other_paths_are_not_found() {
    let s = start(true, Limits::default());
    for path in ["/", "/mcp/", "/MCP", "/sse"] {
        let a = post_raw(s.port, path, &[s.host(), s.bearer()], INIT);
        a.refusal(404, "not_found");
    }
}

#[test]
fn oversized_bodies_are_refused() {
    let limits = Limits {
        max_body_bytes: 64 * 1024,
        ..Limits::default()
    };
    let s = start(true, limits);
    // Declared too large: refused before the body is read.
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        s.host(),
        s.bearer(),
        5 * 1024 * 1024
    );
    let a = exchange(s.port, raw.as_bytes());
    let v = a.refusal(413, "too_large");
    assert!(
        v["error"]["what"].as_str().unwrap().contains("65536"),
        "{v}"
    );
    // Sent chunked, with no length up front: rmcp stops reading at the limit.
    let big = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"pad":"{}"}}}}"#,
        "x".repeat(70 * 1024)
    );
    let mut raw = format!(
        "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nTransfer-Encoding: chunked\r\n\
         Connection: close\r\n\r\n",
        s.host(),
        s.bearer()
    )
    .into_bytes();
    for chunk in big.as_bytes().chunks(8192) {
        raw.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        raw.extend_from_slice(chunk);
        raw.extend_from_slice(b"\r\n");
    }
    raw.extend_from_slice(b"0\r\n\r\n");
    let a = exchange(s.port, &raw);
    assert_eq!(a.status, 413, "{}", a.text());
    assert_no_cors(&a);
    // Under the limit is fine.
    let a = s.post(INIT);
    assert_eq!(a.status, 200, "{}", a.text());
}

#[test]
fn write_tools_are_refused_in_read_only_mode() {
    let s = start(true, Limits::default());
    let version = ["MCP-Protocol-Version: 2025-11-25".to_owned()];
    let tools = s.rpc(&version, 1, "tools/list", json!({}));
    assert!(!names(&tools).contains(&"edit".to_owned()));
    let r = s.rpc(
        &version,
        2,
        "tools/call",
        json!({ "name": "edit", "arguments": { "path": "book.xlsx", "ops": [] } }),
    );
    assert_eq!(r["isError"], true, "{r}");
    let e = &r["structuredContent"]["error"];
    assert!(e["what"].as_str().unwrap().contains("read-only"), "{e}");
    assert!(e["fix"].as_str().unwrap().contains("--allow-writes"), "{e}");
    assert_eq!(std::fs::read(s.dir.join("root/book.xlsx")).unwrap(), b"x");

    // With --allow-writes, the tools that change files are offered.
    let s = start(false, Limits::default());
    let tools = s.rpc(&version, 1, "tools/list", json!({}));
    assert!(names(&tools).contains(&"edit".to_owned()));
}

#[test]
fn too_many_requests_at_once_are_turned_away() {
    let s = start(
        true,
        Limits {
            max_requests: 0,
            ..Limits::default()
        },
    );
    let a = s.post(INIT);
    a.refusal(503, "busy");
    assert_eq!(a.header("retry-after"), Some("1"));
}

#[test]
fn slow_bodies_time_out_without_taking_a_slot() {
    let s = start(
        true,
        Limits {
            max_requests: 1,
            body_timeout: Duration::from_millis(500),
            ..Limits::default()
        },
    );
    // Promise a body and never finish it.
    let mut stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: 1000\r\n\
         Connection: close\r\n\r\n{{\"jsonrpc\"",
        s.host(),
        s.bearer()
    );
    stream.write_all(raw.as_bytes()).unwrap();
    // While it waits, the one work slot is still free.
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(s.post(INIT).status, 200);
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    parse_answer(&bytes).refusal(408, "body_timeout");
}

#[test]
fn idle_connections_are_closed() {
    let s = start(
        true,
        Limits {
            idle_timeout: Duration::from_millis(300),
            ..Limits::default()
        },
    );
    // A connection that never sends anything.
    let started = std::time::Instant::now();
    let mut stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let mut buf = [0u8; 64];
    let n = stream.read(&mut buf).unwrap_or(0);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        n == 0 || buf.starts_with(b"HTTP/1.1 408"),
        "{:?}",
        &buf[..n]
    );

    // A kept-alive connection: two requests answered on it, then closed
    // once it has been idle for the timeout.
    let mut stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\r\n{INIT}",
        s.host(),
        s.bearer(),
        INIT.len()
    );
    for _ in 0..2 {
        stream.write_all(raw.as_bytes()).unwrap();
        assert_eq!(read_one_answer(&mut stream).status, 200);
    }
    let started = std::time::Instant::now();
    let mut rest = Vec::new();
    let _ = stream.read_to_end(&mut rest);
    let idle = started.elapsed();
    assert!(
        rest.is_empty() || rest.starts_with(b"HTTP/1.1 408"),
        "{rest:?}"
    );
    assert!(
        idle >= Duration::from_millis(200) && idle < Duration::from_secs(10),
        "closed after {idle:?}"
    );
}

/// Reads exactly one answer with a Content-Length from a kept-alive stream.
fn read_one_answer(stream: &mut TcpStream) -> Answer {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    let head = parse_answer(&bytes);
    let length: usize = head.header("content-length").unwrap().parse().unwrap();
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).unwrap();
    Answer { body, ..head }
}

#[test]
fn tokens_are_random_and_hidden() {
    let a = Token::generate().unwrap();
    let b = Token::generate().unwrap();
    assert_ne!(a.reveal(), b.reveal());
    assert_eq!(a.reveal().len(), 64);
    assert!(
        a.reveal()
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    );
    assert_eq!(format!("{a:?}"), "Token(hidden)");
    assert!(a.accepts(format!("Bearer {}", a.reveal()).as_bytes()));
    assert!(a.accepts(format!("BEARER   {}", a.reveal()).as_bytes()));
    assert!(!a.accepts(format!("Bearer {}", b.reveal()).as_bytes()));
    assert!(!a.accepts(b"Bearer"));
    assert!(!a.accepts(b""));
}

#[test]
fn token_file_is_private_and_replaces_what_was_there() {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "carrysheet-token-unit-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("token");
    std::fs::write(&path, "old").unwrap();
    let token = Token::generate().unwrap();
    write_token_file(&path, &token).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), token.reveal());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        // A link at the path is replaced, not written through.
        let target = dir.join("target");
        std::fs::write(&target, "keep").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        write_token_file(&link, &token).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
        assert!(!std::fs::symlink_metadata(&link).unwrap().is_symlink());
    }
    // No temporary files are left behind.
    let left: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(left.is_empty());
    let e = write_token_file(&dir.join("no/such/folder/token"), &token).unwrap_err();
    assert_eq!(e.code, ErrorCode::Io);
    assert!(e.fix.contains("--token-file"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn only_loopback_addresses_are_accepted() {
    assert_eq!(
        parse_address("127.0.0.1:0").unwrap(),
        "127.0.0.1:0".parse::<SocketAddr>().unwrap()
    );
    assert_eq!(
        parse_address("[::1]:8080").unwrap(),
        "[::1]:8080".parse::<SocketAddr>().unwrap()
    );
    for refused in [
        "0.0.0.0:8080",
        "[::]:8080",
        "192.168.1.20:8080",
        "10.0.0.1:80",
        "127.0.0.2:8080",
        "[::ffff:127.0.0.1]:8080",
    ] {
        let e = parse_address(refused).unwrap_err();
        assert_eq!(e.code, ErrorCode::Usage, "{refused}");
        assert_eq!(e.exit_code().code(), 2);
        assert!(e.why.contains("DNS rebinding"), "{}", e.why);
        assert!(e.why.contains("other machines"), "{}", e.why);
        assert!(e.fix.contains("127.0.0.1:PORT"), "{}", e.fix);
    }
    for malformed in ["localhost:8080", "127.0.0.1", "8080", "", "127.0.0.1:99999"] {
        let e = parse_address(malformed).unwrap_err();
        assert_eq!(e.code, ErrorCode::Usage, "{malformed}");
        assert!(e.fix.contains("127.0.0.1:PORT"), "{}", e.fix);
    }
}

#[test]
fn guard_checks_without_a_network() {
    let token = Token::generate().unwrap();
    let bearer = format!("Bearer {}", token.reveal());
    let guard = Guard::new(8080, token, 100);
    let uri: Uri = "/mcp".parse().unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:8080"));
    headers.insert(header::AUTHORIZATION, bearer.parse().unwrap());
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("application/json, text/event-stream"),
    );
    let check = |headers: &HeaderMap| {
        guard.check_caller(&uri, headers)?;
        guard.check_request(&Method::POST, &uri, headers)
    };
    assert_eq!(check(&headers), Ok(()));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("101"));
    assert_eq!(check(&headers), Err(Refusal::TooLarge));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("100"));
    assert_eq!(check(&headers), Ok(()));
    headers.insert(header::HOST, HeaderValue::from_static("[::1]:8080"));
    assert_eq!(check(&headers), Ok(()));
    headers.insert(header::HOST, HeaderValue::from_static("[::1]:8081"));
    assert_eq!(check(&headers), Err(Refusal::Forbidden));
    assert_eq!(
        guard.check_request(&Method::GET, &uri, &headers),
        Err(Refusal::MethodNotAllowed)
    );
}

/// A tools/call request body for `feedback`.
fn feedback_call(id: u64) -> String {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "feedback", "arguments": { "kind": "idea", "message": "slow" } },
    })
    .to_string()
}

/// Sends a request and gives up after `wait`: the status if one came, and
/// the connection is closed either way.
fn post_and_leave(s: &TestServer, body: &str, wait: Duration) -> Option<u16> {
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2025-11-25\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        s.host(),
        s.bearer(),
        body.len()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    stream.set_read_timeout(Some(wait)).unwrap();
    let mut buf = [0u8; 16];
    let n = stream.read(&mut buf).ok()?;
    std::str::from_utf8(&buf[..n])
        .ok()?
        .split(' ')
        .nth(1)?
        .parse()
        .ok()
}

#[test]
fn abandoned_slow_calls_cannot_exceed_the_cap() {
    let s = start_slow(
        true,
        Limits {
            max_requests: 2,
            ..Limits::default()
        },
        1500,
    );
    // Clients that send slow calls and hang up at once.
    let mut busy = 0;
    for id in 0..8 {
        match post_and_leave(&s, &feedback_call(id), Duration::from_millis(100)) {
            Some(503) => busy += 1,
            None => {}
            Some(other) => panic!("status {other}"),
        }
    }
    // The first two hold the slots until their work ends; the rest wait for
    // nothing and are turned away.
    assert_eq!(busy, 6);
    std::thread::sleep(Duration::from_millis(300));
    let hooks = s.server.test_hooks();
    assert_eq!(hooks.running.load(Ordering::SeqCst), 2);
    assert_eq!(s.post(INIT).status, 503, "slots are still held");
    // Once the work is done, the slots are free again.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(hooks.running.load(Ordering::SeqCst), 0);
    assert_eq!(hooks.peak.load(Ordering::SeqCst), 2);
    assert_eq!(s.post(INIT).status, 200);
}

#[test]
fn a_timed_out_call_keeps_its_slot_until_it_ends() {
    let s = start_slow(
        true,
        Limits {
            max_requests: 1,
            request_timeout: Duration::from_millis(200),
            ..Limits::default()
        },
        1000,
    );
    let version = ["MCP-Protocol-Version: 2025-11-25".to_owned()];
    let a = s.post_with(&version, &feedback_call(1));
    a.refusal(504, "timeout");
    s.post(INIT).refusal(503, "busy");
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(s.post(INIT).status, 200);
    assert_eq!(s.server.test_hooks().peak.load(Ordering::SeqCst), 1);
}

#[test]
fn clients_without_the_token_cannot_keep_a_client_out() {
    let s = start(true, Limits::default());
    // 200 connections that never send anything.
    let idle: Vec<TcpStream> = (0..200)
        .map(|_| TcpStream::connect(("127.0.0.1", s.port)).unwrap())
        .collect();
    // 50 that are refused and try to stay open.
    let mut refused = Vec::new();
    for _ in 0..50 {
        let mut stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
        let raw = format!(
            "POST {PATH} HTTP/1.1\r\n{}\r\nContent-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\r\n{INIT}",
            s.host(),
            INIT.len()
        );
        stream.write_all(raw.as_bytes()).unwrap();
        refused.push(stream);
    }
    let started = std::time::Instant::now();
    let a = s.post(INIT);
    let took = started.elapsed();
    assert_eq!(a.status, 200, "{}", a.text());
    assert!(took < Duration::from_secs(1), "the client waited {took:?}");
    // A refused connection is closed after its answer.
    let mut stream = refused.pop().unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let started = std::time::Instant::now();
    assert!(bytes.is_empty() || parse_answer(&bytes).status == 401);
    assert!(started.elapsed() < Duration::from_secs(1));
    drop(idle);
}

#[test]
fn unchecked_connections_are_closed_quickly() {
    let s = start(
        true,
        Limits {
            auth_timeout: Duration::from_millis(300),
            ..Limits::default()
        },
    );
    // Sends half a request head and stalls: closed after the auth timeout,
    // long before the 10 s idle timeout.
    let mut stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream
        .write_all(b"POST /mcp HTTP/1.1\r\nX-A: aaaa")
        .unwrap();
    let started = std::time::Instant::now();
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    assert!(bytes.is_empty(), "{bytes:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    // A connection that passed the token check stays open between requests.
    let mut stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\r\n{INIT}",
        s.host(),
        s.bearer(),
        INIT.len()
    );
    stream.write_all(raw.as_bytes()).unwrap();
    assert_eq!(read_one_answer(&mut stream).status, 200);
    std::thread::sleep(Duration::from_millis(600));
    stream.write_all(raw.as_bytes()).unwrap();
    assert_eq!(read_one_answer(&mut stream).status, 200);
}

/// Waits until the server's log holds `needle`, and returns the log.
fn log_with(s: &TestServer, needle: &str) -> String {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let text = std::fs::read_to_string(s.dir.join("server.log")).unwrap_or_default();
        if text.contains(needle) || std::time::Instant::now() > deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn refusals_are_summed_up_in_the_log() {
    let s = start(
        true,
        Limits {
            log_interval: Duration::from_millis(400),
            ..Limits::default()
        },
    );
    for _ in 0..30 {
        post_raw(s.port, PATH, &[s.host()], INIT).refusal(401, "unauthorized");
    }
    for _ in 0..5 {
        post_raw(s.port, PATH, &["Host: evil.example".to_owned()], INIT).refusal(403, "forbidden");
    }
    let log = log_with(&s, "requests refused");
    let lines: Vec<_> = log.lines().filter(|l| l.contains("refused")).collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].contains("35 requests refused"), "{log}");
    assert!(
        lines[0].contains("30 missing or wrong token (401)"),
        "{log}"
    );
    assert!(
        lines[0].contains("5 Host or Origin not this computer (403)"),
        "{log}"
    );
    assert!(!log.contains("evil"), "{log}");
    // A quiet interval writes nothing.
    std::thread::sleep(Duration::from_millis(900));
    let again = std::fs::read_to_string(s.dir.join("server.log")).unwrap();
    assert_eq!(again.lines().filter(|l| l.contains("refused")).count(), 1);
}

#[test]
fn transport_errors_are_json_and_echo_nothing() {
    let s = start(true, Limits::default());
    let p = s.port;
    // Not JSON at all.
    let a = s.post("{\"jsonrpc\": SECRET-NOT-JSON");
    let v = a.refusal(400, "bad_request");
    assert!(!v.to_string().contains("SECRET"), "{v}");
    // A protocol version nobody knows.
    let a = s.post_with(
        &["MCP-Protocol-Version: 1999-SECRET".to_owned()],
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
    );
    assert_eq!(a.status, 400, "{}", a.text());
    assert!(!a.text().contains("SECRET"), "{}", a.text());
    assert_eq!(a.header("content-type"), Some("application/json"));
    // Other methods.
    for method in ["GET", "DELETE", "PUT"] {
        let raw = format!(
            "{method} {PATH} HTTP/1.1\r\n{}\r\n{}\r\nAccept: application/json, text/event-stream\r\n\
             Connection: close\r\n\r\n",
            s.host(),
            s.bearer()
        );
        let a = exchange(p, raw.as_bytes());
        a.refusal(405, "method_not_allowed");
        assert_eq!(a.header("allow"), Some("POST"));
    }
    // Wrong headers.
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: text/plain\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: 2\r\n\
         Connection: close\r\n\r\n{{}}",
        s.host(),
        s.bearer()
    );
    exchange(p, raw.as_bytes()).refusal(415, "unsupported_media_type");
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: application/json\r\n\
         Accept: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}",
        s.host(),
        s.bearer()
    );
    exchange(p, raw.as_bytes()).refusal(406, "not_acceptable");
    // A broken chunked body.
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nTransfer-Encoding: chunked\r\n\
         Connection: close\r\n\r\nzz-SECRET\r\n{{}}\r\n0\r\n\r\n",
        s.host(),
        s.bearer()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", p)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes);
    let a = parse_answer(&bytes);
    a.refusal(400, "bad_request");
    assert!(!a.text().contains("SECRET"));
}

#[test]
fn every_tool_answers_with_plain_json() {
    // rmcp falls back to an event stream, outside the work slot and the
    // request timeout, if a tool sends a notification before its result.
    // None may: every tool's answer must be one plain JSON response.
    let s = start(false, Limits::default());
    let version = "MCP-Protocol-Version: 2025-11-25".to_owned();
    for (id, command) in carrysheet_commands::table::mcp_tools().enumerate() {
        let body = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": command.name, "arguments": { "path": "book.xlsx" } },
        });
        let a = s.post_with(std::slice::from_ref(&version), &body.to_string());
        assert_eq!(a.status, 200, "{}: {}", command.name, a.text());
        assert_eq!(
            a.header("content-type"),
            Some("application/json"),
            "{} answered with an event stream",
            command.name
        );
        assert_eq!(a.json()["id"], id, "{}", command.name);
    }
}

#[test]
fn bind_errors_say_what_to_do() {
    use std::io::{Error, ErrorKind};
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap();
    let e = std::net::TcpListener::bind(address).unwrap_err();
    let e = bind_error(address, &e);
    assert_eq!(e.code, ErrorCode::Io);
    assert!(e.fix.contains("pick another one"), "{}", e.fix);
    let e = bind_error(address, &Error::from(ErrorKind::PermissionDenied));
    assert!(e.fix.contains("above 1024"), "{}", e.fix);
    let e = bind_error(address, &Error::from(ErrorKind::AddrNotAvailable));
    assert!(e.fix.contains("127.0.0.1:PORT"), "{}", e.fix);
}

#[test]
fn token_file_is_removed_only_while_it_holds_the_token() {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "carrysheet-token-rm-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("token");
    let token = Token::generate().unwrap();
    write_token_file(&path, &token).unwrap();
    std::fs::write(&path, "someone else's").unwrap();
    assert!(!remove_token_file(&path, token.reveal()));
    assert!(path.exists());
    write_token_file(&path, &token).unwrap();
    assert!(remove_token_file(&path, token.reveal()));
    assert!(!path.exists());
    assert!(!remove_token_file(&path, token.reveal()));
    let _ = std::fs::remove_dir_all(&dir);
}

impl TestServer {
    /// Stops the server and waits for it, as at Ctrl+C.
    fn stop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Opens connections that never send anything, about `per_second` in all
/// over `threads` threads, each held until the server closes it, until
/// `stop` is set. Returns how many were opened.
fn churn(
    port: u16,
    threads: usize,
    per_second: u32,
    stop: Arc<AtomicBool>,
) -> Vec<std::thread::JoinHandle<usize>> {
    let pause = Duration::from_secs(1) * u32::try_from(threads).unwrap() / per_second;
    (0..threads)
        .map(|_| {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut held: std::collections::VecDeque<TcpStream> = Default::default();
                let mut opened = 0;
                while !stop.load(Ordering::SeqCst) {
                    if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)) {
                        let _ = stream.set_nonblocking(true);
                        held.push_back(stream);
                        opened += 1;
                    }
                    if opened % 32 == 0 {
                        // Let go of the ones the server has closed.
                        held.retain(|mut s| {
                            let mut b = [0u8; 1];
                            matches!(s.read(&mut b), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
                        });
                    }
                    while held.len() > 400 {
                        held.pop_front();
                    }
                    std::thread::sleep(pause);
                }
                opened
            })
        })
        .collect()
}

#[test]
fn churning_connections_cannot_lock_out_a_waiting_client() {
    let mut s = start(true, Limits::default());
    let ping = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    for threads in [2, 16] {
        // About the rate a script reaches on this machine (2,500 to 3,000
        // a second): with a 50 ms wait, about 150 arrive in between, fewer
        // than the 256 connections the server keeps.
        let stop = Arc::new(AtomicBool::new(false));
        let churners = churn(s.port, threads, 3000, stop.clone());
        std::thread::sleep(Duration::from_secs(1));
        let mut failed = Vec::new();
        for delay in [0, 20, 50] {
            for _ in 0..10 {
                let mut stream = TcpStream::connect(("127.0.0.1", s.port)).unwrap();
                stream.set_read_timeout(Some(WAIT)).unwrap();
                std::thread::sleep(Duration::from_millis(delay));
                let raw = format!(
                    "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: application/json\r\n\
                     Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{ping}",
                    s.host(),
                    s.bearer(),
                    ping.len()
                );
                let _ = stream.write_all(raw.as_bytes());
                let mut bytes = Vec::new();
                let _ = stream.read_to_end(&mut bytes);
                if !bytes.starts_with(b"HTTP/1.1 200") {
                    failed.push(delay);
                }
            }
        }
        stop.store(true, Ordering::SeqCst);
        let opened: usize = churners.into_iter().map(|t| t.join().unwrap()).sum();
        assert!(opened > 1000, "the churn opened only {opened} connections");
        assert!(
            failed.is_empty(),
            "{threads} threads: failed with delays {failed:?}"
        );
    }
    // The summary at shutdown counts the connections closed to make room.
    s.stop();
    let log = log_with(&s, "closed to make room");
    assert!(
        log.contains("closed to make room"),
        "no eviction happened: {log}"
    );
}

/// A tools/call with about `bytes` of padding, so its body is that large.
fn padded_call(id: u64, bytes: usize) -> String {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": { "name": "feedback", "arguments": { "kind": "idea", "message": "x", "pad": "p".repeat(bytes) } },
    })
    .to_string()
}

#[test]
fn big_bodies_share_a_budget() {
    let s = start_slow(
        true,
        Limits {
            max_body_bytes: 64 * 1024,
            body_budget_bytes: 128 * 1024,
            ..Limits::default()
        },
        1000,
    );
    // Two 60 KiB calls, abandoned at once, keep their share while they run.
    for id in 0..2 {
        assert_eq!(
            post_and_leave(&s, &padded_call(id, 60 * 1024), Duration::from_millis(100)),
            None
        );
    }
    std::thread::sleep(Duration::from_millis(100));
    // A third big one does not fit (refused on its head, before its body
    // is read); a small one does.
    let version = ["MCP-Protocol-Version: 2025-11-25".to_owned()];
    let raw = format!(
        "POST {PATH} HTTP/1.1\r\n{}\r\n{}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        s.host(),
        s.bearer(),
        60 * 1024
    );
    exchange(s.port, raw.as_bytes()).refusal(503, "busy");
    assert_eq!(s.post(INIT).status, 200);
    // When the work ends, the budget is free again.
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        s.post_with(&version, &padded_call(4, 60 * 1024)).status,
        200
    );
}

#[test]
fn header_mismatch_errors_echo_nothing() {
    let s = start(true, Limits::default());
    let call = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"feedback","arguments":{"kind":"bug","message":"x"}}}"#;
    let ping = r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#;
    for (headers, body) in [
        (
            vec![
                "MCP-Protocol-Version: 2026-07-28".to_owned(),
                "Mcp-Method: ZZMARKER<script>".to_owned(),
            ],
            ping,
        ),
        (
            vec![
                "MCP-Protocol-Version: 2026-07-28".to_owned(),
                "Mcp-Method: tools/call".to_owned(),
                "Mcp-Name: ZZMARKER<script>".to_owned(),
            ],
            call,
        ),
        (vec!["MCP-Protocol-Version: ZZMARKER".to_owned()], ping),
    ] {
        let a = s.post_with(&headers, body);
        assert!(a.status >= 400, "{headers:?}: {}", a.text());
        assert!(!a.text().contains("ZZMARKER"), "{headers:?}: {}", a.text());
        assert_eq!(a.header("content-type"), Some("application/json"));
        let v = a.json();
        if v.get("jsonrpc").is_some() {
            assert_eq!(v["id"], 7, "{v}");
            assert!(v["error"]["code"].is_i64(), "{v}");
        }
    }
}

#[test]
fn the_last_summary_tells_the_real_time() {
    let mut s = start(true, Limits::default());
    for _ in 0..3 {
        post_raw(s.port, PATH, &[s.host()], INIT).refusal(401, "unauthorized");
    }
    s.stop();
    let log = log_with(&s, "requests refused");
    let line = log
        .lines()
        .find(|l| l.contains("requests refused"))
        .unwrap();
    assert!(line.contains("3 requests refused"), "{line}");
    assert!(!line.contains("in the last 60 s"), "{line}");
}

#[test]
fn shutdown_waits_for_running_calls() {
    let mut s = start_slow(true, Limits::default(), 800);
    assert_eq!(
        post_and_leave(&s, &feedback_call(1), Duration::from_millis(100)),
        None
    );
    s.stop();
    let log = log_with(&s, "tools/call feedback");
    assert!(
        log.contains("waiting up to 5 s for 1 running tool calls"),
        "{log}"
    );
    assert!(log.contains("tools/call feedback: ok"), "{log}");
    assert!(!log.contains("still running"), "{log}");

    // A call that outlasts the wait is logged.
    let mut s = start_slow(
        true,
        Limits {
            shutdown_wait: Duration::from_millis(200),
            ..Limits::default()
        },
        3000,
    );
    assert_eq!(
        post_and_leave(&s, &feedback_call(1), Duration::from_millis(100)),
        None
    );
    s.stop();
    let log = log_with(&s, "still running");
    assert!(
        log.contains("1 tool calls were still running and stop with the server"),
        "{log}"
    );
}

#[test]
fn jsonrpc_errors_echo_nothing_but_the_id() {
    let s = start(true, Limits::default());
    let version = ["MCP-Protocol-Version: 2025-11-25".to_owned()];
    for body in [
        r#"{"jsonrpc":"2.0","id":7,"method":"ZZMARKER<script>"}"#,
        r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":5,"ZZMARKER":1}}"#,
    ] {
        let a = s.post_with(&version, body);
        assert!(!a.text().contains("ZZMARKER"), "{}", a.text());
        let v = a.json();
        assert_eq!(v["id"], 7, "{v}");
        assert!(v["error"]["code"].is_i64(), "{v}");
        assert!(v["error"]["message"].is_string(), "{v}");
    }
    // Results pass unchanged.
    let a = s.post(INIT);
    assert_eq!(a.json()["result"]["serverInfo"]["name"], "carrysheet");
}

/// Fewer open files allowed means fewer connections, leaving room for the
/// rest of the server; never fewer than 8.
#[test]
fn connections_follow_the_file_limit() {
    let limit = |now| Some(FileLimit { before: now, now });
    assert_eq!(connections_for(None, 256), 256);
    assert_eq!(connections_for(limit(4096), 256), 256);
    assert_eq!(connections_for(limit(256), 256), 192);
    assert_eq!(connections_for(limit(64), 256), 8);
    assert_eq!(connections_for(limit(10), 256), 8);
    assert_eq!(connections_for(limit(u64::MAX), 256), 256);
}

/// Failed accepts go into the summary, one count and the last reason, not
/// one line each.
#[test]
fn accept_failures_are_summed_up() {
    let counts = Counts::default();
    assert_eq!(counts.summary(), None);
    for _ in 0..500 {
        counts.accept_failed(&std::io::Error::from_raw_os_error(24));
    }
    let line = counts.summary().unwrap();
    assert!(
        line.contains("500 connections could not be accepted (last: "),
        "{line}"
    );
    assert!(line.len() < 200, "{line}");
    assert_eq!(counts.summary(), None);
}
