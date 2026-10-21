//! `mcp serve --http`: the MCP server over streamable HTTP, on this computer
//! only.
//!
//! A web page can reach a server on 127.0.0.1 through DNS rebinding: it
//! points its own host name at 127.0.0.1 and then sends requests from the
//! browser. So every request passes these checks, in this order, before rmcp
//! sees it:
//!
//! 1. `Host` is `127.0.0.1:PORT`, `localhost:PORT` or `[::1]:PORT`, and
//!    `Origin` is absent or the same with `http://`. Anything else: 403.
//! 2. `Authorization: Bearer <token>`, with the 256-bit token made at start
//!    from the operating system's random source, compared in constant time.
//!    Missing or wrong: 401.
//! 3. A POST (405) to `/mcp` (404) of JSON (415) from a client that accepts
//!    JSON and event streams (406), with a body of at most 1 MiB (413) that
//!    arrives within 10 s (408).
//! 4. Then it takes one of [`Limits::max_requests`] work slots, or gets 503.
//!    A tool call keeps its slot until its work ends, even when the client
//!    has gone or the request timed out (504), so abandoned calls cannot pile
//!    up threads.
//!
//! Connections are limited too. One that has not passed the token check
//! within [`Limits::auth_timeout`] is closed, every refusal closes its
//! connection, and when all [`Limits::max_connections`] are open, the oldest
//! one without a token check makes room. So clients without the token cannot
//! keep a real client out, as long as fewer than that many connections
//! arrive between a client's connect and its request.
//!
//! Bodies are limited to 1 MiB and share a budget of 2 MiB while they are
//! read, parsed and worked on: parsing JSON takes many times a body's size.
//!
//! Refusals are short JSON errors that never repeat anything from the
//! request, and rmcp's own plain-text errors are replaced by the same kind.
//! The log gets one summary of refusals per [`Limits::log_interval`], not a
//! line each. No response carries CORS headers, so a browser page cannot
//! read one even when a request gets through. The server listens on
//! 127.0.0.1 or [::1] only; [`parse_address`] refuses every other address.
//!
//! rmcp runs stateless (no sessions, as on stdio, where a workbook is named
//! by its path in every call) and answers with plain JSON. Both lifecycles
//! work: `initialize` for 2025-11-25 and older, and `server/discover` with
//! metadata on each request for 2026-07-28.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fs::OpenOptions;
use std::future::Future;
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use carrysheet_commands::paths::display_path;
use carrysheet_commands::{CommandError, ErrorCode};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::{TokioIo, TokioTimer};
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, oneshot};

use crate::log::Log;
use crate::mcp::Server;

/// The one path the server answers on.
pub const PATH: &str = "/mcp";

type Body = BoxBody<Bytes, Infallible>;

/// Limits on what one client can make the server do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest request body. Workbooks are named by path, so no real
    /// call needs more than a few kilobytes.
    pub max_body_bytes: usize,
    /// Request bodies, in bytes, that may be read and parsed at once. A
    /// request reserves its declared length (or `max_body_bytes` when it
    /// gives none) before its body is read, and keeps that share with its
    /// work slot; parsing JSON takes many times its size in memory.
    pub body_budget_bytes: usize,
    /// Work slots: requests handled at once. More get 503.
    pub max_requests: usize,
    /// Open connections. At the limit, the oldest one that has not passed
    /// the token check is closed; if every one has, the new one is. A client
    /// that waits after connecting is safe while fewer than this many new
    /// connections arrive in the meantime, so the limit is high: a waiting
    /// connection costs only a socket.
    pub max_connections: usize,
    /// A connection must pass the token check within this time.
    pub auth_timeout: Duration,
    /// A connection that sends no complete request head for this long,
    /// including between requests on a kept-alive connection, is closed.
    pub idle_timeout: Duration,
    /// The whole request body must arrive within this time; then 408.
    pub body_timeout: Duration,
    /// The longest the server works on a request before answering 504.
    pub request_timeout: Duration,
    /// How often refused requests are summed up in the log.
    pub log_interval: Duration,
    /// At shutdown, how long to wait for running tool calls to end.
    pub shutdown_wait: Duration,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_body_bytes: 1024 * 1024,
            body_budget_bytes: 2 * 1024 * 1024,
            max_requests: 8,
            max_connections: 256,
            auth_timeout: Duration::from_secs(3),
            idle_timeout: Duration::from_secs(10),
            body_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(120),
            log_interval: Duration::from_secs(60),
            shutdown_wait: Duration::from_secs(5),
        }
    }
}

/// Open files kept free for everything but connections: stdin, stdout and
/// stderr, the log, the listener, tokio's own handles, and the workbooks
/// that running tool calls have open.
const RESERVED_FILES: u64 = 64;

/// The soft open-file limit is raised to at most this. Enough for
/// [`Limits::max_connections`] and [`RESERVED_FILES`] with room to spare;
/// macOS refuses an unlimited soft limit, so ask for a number. Unix only,
/// like the limit itself.
#[cfg(unix)]
const WANTED_FILES: u64 = 4096;

/// The open-file limit at start, and after raising it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileLimit {
    /// The soft limit before.
    pub before: u64,
    /// The soft limit now.
    pub now: u64,
}

/// Raises this process's soft open-file limit toward its hard limit, up to
/// [`WANTED_FILES`]. macOS starts programs with 256, which would run out of
/// handles before [`Limits::max_connections`] connections are open, and
/// then the oldest connection without a token is never closed to make room.
/// `None` where there is no such limit (Windows), or it cannot be read.
pub fn raise_file_limit() -> Option<FileLimit> {
    #[cfg(unix)]
    {
        use rustix::process::{Resource, getrlimit, setrlimit};
        let limit = getrlimit(Resource::Nofile);
        // `None` is unlimited.
        let before = limit.current.unwrap_or(u64::MAX);
        let hard = limit.maximum.unwrap_or(u64::MAX);
        let wanted = hard.min(WANTED_FILES);
        let mut now = before;
        if before < wanted {
            let raised = rustix::process::Rlimit {
                current: Some(wanted),
                maximum: limit.maximum,
            };
            if setrlimit(Resource::Nofile, raised).is_ok() {
                now = wanted;
            }
        }
        Some(FileLimit { before, now })
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// How many connections may be open with `files` open files allowed: at
/// most `wanted`, and [`RESERVED_FILES`] fewer than the limit, so that a
/// new connection always gets accepted and the oldest one without a token
/// can be closed to make room for it. Never fewer than 8.
pub fn connections_for(files: Option<FileLimit>, wanted: usize) -> usize {
    let Some(files) = files else {
        return wanted;
    };
    let room = usize::try_from(files.now.saturating_sub(RESERVED_FILES)).unwrap_or(usize::MAX);
    room.min(wanted).max(8)
}

/// Reads `--http`: `127.0.0.1:PORT` or `[::1]:PORT`, nothing else.
pub fn parse_address(text: &str) -> Result<SocketAddr, CommandError> {
    let fix = "Use --http 127.0.0.1:PORT or --http [::1]:PORT (port 0 picks a free port). \
               To reach it from another machine, use an SSH tunnel.";
    let shown: String = text.chars().take(80).collect();
    let addr: SocketAddr = text.trim().parse().map_err(|_| {
        CommandError::new(
            ErrorCode::Usage,
            format!("--http {shown} is not an IP address with a port"),
            "--http takes the loopback address and a port, like 127.0.0.1:8080",
            fix,
        )
    })?;
    let loopback = match addr.ip() {
        IpAddr::V4(ip) => ip == Ipv4Addr::LOCALHOST,
        IpAddr::V6(ip) => ip == Ipv6Addr::LOCALHOST,
    };
    if loopback {
        Ok(addr)
    } else {
        Err(CommandError::new(
            ErrorCode::Usage,
            format!("--http {addr} is not 127.0.0.1 or [::1]"),
            "on any other address other machines could connect, and the protection against web \
             pages (DNS rebinding) relies on the server being reachable from this computer only",
            fix,
        ))
    }
}

/// The secret clients send as `Authorization: Bearer <token>`: 256 random
/// bits as 64 lowercase hex digits. `Debug` never shows it.
pub struct Token {
    hex: String,
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(hidden)")
    }
}

impl Token {
    /// A new token from the operating system's random source.
    pub fn generate() -> Result<Token, CommandError> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| {
            CommandError::new(
                ErrorCode::Internal,
                "no token could be made for --http",
                format!("the operating system's random source failed: {e}"),
                "Try again; if it keeps failing, use stdio (leave out --http).",
            )
        })?;
        let hex = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Token { hex })
    }

    /// The token, to show once or write to `--token-file`.
    pub fn reveal(&self) -> &str {
        &self.hex
    }

    /// True when an `Authorization` value is `Bearer <this token>`. The
    /// comparison takes the same time wherever the first difference is.
    fn accepts(&self, value: &[u8]) -> bool {
        const SCHEME: &[u8] = b"bearer ";
        if value.len() < SCHEME.len() || !value[..SCHEME.len()].eq_ignore_ascii_case(SCHEME) {
            return false;
        }
        let given = value[SCHEME.len()..].trim_ascii_start();
        bool::from(given.ct_eq(self.hex.as_bytes()))
    }
}

/// Writes the token to `path`, readable by its owner only (mode 0600) on
/// Linux and macOS; on Windows the file gets the folder's default
/// permissions. The token goes into a new file beside `path`, which is then
/// renamed over it, so a file or link already at `path` is replaced, never
/// written through.
pub fn write_token_file(path: &Path, token: &Token) -> Result<(), CommandError> {
    let fail = |why: String| {
        CommandError::new(
            ErrorCode::Io,
            format!("cannot write the token file {}", display_path(path)),
            why,
            "Give --token-file a path in a folder you can write to, or leave it out to see the token on stderr.",
        )
    };
    let Some(name) = path.file_name() else {
        return Err(fail("the path names a folder, not a file".into()));
    };
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        name.to_string_lossy(),
        std::process::id()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options.open(&temp).and_then(|mut file| {
        file.write_all(token.reveal().as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    });
    written.map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        fail(e.to_string())
    })
}

/// The error for a failed bind, with a fix that fits the reason.
pub fn bind_error(address: SocketAddr, e: &std::io::Error) -> CommandError {
    use std::io::ErrorKind;
    let fix = match e.kind() {
        ErrorKind::AddrInUse => {
            "Another program uses this port: pick another one, or port 0 to let the system choose a free one."
        }
        ErrorKind::PermissionDenied => {
            "Ports below 1024 need special rights: use a port above 1024, or port 0 for a free one."
        }
        ErrorKind::AddrNotAvailable => {
            "This computer cannot listen on that address (IPv6 may be turned off): use --http 127.0.0.1:PORT."
        }
        _ => "Pick another port, or port 0 to let the system choose a free one.",
    };
    CommandError::new(
        ErrorCode::Io,
        format!("cannot listen on {address}"),
        e.to_string(),
        fix,
    )
}

/// Removes the token file when the server stops, but only if it still holds
/// this server's token: a file someone has since replaced is left alone.
/// True when it was removed.
pub fn remove_token_file(path: &Path, token: &str) -> bool {
    match std::fs::read(path) {
        Ok(content) if bool::from(content.ct_eq(token.as_bytes())) => {
            std::fs::remove_file(path).is_ok()
        }
        _ => false,
    }
}

/// What a request holds while it runs: a work slot and its share of the
/// body budget. Released when dropped.
pub struct Held {
    _work: OwnedSemaphorePermit,
    _body: OwnedSemaphorePermit,
}

/// A request's [`Held`] permits, handed to the tool call through the
/// request's extensions. The call takes them and holds them until its work
/// ends, so the slot and the memory of the parsed arguments stay counted; if
/// no call takes them, they are released with the request.
#[derive(Clone)]
pub struct WorkSlot(Arc<Mutex<Option<Held>>>);

impl WorkSlot {
    fn new(held: Held) -> WorkSlot {
        WorkSlot(Arc::new(Mutex::new(Some(held))))
    }

    /// Takes the permits; `None` if they were already taken.
    pub fn take(&self) -> Option<Held> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// The body budget is counted in KiB, so a 1 MiB body is 1024 permits.
const BUDGET_UNIT: usize = 1024;

/// Budget permits for a body of `bytes`: at least one.
fn budget_units(bytes: usize) -> u32 {
    u32::try_from(bytes.div_ceil(BUDGET_UNIT).max(1)).unwrap_or(u32::MAX)
}

/// Why a request was refused. Each has a fixed answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    /// `Host` or `Origin` is not this server on loopback.
    Forbidden,
    /// No token, or the wrong one.
    Unauthorized,
    /// Not `/mcp`.
    NotFound,
    /// Not a POST.
    MethodNotAllowed,
    /// `Accept` lacks JSON or event streams.
    NotAcceptable,
    /// `Content-Type` is not JSON.
    UnsupportedMediaType,
    /// The body did not arrive in time.
    BodyTimeout,
    /// The body is over the limit.
    TooLarge,
    /// Not one JSON-RPC message, or an MCP header that does not match it.
    BadRequest,
    /// Every work slot is taken.
    Busy,
    /// The work took too long.
    Timeout,
    /// The transport failed.
    Internal,
}

impl Refusal {
    const ALL: [Refusal; 12] = [
        Refusal::Forbidden,
        Refusal::Unauthorized,
        Refusal::NotFound,
        Refusal::MethodNotAllowed,
        Refusal::NotAcceptable,
        Refusal::UnsupportedMediaType,
        Refusal::BodyTimeout,
        Refusal::TooLarge,
        Refusal::BadRequest,
        Refusal::Busy,
        Refusal::Timeout,
        Refusal::Internal,
    ];

    fn status(self) -> StatusCode {
        match self {
            Refusal::Forbidden => StatusCode::FORBIDDEN,
            Refusal::Unauthorized => StatusCode::UNAUTHORIZED,
            Refusal::NotFound => StatusCode::NOT_FOUND,
            Refusal::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Refusal::NotAcceptable => StatusCode::NOT_ACCEPTABLE,
            Refusal::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Refusal::BodyTimeout => StatusCode::REQUEST_TIMEOUT,
            Refusal::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Refusal::BadRequest => StatusCode::BAD_REQUEST,
            Refusal::Busy => StatusCode::SERVICE_UNAVAILABLE,
            Refusal::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Refusal::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// The JSON error. Fixed text only: nothing from the request.
    fn response(self, limits: &Limits) -> Response<Body> {
        let seconds = |d: Duration| d.as_secs_f64();
        let (code, what, fix, why) = match self {
            Refusal::Forbidden => (
                "forbidden",
                "this request did not come from an MCP client on this computer".to_owned(),
                "Connect to 127.0.0.1, localhost or [::1] from a local MCP client; web pages cannot use this server.",
                "its Host or Origin header names another site, which is how web pages attack local servers (DNS rebinding)".to_owned(),
            ),
            Refusal::Unauthorized => (
                "unauthorized",
                "the token is missing or wrong".to_owned(),
                "Send the header Authorization: Bearer <token>, with the token from the server's stderr or --token-file.",
                "every request needs the token that mcp serve --http made at start".to_owned(),
            ),
            Refusal::NotFound => (
                "not_found",
                "there is nothing at this path".to_owned(),
                "Send MCP requests to /mcp.",
                "the server answers on /mcp only".to_owned(),
            ),
            Refusal::MethodNotAllowed => (
                "method_not_allowed",
                "this server takes POST requests only".to_owned(),
                "Send each MCP message as a POST to /mcp.",
                "it runs without sessions, so there is no event stream to GET and no session to DELETE".to_owned(),
            ),
            Refusal::NotAcceptable => (
                "not_acceptable",
                "the request does not accept the answer's formats".to_owned(),
                "Send the header Accept: application/json, text/event-stream.",
                "MCP clients must accept both JSON and event streams".to_owned(),
            ),
            Refusal::UnsupportedMediaType => (
                "unsupported_media_type",
                "the request body is not marked as JSON".to_owned(),
                "Send the header Content-Type: application/json.",
                "MCP messages are JSON".to_owned(),
            ),
            Refusal::BodyTimeout => (
                "body_timeout",
                format!(
                    "the request body did not arrive within {} seconds",
                    seconds(limits.body_timeout)
                ),
                "Send the whole body at once.",
                "a body that arrives slowly would hold the server".to_owned(),
            ),
            Refusal::TooLarge => (
                "too_large",
                format!(
                    "the request body is larger than {} bytes",
                    limits.max_body_bytes
                ),
                "Send a smaller request; name workbooks by path instead of sending their contents.",
                "large bodies are refused so one client cannot use up the server's memory".to_owned(),
            ),
            Refusal::BadRequest => (
                "bad_request",
                "the request is not a valid MCP message".to_owned(),
                "Send one JSON-RPC message as JSON, with an MCP-Protocol-Version header the server supports.",
                "the body is not one JSON-RPC message, or an MCP header does not match it".to_owned(),
            ),
            Refusal::Busy => (
                "busy",
                "the server is busy".to_owned(),
                "Try again in a moment.",
                format!(
                    "calls still running hold all {} work slots or the {} KiB body budget",
                    limits.max_requests,
                    limits.body_budget_bytes / BUDGET_UNIT
                ),
            ),
            Refusal::Timeout => (
                "timeout",
                format!(
                    "the request took longer than {} seconds",
                    seconds(limits.request_timeout)
                ),
                "Try again with a smaller range or file, or use stdio, which has no time limit.",
                "requests over HTTP have a time limit; the work goes on and keeps its slot until it ends".to_owned(),
            ),
            Refusal::Internal => (
                "internal",
                "the server failed to answer".to_owned(),
                "Try again; if it happens again, report it with `carrysheet-cli feedback bug`.",
                "the MCP transport reported an error".to_owned(),
            ),
        };
        let body = json!({
            "ok": false,
            "error": { "code": code, "what": what, "fix": fix, "why": why },
        });
        let mut response = Response::new(Full::new(Bytes::from(body.to_string())).boxed());
        *response.status_mut() = self.status();
        let headers = response.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        // A refused client gets no second try on the same connection.
        headers.insert(header::CONNECTION, HeaderValue::from_static("close"));
        match self {
            Refusal::Unauthorized => {
                headers.insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            Refusal::MethodNotAllowed => {
                headers.insert(header::ALLOW, HeaderValue::from_static("POST"));
            }
            Refusal::Busy => {
                headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            }
            _ => {}
        }
        response
    }

    /// For the log: what was wrong, never the request's own text.
    fn log_text(self) -> &'static str {
        match self {
            Refusal::Forbidden => "Host or Origin not this computer (403)",
            Refusal::Unauthorized => "missing or wrong token (401)",
            Refusal::NotFound => "path not /mcp (404)",
            Refusal::MethodNotAllowed => "not a POST (405)",
            Refusal::NotAcceptable => "Accept header lacks JSON (406)",
            Refusal::UnsupportedMediaType => "body not JSON (415)",
            Refusal::BodyTimeout => "body too slow (408)",
            Refusal::TooLarge => "body too large (413)",
            Refusal::BadRequest => "not a valid MCP message (400)",
            Refusal::Busy => "all work slots taken (503)",
            Refusal::Timeout => "took too long (504)",
            Refusal::Internal => "transport error (500)",
        }
    }

    /// Replaces rmcp's error answers, which can repeat header values, the
    /// method and parser messages from the request, with fixed text. A
    /// plain-text error becomes our JSON error; a JSON-RPC error keeps its
    /// status, id and code, with a fixed message. Results pass unchanged.
    async fn from_rmcp(response: Response<Body>) -> Result<Response<Body>, Refusal> {
        let status = response.status();
        let json = response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| v.as_bytes().starts_with(b"application/json"));
        if !json {
            if status.is_success() {
                // 202 for notifications; an event stream (see `serve`).
                return Ok(response);
            }
            return Err(match status {
                StatusCode::METHOD_NOT_ALLOWED => Refusal::MethodNotAllowed,
                StatusCode::NOT_ACCEPTABLE => Refusal::NotAcceptable,
                StatusCode::PAYLOAD_TOO_LARGE => Refusal::TooLarge,
                StatusCode::FORBIDDEN => Refusal::Forbidden,
                StatusCode::NOT_FOUND => Refusal::NotFound,
                // The Content-Type was checked before: a 415 from rmcp means
                // the body did not parse.
                s if s.is_client_error() => Refusal::BadRequest,
                _ => Refusal::Internal,
            });
        }
        let (parts, body) = response.into_parts();
        let Ok(body) = body.collect().await;
        let bytes = body.to_bytes();
        // Only the top-level `id` and `error` are read; a result's contents
        // are skipped without being kept.
        let peek: Peek = serde_json::from_slice(&bytes).unwrap_or_default();
        let Some(error) = peek.error else {
            if status.is_success() {
                return Ok(Response::from_parts(parts, Full::new(bytes).boxed()));
            }
            return Err(if status.is_client_error() {
                Refusal::BadRequest
            } else {
                Refusal::Internal
            });
        };
        let code = error.code.unwrap_or(-32603);
        let fixed = json!({
            "jsonrpc": "2.0",
            "id": peek.id.unwrap_or(serde_json::Value::Null),
            "error": { "code": code, "message": jsonrpc_message(code) },
        });
        let mut response = Response::new(Full::new(Bytes::from(fixed.to_string())).boxed());
        *response.status_mut() = status;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        Ok(response)
    }
}

/// The parts of a JSON-RPC answer that [`Refusal::from_rmcp`] looks at.
#[derive(Default, serde::Deserialize)]
struct Peek {
    id: Option<serde_json::Value>,
    error: Option<PeekError>,
}

#[derive(serde::Deserialize)]
struct PeekError {
    code: Option<i64>,
}

/// The fixed message for a JSON-RPC error code. The request's id is still
/// repeated, as JSON-RPC requires.
fn jsonrpc_message(code: i64) -> &'static str {
    match code {
        -32700 => "Parse error: the body is not JSON.",
        -32600 => "Invalid request: the body is not a valid JSON-RPC request.",
        -32601 => "Method not found.",
        -32602 => "Invalid params.",
        -32603 => "Internal error.",
        _ => {
            "Refused: an MCP header or the protocol version does not match the request \
             (check MCP-Protocol-Version, Mcp-Method and Mcp-Name)."
        }
    }
}

/// Counts refusals and closed connections, for one summary per interval.
struct Counts {
    /// When the last summary was taken.
    since: Mutex<std::time::Instant>,
    refused: [AtomicU64; Refusal::ALL.len()],
    /// New connections closed because every slot held a checked one.
    turned_away: AtomicU64,
    /// Unchecked connections closed to make room.
    evicted: AtomicU64,
    /// Connections that could not be accepted, and the last reason.
    accept_failed: AtomicU64,
    accept_error: Mutex<Option<String>>,
}

impl Default for Counts {
    fn default() -> Counts {
        Counts {
            since: Mutex::new(std::time::Instant::now()),
            refused: Default::default(),
            turned_away: AtomicU64::new(0),
            evicted: AtomicU64::new(0),
            accept_failed: AtomicU64::new(0),
            accept_error: Mutex::new(None),
        }
    }
}

impl Counts {
    fn refused(&self, refusal: Refusal) {
        let i = Refusal::ALL
            .iter()
            .position(|r| *r == refusal)
            .unwrap_or_default();
        self.refused[i].fetch_add(1, Ordering::Relaxed);
    }

    fn accept_failed(&self, error: &std::io::Error) {
        self.accept_failed.fetch_add(1, Ordering::Relaxed);
        *self
            .accept_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(error.to_string());
    }

    /// The summary since the last call, if anything happened, with the
    /// time that really passed.
    fn summary(&self) -> Option<String> {
        let elapsed = {
            let mut since = self.since.lock().unwrap_or_else(PoisonError::into_inner);
            let elapsed = since.elapsed();
            *since = std::time::Instant::now();
            elapsed
        };
        let mut total = 0;
        let mut parts = Vec::new();
        for (refusal, count) in Refusal::ALL.iter().zip(&self.refused) {
            let n = count.swap(0, Ordering::Relaxed);
            if n > 0 {
                total += n;
                parts.push(format!("{n} {}", refusal.log_text()));
            }
        }
        let evicted = self.evicted.swap(0, Ordering::Relaxed);
        let turned_away = self.turned_away.swap(0, Ordering::Relaxed);
        let accept_failed = self.accept_failed.swap(0, Ordering::Relaxed);
        let accept_error = self
            .accept_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if total == 0 && evicted == 0 && turned_away == 0 && accept_failed == 0 {
            return None;
        }
        let seconds = (elapsed.as_secs_f64() * 10.0).round() / 10.0;
        let mut line = format!("http: in the last {seconds} s: {total} requests refused");
        if !parts.is_empty() {
            line.push_str(&format!(" ({})", parts.join(", ")));
        }
        if evicted > 0 {
            line.push_str(&format!(
                "; {evicted} connections without a token closed to make room"
            ));
        }
        if turned_away > 0 {
            line.push_str(&format!(
                "; {turned_away} new connections closed: all slots in use"
            ));
        }
        if accept_failed > 0 {
            line.push_str(&format!(
                "; {accept_failed} connections could not be accepted (last: {})",
                accept_error.as_deref().unwrap_or("unknown error")
            ));
        }
        Some(line)
    }
}

/// The `Host` values this server accepts, for its port.
fn local_authorities(port: u16) -> [String; 3] {
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
}

/// The checks every request passes before rmcp sees it.
struct Guard {
    hosts: [String; 3],
    origins: [String; 3],
    token: Token,
    max_body_bytes: usize,
}

impl Guard {
    fn new(port: u16, token: Token, max_body_bytes: usize) -> Guard {
        let hosts = local_authorities(port);
        let origins = hosts.clone().map(|h| format!("http://{h}"));
        Guard {
            hosts,
            origins,
            token,
            max_body_bytes,
        }
    }

    fn is_local_host(&self, value: &[u8]) -> bool {
        self.hosts
            .iter()
            .any(|h| value.eq_ignore_ascii_case(h.as_bytes()))
    }

    /// Who is asking: a local client (Host, Origin) with the token.
    fn check_caller(&self, uri: &Uri, headers: &HeaderMap) -> Result<(), Refusal> {
        let host_ok = match one(headers, &header::HOST) {
            Some(Some(host)) => self.is_local_host(host.as_bytes()),
            _ => false,
        };
        let authority_ok = uri
            .authority()
            .is_none_or(|a| self.is_local_host(a.as_str().as_bytes()));
        let origin_ok = match one(headers, &header::ORIGIN) {
            Some(None) => true,
            Some(Some(origin)) => self
                .origins
                .iter()
                .any(|o| origin.as_bytes().eq_ignore_ascii_case(o.as_bytes())),
            None => false,
        };
        if !(host_ok && authority_ok && origin_ok) {
            return Err(Refusal::Forbidden);
        }
        match one(headers, &header::AUTHORIZATION) {
            Some(Some(value)) if self.token.accepts(value.as_bytes()) => Ok(()),
            _ => Err(Refusal::Unauthorized),
        }
    }

    /// What is asked: a POST of JSON to /mcp, small enough.
    fn check_request(
        &self,
        method: &Method,
        uri: &Uri,
        headers: &HeaderMap,
    ) -> Result<(), Refusal> {
        if uri.path() != PATH {
            return Err(Refusal::NotFound);
        }
        if method != Method::POST {
            return Err(Refusal::MethodNotAllowed);
        }
        let text = |name| {
            headers
                .get(name)
                .and_then(|v: &HeaderValue| v.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase()
        };
        if !text(header::CONTENT_TYPE).starts_with("application/json") {
            return Err(Refusal::UnsupportedMediaType);
        }
        let accept = text(header::ACCEPT);
        if !(accept.contains("application/json") && accept.contains("text/event-stream")) {
            return Err(Refusal::NotAcceptable);
        }
        let declared = headers
            .get(header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        if declared.is_some_and(|n| n > self.max_body_bytes as u64) {
            return Err(Refusal::TooLarge);
        }
        Ok(())
    }
}

/// A header that may appear at most once: `Some(None)` when absent,
/// `Some(Some(value))` when there once, `None` when repeated.
fn one<'a>(headers: &'a HeaderMap, name: &header::HeaderName) -> Option<Option<&'a HeaderValue>> {
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (None, _) => Some(None),
        (Some(value), None) => Some(Some(value)),
        (Some(_), Some(_)) => None,
    }
}

/// Removes CORS headers, so a browser never lets a page read an answer.
fn strip_cors(headers: &mut HeaderMap) {
    let cors: Vec<_> = headers
        .keys()
        .filter(|k| k.as_str().starts_with("access-control-"))
        .cloned()
        .collect();
    for name in cors {
        headers.remove(name);
    }
}

/// The open connections, oldest first, and whether each has passed the
/// token check.
struct Connections {
    max: usize,
    table: Mutex<ConnectionTable>,
}

#[derive(Default)]
struct ConnectionTable {
    next: u64,
    open: BTreeMap<u64, Open>,
}

struct Open {
    checked: Arc<AtomicBool>,
    close: oneshot::Sender<()>,
}

/// A connection let in: removed from the table when dropped.
struct Admitted {
    id: u64,
    connections: Arc<Connections>,
    checked: Arc<AtomicBool>,
    closed: Option<oneshot::Receiver<()>>,
}

impl Drop for Admitted {
    fn drop(&mut self) {
        let mut table = self
            .connections
            .table
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        table.open.remove(&self.id);
    }
}

impl Connections {
    /// Lets a new connection in. At the limit, the oldest connection that
    /// has not passed the token check is closed to make room; if every one
    /// has, the new one is refused (`None`). The flag says whether one was
    /// closed.
    fn admit(self: &Arc<Self>) -> (Option<Admitted>, bool) {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        let mut evicted = false;
        if table.open.len() >= self.max {
            let oldest = table
                .open
                .iter()
                .find(|(_, o)| !o.checked.load(Ordering::SeqCst))
                .map(|(id, _)| *id);
            match oldest.and_then(|id| table.open.remove(&id)) {
                Some(open) => {
                    let _ = open.close.send(());
                    evicted = true;
                }
                None => return (None, false),
            }
        }
        let id = table.next;
        table.next += 1;
        let checked = Arc::new(AtomicBool::new(false));
        let (close, closed) = oneshot::channel();
        table.open.insert(
            id,
            Open {
                checked: checked.clone(),
                close,
            },
        );
        let admitted = Admitted {
            id,
            connections: self.clone(),
            checked,
            closed: Some(closed),
        };
        (Some(admitted), evicted)
    }
}

struct State {
    guard: Guard,
    mcp: StreamableHttpService<Server, NeverSessionManager>,
    slots: Arc<Semaphore>,
    /// The body budget, in [`BUDGET_UNIT`]s.
    body_budget: Arc<Semaphore>,
    limits: Limits,
    counts: Counts,
}

async fn handle(
    state: Arc<State>,
    checked: Arc<AtomicBool>,
    request: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let mut response = match respond(&state, &checked, request).await {
        Ok(response) => response,
        Err(refusal) => {
            state.counts.refused(refusal);
            refusal.response(&state.limits)
        }
    };
    let headers = response.headers_mut();
    strip_cors(headers);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

async fn respond(
    state: &State,
    checked: &AtomicBool,
    request: Request<Incoming>,
) -> Result<Response<Body>, Refusal> {
    state.guard.check_caller(request.uri(), request.headers())?;
    checked.store(true, Ordering::SeqCst);
    state
        .guard
        .check_request(request.method(), request.uri(), request.headers())?;
    // A share of the body budget for the declared length, before the body
    // is read; a chunked body reserves the most it may be.
    let max = state.limits.max_body_bytes;
    let declared = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map_or(max, |n| n.min(max));
    let mut budget = state
        .body_budget
        .clone()
        .try_acquire_many_owned(budget_units(declared))
        .map_err(|_| Refusal::Busy)?;
    let (mut parts, body) = request.into_parts();
    // The whole body first, under a time limit, before taking a work slot.
    let body =
        match tokio::time::timeout(state.limits.body_timeout, Limited::new(body, max).collect())
            .await
        {
            Err(_) => return Err(Refusal::BodyTimeout),
            Ok(Err(e)) if e.is::<LengthLimitError>() => return Err(Refusal::TooLarge),
            Ok(Err(_)) => return Err(Refusal::BadRequest),
            Ok(Ok(collected)) => collected.to_bytes(),
        };
    // Give back what a shorter body did not use.
    let used = budget_units(body.len()) as usize;
    if budget.num_permits() > used {
        drop(budget.split(budget.num_permits() - used));
    }
    let work = state
        .slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| Refusal::Busy)?;
    parts.extensions.insert(WorkSlot::new(Held {
        _work: work,
        _body: budget,
    }));
    let request = Request::from_parts(parts, Full::new(body));
    let response = tokio::time::timeout(state.limits.request_timeout, state.mcp.handle(request))
        .await
        .map_err(|_| Refusal::Timeout)?;
    Refusal::from_rmcp(response).await
}

/// Serves `server` on `listener` until `shutdown` completes. The listener
/// must be on loopback; see [`parse_address`].
pub async fn serve(
    listener: TcpListener,
    server: Server,
    token: Token,
    limits: Limits,
    log: Arc<Log>,
    shutdown: impl Future<Output = ()>,
) -> std::io::Result<()> {
    let port = listener.local_addr()?.port();
    let guard = Guard::new(port, token, limits.max_body_bytes);
    // rmcp's own Host, Origin and size checks, set the same: a second line
    // behind the guard.
    //
    // With `json_response`, rmcp answers a stateless request with JSON, but
    // falls back to an event stream if the handler sends a notification or
    // request before its result (rmcp 3.5.1, transport/streamable_http_server/
    // tower.rs:2354). That stream would outlive the request timeout. Our
    // tools never send one; `every_tool_answers_with_plain_json` in the tests
    // fails if that changes.
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts(guard.hosts.clone())
        .with_allowed_origins(guard.origins.clone())
        .enforce_origin_validation()
        .with_max_request_body_bytes(limits.max_body_bytes);
    let mcp = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        config,
    );
    let state = Arc::new(State {
        guard,
        mcp,
        slots: Arc::new(Semaphore::new(limits.max_requests)),
        body_budget: Arc::new(Semaphore::new(
            limits.body_budget_bytes.max(limits.max_body_bytes) / BUDGET_UNIT,
        )),
        limits,
        counts: Counts::default(),
    });
    let connections = Arc::new(Connections {
        max: limits.max_connections,
        table: Mutex::new(ConnectionTable::default()),
    });
    let mut http = hyper::server::conn::http1::Builder::new();
    http.timer(TokioTimer::new())
        .header_read_timeout(limits.idle_timeout)
        .max_headers(64);

    let reporter = {
        let state = state.clone();
        let log = log.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(limits.log_interval).await;
                if let Some(line) = state.counts.summary() {
                    log.line(&line);
                }
            }
        })
    };

    // One permit per accepted socket, held until the socket is closed. A
    // connection closed to make room keeps its socket until its task runs,
    // and on this single-threaded runtime the accept loop can take a whole
    // backlog first (2026-10-21: 2 accepts failed for lack of file handles
    // under a limit of 64). Waiting here for a socket to really close keeps
    // the sockets at most one over the connection limit.
    let sockets = Arc::new(Semaphore::new(limits.max_connections + 1));
    tokio::pin!(shutdown);
    loop {
        let socket = tokio::select! {
            () = &mut shutdown => break,
            permit = sockets.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                // Never closed.
                Err(_) => break,
            },
        };
        let accepted = tokio::select! {
            () = &mut shutdown => break,
            accepted = listener.accept() => accepted,
        };
        let stream = match accepted {
            Ok((stream, _)) => stream,
            Err(e) => {
                // Out of file handles, or a connection reset before accept:
                // count it for the summary, wait a little and go on.
                state.counts.accept_failed(&e);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let (admitted, evicted) = connections.admit();
        if evicted {
            state.counts.evicted.fetch_add(1, Ordering::Relaxed);
        }
        let Some(mut admitted) = admitted else {
            state.counts.turned_away.fetch_add(1, Ordering::Relaxed);
            drop(stream);
            continue;
        };
        let _ = stream.set_nodelay(true);
        let checked = admitted.checked.clone();
        let service = {
            let state = state.clone();
            let checked = checked.clone();
            hyper::service::service_fn(move |request| {
                handle(state.clone(), checked.clone(), request)
            })
        };
        let connection = http.serve_connection(TokioIo::new(stream), service);
        let closed = admitted.closed.take();
        let auth_timeout = limits.auth_timeout;
        tokio::spawn(async move {
            // Dropped after `connection`, which owns the socket.
            let _socket = socket;
            let _admitted = admitted;
            // Closed if no request passes the token check in time.
            let unchecked = async {
                tokio::time::sleep(auth_timeout).await;
                if checked.load(Ordering::SeqCst) {
                    std::future::pending::<()>().await;
                }
            };
            let closed = async {
                match closed {
                    Some(closed) => {
                        let _ = closed.await;
                    }
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = connection => {}
                () = closed => {}
                () = unchecked => {}
            }
        });
    }
    reporter.abort();
    if let Some(line) = state.counts.summary() {
        log.line(&line);
    }
    finish_work(&state, &log).await;
    Ok(())
}

/// At shutdown, waits up to [`Limits::shutdown_wait`] for running tool
/// calls, and logs any that were still running. Those are stopped with the
/// process. That is safe only because commands that write must save to a
/// temporary file and rename it over the target (docs/agents.md, Safety), so
/// a stopped write leaves no half-written file. No command writes files yet;
/// the first one that does must keep to this.
async fn finish_work(state: &State, log: &Log) {
    let all = state.limits.max_requests;
    let running = all - state.slots.available_permits();
    if running == 0 {
        return;
    }
    log.line(&format!(
        "waiting up to {} s for {running} running tool calls",
        state.limits.shutdown_wait.as_secs_f64()
    ));
    let permits = u32::try_from(all).unwrap_or(u32::MAX);
    let finished = tokio::time::timeout(
        state.limits.shutdown_wait,
        state.slots.acquire_many(permits),
    )
    .await;
    if finished.is_err() {
        let still = all - state.slots.available_permits();
        log.line(&format!(
            "{still} tool calls were still running and stop with the server"
        ));
    }
}

#[cfg(test)]
mod tests;
