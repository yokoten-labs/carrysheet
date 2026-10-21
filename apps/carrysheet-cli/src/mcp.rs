//! `mcp serve`: the MCP server over stdio, or over HTTP on loopback with
//! `--http` (see [`crate::http`]), built on rmcp.
//!
//! - Tools come from the command table, in its fixed order, with all four
//!   annotations set. `--read-only` leaves out every tool that changes files;
//!   over HTTP that is the default, and `--allow-writes` turns it off.
//! - A tool call goes through the same `validate_args` and [`run::run`] as
//!   the command line. Its result carries the same JSON as compact text and
//!   as `structuredContent`; a failure is `isError: true` with `fix` first.
//! - Every path argument must stay inside `--root` (default: the current
//!   folder) and is handed on resolved.
//! - stdout carries only JSON-RPC (over HTTP, nothing). The log goes to
//!   stderr or `--log-file`.
//! - Both lifecycles, as rmcp implements them: `initialize` for 2025-11-25
//!   and older, and `server/discover` with metadata on each request for
//!   2026-07-28. rmcp's default version list (2024-11-05 to 2026-07-28) is
//!   kept, so clients still on older revisions can connect.

use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use carrysheet_commands::paths::{display_path, home_dir};
use carrysheet_commands::root::Root;
use carrysheet_commands::schema::tool_definition;
use carrysheet_commands::table::{ArgType, Command, mcp_tools};
use carrysheet_commands::{CommandError, ErrorCode, validate_args};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{QuitReason, RequestContext, ServerInitializeError};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Map, Value};

use crate::http;
use crate::log::{Log, TracingToLog};
use crate::output::ok_envelope;
use crate::prompt::Context;
use crate::run;

/// Told to the client at the start; some clients show it to the model.
const INSTRUCTIONS: &str = "Carrysheet reads, checks, converts and edits spreadsheet files \
(xlsx, xlsm, xlsb, xls, csv). Call overview first on any workbook. Paths are relative to the \
server's root folder; files outside it are refused. Name the workbook by its path in every call.";

/// What `mcp serve` was started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// `--root`; without it, the current folder.
    pub root: Option<PathBuf>,
    /// Offer only tools that never change files. Over HTTP, on unless
    /// `--allow-writes`.
    pub read_only: bool,
    pub log_file: Option<PathBuf>,
    /// `--http`: a loopback address to serve on instead of stdio.
    pub http: Option<SocketAddr>,
    /// `--token-file`: where to write the HTTP token instead of stderr.
    pub token_file: Option<PathBuf>,
}

impl Options {
    /// Reads the validated `mcp serve` arguments.
    pub fn from_args(args: &Map<String, Value>) -> Result<Options, CommandError> {
        let text = |name: &str| args.get(name).and_then(Value::as_str);
        let flag = |name: &str| args.get(name).and_then(Value::as_bool) == Some(true);
        let http = text("http").map(http::parse_address).transpose()?;
        let token_file = text("token_file").map(PathBuf::from);
        let allow_writes = flag("allow_writes");
        if http.is_none() && (allow_writes || token_file.is_some()) {
            let given = if allow_writes {
                "--allow-writes"
            } else {
                "--token-file"
            };
            return Err(CommandError::new(
                ErrorCode::Usage,
                format!("{given} works only with --http"),
                "over stdio there is no token, and tools that change files are offered unless --read-only",
                format!("Add --http 127.0.0.1:PORT, or leave out {given}."),
            ));
        }
        if allow_writes && flag("read_only") {
            return Err(CommandError::new(
                ErrorCode::Usage,
                "--allow-writes and --read-only contradict each other",
                "--allow-writes offers tools that change files; --read-only hides them",
                "Give one of them. Over HTTP the server is read-only unless --allow-writes.",
            ));
        }
        Ok(Options {
            root: text("root").map(PathBuf::from),
            read_only: flag("read_only") || (http.is_some() && !allow_writes),
            log_file: text("log_file").map(PathBuf::from),
            http,
            token_file,
        })
    }
}

/// How clients reach the server; read-only mode says how to turn it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    Http,
}

/// The server: the tools it offers and the folder it may use.
#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

struct Inner {
    root: Root,
    read_only: bool,
    transport: Transport,
    #[cfg(test)]
    test_hooks: TestHooks,
    /// The offered commands, in table order, and their tool definitions.
    commands: Vec<&'static Command>,
    tools: Vec<Tool>,
    log: Arc<Log>,
}

/// Test-only: makes every tool call slow and counts calls running at once.
#[cfg(test)]
#[derive(Default)]
pub struct TestHooks {
    pub delay_ms: std::sync::atomic::AtomicU64,
    pub running: std::sync::atomic::AtomicUsize,
    pub peak: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
pub struct Running<'a>(&'a TestHooks);

#[cfg(test)]
impl TestHooks {
    fn enter(&self) -> Running<'_> {
        use std::sync::atomic::Ordering::SeqCst;
        let now = self.running.fetch_add(1, SeqCst) + 1;
        self.peak.fetch_max(now, SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(self.delay_ms.load(SeqCst)));
        Running(self)
    }
}

#[cfg(test)]
impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0
            .running
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The commands offered as tools: every MCP tool in the table; in read-only
/// mode, only those that never change files and say so in `readOnlyHint`.
pub fn offered(read_only: bool) -> Vec<&'static Command> {
    mcp_tools()
        .filter(|c| !read_only || is_read_only(c))
        .collect()
}

/// Offered in read-only mode: never changes files and is marked read-only.
fn is_read_only(command: &Command) -> bool {
    !command.changes_files && command.annotations.read_only
}

/// A command's tool definition as rmcp's type. The JSON comes from the
/// table, so nothing here can drift from `commands --json`.
fn tool(command: &Command) -> Tool {
    serde_json::from_value(tool_definition(command))
        .expect("tool definitions from the table are valid MCP tools")
}

impl Server {
    pub fn new(root: Root, read_only: bool, transport: Transport, log: Arc<Log>) -> Server {
        let commands = offered(read_only);
        let tools = commands.iter().map(|c| tool(c)).collect();
        Server {
            inner: Arc::new(Inner {
                root,
                read_only,
                transport,
                #[cfg(test)]
                test_hooks: TestHooks::default(),
                commands,
                tools,
                log,
            }),
        }
    }

    #[cfg(test)]
    pub fn test_hooks(&self) -> &TestHooks {
        &self.inner.test_hooks
    }

    /// Runs one tool call and turns the outcome into a tool result. Never a
    /// protocol error, so the client's session survives every failure.
    pub fn call(&self, name: &str, arguments: Option<Map<String, Value>>) -> CallToolResult {
        let started = Instant::now();
        #[cfg(test)]
        let _running = self.inner.test_hooks.enter();
        let outcome = self.run_tool(name, arguments);
        let status = match &outcome {
            Ok(_) => "ok".to_owned(),
            Err(e) => e.code.as_str().to_owned(),
        };
        // The name comes from the client: log it only if it is a real tool,
        // so a long or multi-line name cannot fill or forge log lines.
        let shown = if mcp_tools().any(|c| c.name == name) {
            name
        } else {
            "(unknown tool)"
        };
        self.inner.log.line(&format!(
            "tools/call {shown}: {status} in {} ms",
            started.elapsed().as_millis()
        ));
        match outcome {
            Ok(data) => CallToolResult::structured(ok_envelope(&data)),
            Err(e) => CallToolResult::structured_error(error_envelope(&e)),
        }
    }

    fn run_tool(
        &self,
        name: &str,
        arguments: Option<Map<String, Value>>,
    ) -> Result<Map<String, Value>, CommandError> {
        let Some(command) = self.inner.commands.iter().find(|c| c.name == name) else {
            return Err(self.unknown_tool(name));
        };
        let args = Value::Object(arguments.unwrap_or_default());
        let mut args = validate_args(command, &args)?;
        self.confine(command, &mut args)?;
        // An agent is reading: never ask, never wait for a terminal.
        // MCP calls keep the default time limit (limits::MAX_CALL_SECONDS).
        let ctx = Context {
            json: true,
            yes: false,
            interactive: false,
            max_seconds: None,
        };
        // Writes stay inside the root even if a link changes meanwhile.
        let _confined = carrysheet_commands::engine::save::confine_writes(&self.inner.root.dir());
        run::run(command, &args, &ctx).map(|done| done.data)
    }

    /// Replaces every path argument with the same path resolved inside the
    /// root, or refuses the call.
    fn confine(
        &self,
        command: &Command,
        args: &mut Map<String, Value>,
    ) -> Result<(), CommandError> {
        for arg in command.all_args() {
            match arg.ty {
                ArgType::Path => {
                    if let Some(Value::String(given)) = args.get(arg.name) {
                        let resolved = self.resolve(given)?;
                        args.insert(arg.name.into(), Value::String(resolved));
                    }
                }
                ArgType::ObjectList(fields) => {
                    let paths: Vec<_> = fields.iter().filter(|f| f.ty == ArgType::Path).collect();
                    if paths.is_empty() {
                        continue;
                    }
                    let Some(Value::Array(items)) = args.get_mut(arg.name) else {
                        continue;
                    };
                    for item in items.iter_mut().filter_map(Value::as_object_mut) {
                        for field in &paths {
                            if let Some(Value::String(given)) = item.get(field.name) {
                                let resolved = self.resolve(given)?;
                                item.insert(field.name.into(), Value::String(resolved));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn resolve(&self, given: &str) -> Result<String, CommandError> {
        let path = self.inner.root.resolve(given)?;
        path.into_os_string().into_string().map_err(|_| {
            CommandError::invalid_argument(
                "path",
                "the resolved path is not valid Unicode text",
                "Rename the file or folder so its name is plain text.",
            )
        })
    }

    fn unknown_tool(&self, name: &str) -> CommandError {
        let hidden =
            self.inner.read_only && mcp_tools().any(|c| c.name == name && !is_read_only(c));
        let names: Vec<_> = self.inner.commands.iter().map(|c| c.name).collect();
        if hidden {
            let (why, restart) = match self.inner.transport {
                Transport::Stdio => (
                    "the server was started with --read-only, which hides tools that change files",
                    "restart the server without --read-only",
                ),
                Transport::Http => (
                    "over HTTP the server hides tools that change files unless started with --allow-writes",
                    "restart the server with --allow-writes",
                ),
            };
            CommandError::new(
                ErrorCode::InvalidArgument,
                format!("the tool `{name}` is not offered in read-only mode"),
                why,
                format!(
                    "Use one of: {}. To change files, {restart}.",
                    names.join(", ")
                ),
            )
        } else {
            CommandError::new(
                ErrorCode::InvalidArgument,
                format!("there is no tool named `{name}`"),
                "the name is not in this server's tool list",
                format!("Use one of: {}.", names.join(", ")),
            )
        }
    }
}

/// The longest `what` a tool error carries, so `fix` always starts within
/// the first 200 characters: some clients keep only the start of an error.
const MAX_WHAT: usize = 100;

/// The longest `fix` and `why` a tool error carries. Errors name the
/// argument or value at fault, which comes from the client and can be any
/// length.
const MAX_DETAIL: usize = 200;

/// The command line's error envelope (`code`, `what`, `fix`, `why`, home
/// folder shown as `~`), with `what` cut to [`MAX_WHAT`] characters and
/// `fix` and `why` to [`MAX_DETAIL`].
pub fn error_envelope(e: &CommandError) -> Value {
    let mut envelope = e.to_envelope();
    for (part, max) in [
        ("/error/what", MAX_WHAT),
        ("/error/fix", MAX_DETAIL),
        ("/error/why", MAX_DETAIL),
    ] {
        if let Some(Value::String(text)) = envelope.pointer_mut(part)
            && text.chars().count() > max
        {
            let cut: String = text.chars().take(max - 3).collect();
            *text = format!("{}...", cut.trim_end());
        }
    }
    envelope
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        let mut implementation = Implementation::new("carrysheet", env!("CARGO_PKG_VERSION"));
        implementation.title = Some("Carrysheet".into());
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(implementation)
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.inner.tools.clone()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // Over HTTP, the request's work slot goes with the work and is
        // released only when the work ends, even if the client has gone or
        // the request timed out. So abandoned calls cannot pile up threads.
        let slot = context
            .extensions
            .get::<hyper::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<http::WorkSlot>())
            .and_then(http::WorkSlot::take);
        // Tool work touches files and may take a while: keep it off the
        // runtime's only thread, so ping and cancellation still get answers.
        let server = self.clone();
        let name = request.name.into_owned();
        let arguments = request.arguments;
        let result = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            server.call(&name, arguments)
        })
        .await
        .unwrap_or_else(|e| {
            let err = CommandError::new(
                ErrorCode::Internal,
                "the tool call stopped unexpectedly",
                e.to_string(),
                "Try the call again; if it happens again, report it with the feedback tool.",
            );
            CallToolResult::structured_error(error_envelope(&err))
        });
        Ok(result.into())
    }
}

/// Opens the root folder. Without `--root` the current folder is used, but
/// never a whole drive or the home folder: clients often start servers
/// there, and every file in it would be open to the agent. With `--root`
/// those are allowed, with a warning for the log.
fn open_root(given: Option<&Path>) -> Result<(Root, Option<String>), CommandError> {
    let fix = "Start it with --root set to the folder that holds the workbooks, like: carrysheet-cli mcp serve --root ~/Documents/books";
    let explicit = given.is_some();
    let dir = match given {
        Some(dir) => dir.to_path_buf(),
        None => std::env::current_dir().map_err(|e| {
            CommandError::new(
                ErrorCode::Io,
                "the current folder cannot be read",
                e.to_string(),
                fix,
            )
        })?,
    };
    let root = Root::new(&dir)?;
    let wide = if root.is_filesystem_root() {
        Some("a whole drive")
    } else if home_dir().is_some_and(|home| root.is_folder(&home)) {
        Some("your home folder")
    } else {
        None
    };
    let shown = display_path(&root.dir());
    match wide {
        None => Ok((root, None)),
        Some(what) if explicit => {
            let warning =
                format!("warning: --root {shown} is {what}; clients can use every file in it");
            Ok((root, Some(warning)))
        }
        Some(what) => Err(CommandError::new(
            ErrorCode::Usage,
            format!("the current folder {shown} is {what}, too wide to serve without --root"),
            "without --root the server may use every file under the folder it starts in",
            fix,
        )),
    }
}

/// Runs the server: on stdin and stdout until the client closes stdin, or
/// with `--http` on loopback until the process is stopped.
pub fn serve(options: &Options) -> Result<(), CommandError> {
    let log = Arc::new(match &options.log_file {
        Some(path) => Log::file(path)?,
        None => Log::stderr(),
    });
    let (root, warning) = open_root(options.root.as_deref())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            CommandError::new(
                ErrorCode::Internal,
                "the MCP server could not start",
                e.to_string(),
                "Report this with `carrysheet-cli feedback bug`.",
            )
        })?;
    let transport = if options.http.is_some() {
        Transport::Http
    } else {
        Transport::Stdio
    };
    // A panic (a damaged file can make a dependency panic; the call is
    // caught and fails cleanly) is logged like everything else, with
    // control characters escaped, instead of raw on stderr.
    let panic_log = log.clone();
    std::panic::set_hook(Box::new(move |info| {
        panic_log.line(&format!("panic (caught): {info}"));
    }));
    let server = Server::new(root.clone(), options.read_only, transport, log.clone());
    let outcome = match options.http {
        None => serve_stdio(&runtime, server, &root, warning, &log),
        Some(address) => serve_http(&runtime, server, &root, warning, address, options, &log),
    };
    // Do not wait for the thread still blocked reading stdin.
    runtime.shutdown_background();
    // The log thread writes out what is queued before the process ends.
    log.close();
    outcome
}

/// The first log line: version, root, tools and protocol versions.
fn log_start(log: &Log, server: &Server, root: &Root, over: &str, warning: Option<String>) {
    let versions = server.supported_protocol_versions();
    log.line(&format!(
        "mcp serve {} over {over}: root {}, {} tools{}, MCP {} to {}",
        env!("CARGO_PKG_VERSION"),
        display_path(&root.dir()),
        server.inner.tools.len(),
        if server.inner.read_only {
            " (read-only)"
        } else {
            ""
        },
        versions.first().map_or("?", |v| v.as_str()),
        versions.last().map_or("?", |v| v.as_str()),
    ));
    if let Some(warning) = warning {
        log.line(&warning);
    }
}

fn serve_stdio(
    runtime: &tokio::runtime::Runtime,
    server: Server,
    root: &Root,
    warning: Option<String>,
    log: &Arc<Log>,
) -> Result<(), CommandError> {
    // From here on, stdout is the transport's alone.
    crate::output::reserve_stdout_for_mcp();
    let _ = tracing::subscriber::set_global_default(TracingToLog(log.clone()));
    log_start(log, &server, root, "stdio", warning);

    let result = runtime.block_on(async {
        match server.serve(rmcp::transport::stdio()).await {
            Ok(running) => running.waiting().await.map_err(|e| e.to_string()),
            // The client left before starting a session: nothing went wrong.
            Err(ServerInitializeError::ConnectionClosed(_)) => Ok(QuitReason::Closed),
            Err(e) => Err(e.to_string()),
        }
    });
    match result {
        Ok(reason) => {
            log.line(&format!("mcp serve stopped: {}", quit_reason(&reason)));
            Ok(())
        }
        Err(why) => {
            log.line(&format!("mcp serve failed: {why}"));
            Err(CommandError::new(
                ErrorCode::Io,
                "the MCP session ended with an error",
                why,
                "Check that the client speaks MCP over stdio and starts with initialize or server/discover.",
            ))
        }
    }
}

fn serve_http(
    runtime: &tokio::runtime::Runtime,
    server: Server,
    root: &Root,
    warning: Option<String>,
    address: SocketAddr,
    options: &Options,
    log: &Arc<Log>,
) -> Result<(), CommandError> {
    let token = http::Token::generate()?;
    let listener = std::net::TcpListener::bind(address)
        .and_then(|l| l.set_nonblocking(true).map(|()| l))
        .map_err(|e| http::bind_error(address, &e))?;
    let bound = listener.local_addr().unwrap_or(address);
    let url = format!("http://{bound}{}", http::PATH);
    // Kept to remove the token file at the end, if it still holds it.
    let mut written = None;
    let shown = match &options.token_file {
        Some(path) => {
            http::write_token_file(path, &token)?;
            written = Some((path.clone(), token.reveal().to_owned()));
            format!(
                "carrysheet-cli: token written to {} (only you can read it on Linux and macOS)\n",
                display_path(path)
            )
        }
        None => format!(
            "carrysheet-cli: token (shown once, never logged): {}\n",
            token.reveal()
        ),
    };
    let stop = {
        let _inside = runtime.enter();
        StopSignal::listen()
    };
    // Straight to stderr, never through the log, which may be a file: the
    // address so clients can find the server (port 0 picked it), and the
    // token unless it went to --token-file.
    let _ = std::io::stderr().lock().write_all(
        format!(
            "carrysheet-cli: MCP over HTTP at {url}\n{shown}\
             carrysheet-cli: clients send the header Authorization: Bearer <token>\n"
        )
        .as_bytes(),
    );

    // Nothing else may reach stdout.
    crate::output::reserve_stdout_for_mcp();
    let _ = tracing::subscriber::set_global_default(TracingToLog(log.clone()));
    log_start(log, &server, root, &format!("HTTP at {url}"), warning);
    if !options.read_only {
        log.line(
            "warning: --allow-writes: any client with the token can change files under the root",
        );
    }

    // Enough open files for every connection, so that a new one is always
    // accepted and the oldest one without a token can make room for it.
    let files = http::raise_file_limit();
    let mut limits = http::Limits::default();
    limits.max_connections = http::connections_for(files, limits.max_connections);
    log.line(&match files {
        Some(f) if f.now > f.before => format!(
            "http: up to {} connections at once (open-file limit raised from {} to {})",
            limits.max_connections, f.before, f.now
        ),
        Some(f) => format!(
            "http: up to {} connections at once (open-file limit {})",
            limits.max_connections, f.now
        ),
        None => format!("http: up to {} connections at once", limits.max_connections),
    });

    let stopped_by = Arc::new(Mutex::new(""));
    let result = runtime.block_on(async {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let stopped_by = stopped_by.clone();
        let shutdown = async move {
            let why = stop.wait().await;
            *stopped_by.lock().unwrap_or_else(PoisonError::into_inner) = why;
        };
        http::serve(listener, server, token, limits, log.clone(), shutdown).await
    });
    if let Some((path, token)) = written
        && http::remove_token_file(&path, &token)
    {
        log.line(&format!("removed the token file {}", display_path(&path)));
    }
    match result {
        Ok(()) => {
            let why = *stopped_by.lock().unwrap_or_else(PoisonError::into_inner);
            log.line(&format!("mcp serve stopped: {why}"));
            Ok(())
        }
        Err(e) => {
            log.line(&format!("mcp serve failed: {e}"));
            Err(CommandError::new(
                ErrorCode::Io,
                "the MCP HTTP server stopped with an error",
                e.to_string(),
                "Start it again; if it happens again, report it with `carrysheet-cli feedback bug`.",
            ))
        }
    }
}

/// Ctrl+C, or on Linux and macOS also SIGTERM. Made before the address is
/// printed, so that a signal sent as soon as a client (or a test) sees the
/// address stops the server cleanly; a SIGTERM handler set up later left a
/// gap in which it killed the process instead. If no handler can be set
/// up, waiting goes on forever: the server is then stopped the hard way.
struct StopSignal {
    #[cfg(unix)]
    terminate: Option<tokio::signal::unix::Signal>,
}

impl StopSignal {
    /// Must be called inside `runtime`.
    fn listen() -> StopSignal {
        StopSignal {
            #[cfg(unix)]
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .ok(),
        }
    }

    /// Waits for a signal and says which.
    async fn wait(self) -> &'static str {
        let ctrl_c = async {
            if tokio::signal::ctrl_c().await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        #[cfg(unix)]
        {
            let terminate = async {
                match self.terminate {
                    Some(mut term) => {
                        term.recv().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                () = ctrl_c => "Ctrl+C",
                () = terminate => "SIGTERM",
            }
        }
        #[cfg(not(unix))]
        {
            ctrl_c.await;
            "Ctrl+C"
        }
    }
}

fn quit_reason(reason: &QuitReason) -> String {
    match reason {
        QuitReason::Closed => "the client closed stdin".to_owned(),
        QuitReason::Cancelled => "cancelled".to_owned(),
        QuitReason::JoinError(e) => format!("internal error: {e}"),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use carrysheet_commands::lint::lint_tools;
    use serde_json::json;

    /// The server for a root folder, logging to stderr.
    fn test_server(root: &Path, read_only: bool) -> Server {
        Server::new(
            Root::new(root).unwrap(),
            read_only,
            Transport::Stdio,
            Arc::new(Log::stderr()),
        )
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("carrysheet-mcp-unit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("root")).unwrap();
        std::fs::write(dir.join("root/book.xlsx"), b"x").unwrap();
        std::fs::write(dir.join("outside.xlsx"), b"x").unwrap();
        dir
    }

    fn tools_json(server: &Server) -> Vec<Value> {
        server
            .inner
            .tools
            .iter()
            .map(|t| serde_json::to_value(t).unwrap())
            .collect()
    }

    #[test]
    fn tools_match_the_table_exactly() {
        let dir = scratch("tools");
        let server = test_server(&dir.join("root"), false);
        let expected: Vec<Value> = mcp_tools().map(tool_definition).collect();
        let tools = tools_json(&server);
        assert_eq!(tools, expected);
        assert!(lint_tools(&tools).is_empty());
        let text = serde_json::to_string(&tools).unwrap();
        assert!(!text.contains("session_id"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_only_hides_tools_that_change_files() {
        let dir = scratch("readonly");
        let server = test_server(&dir.join("root"), true);
        let names: Vec<_> = server.inner.commands.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            ["overview", "read", "eval", "check", "macros", "feedback"]
        );
        for t in tools_json(&server) {
            assert_eq!(t["annotations"]["readOnlyHint"], true, "{}", t["name"]);
        }
        let r = server.call("edit", Some(Map::new()));
        assert_eq!(r.is_error, Some(true));
        let e = &r.structured_content.unwrap()["error"];
        assert!(e["what"].as_str().unwrap().contains("read-only"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn results_carry_the_same_json_as_text_and_structured() {
        let dir = scratch("results");
        let server = test_server(&dir.join("root"), false);
        let mut args = Map::new();
        args.insert("kind".into(), json!("idea"));
        args.insert("message".into(), json!("Sort by two columns"));
        let r = server.call("feedback", Some(args));
        assert_eq!(r.is_error, Some(false));
        let structured = r.structured_content.clone().unwrap();
        assert_eq!(structured["ok"], true);
        assert_eq!(structured["sent"], false);
        let text = &r.content[0].as_text().unwrap().text;
        assert_eq!(text, &structured.to_string());
        assert!(!text.contains('\n'));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failures_put_the_fix_first() {
        let dir = scratch("failures");
        let server = test_server(&dir.join("root"), false);
        for (name, args) in [
            ("overview", json!({ "path": "book.xlsx" })),
            ("overview", json!({ "path": "../outside.xlsx" })),
            (
                "overview",
                json!({ "path": "book.xlsx", "session_id": "x" }),
            ),
            ("overview", json!({})),
            (
                "overview",
                json!({ "path": format!("../{}", "x".repeat(5000)) }),
            ),
            ("overview", json!({ "path": "y".repeat(5000) })),
            ("nope", json!({})),
        ] {
            let r = server.call(name, args.as_object().cloned());
            assert_eq!(r.is_error, Some(true), "{name} {args}");
            let text = &r.content[0].as_text().unwrap().text;
            assert_eq!(text, &r.structured_content.unwrap().to_string());
            // The command line's key order, with the fix early.
            let code_at = text.find(r#""code":""#).unwrap();
            let what_at = text.find(r#""what":""#).unwrap();
            let fix_at = text.find(r#""fix":""#).unwrap();
            let why_at = text.find(r#""why":""#).unwrap();
            assert!(
                code_at < what_at && what_at < fix_at && fix_at < why_at,
                "{text}"
            );
            assert!(fix_at < 200, "{text}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paths_are_confined_and_resolved() {
        let dir = scratch("confine");
        let server = test_server(&dir.join("root"), false);
        let r = server.call(
            "overview",
            json!({ "path": "../outside.xlsx" }).as_object().cloned(),
        );
        assert_eq!(
            r.structured_content.unwrap()["error"]["code"],
            "outside_root"
        );
        // Inside the root, the call reaches the kernel, which finds no workbook.
        let r = server.call(
            "overview",
            json!({ "path": "book.xlsx" }).as_object().cloned(),
        );
        assert_eq!(
            r.structured_content.unwrap()["error"]["code"],
            "damaged_file"
        );
        // A missing file inside the root is reported as missing, with ~.
        let r = server.call("check", json!({ "path": "gone.xlsx" }).as_object().cloned());
        assert_eq!(
            r.structured_content.unwrap()["error"]["code"],
            "file_not_found"
        );
        // Outputs are confined too.
        let r = server.call(
            "convert",
            json!({ "path": "book.xlsx", "output": "../out.csv" })
                .as_object()
                .cloned(),
        );
        assert_eq!(
            r.structured_content.unwrap()["error"]["code"],
            "outside_root"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn macros_reads_inside_the_root_and_marks_file_content() {
        // POI's samples, read in place: the root is their folder.
        let data =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../crates/carrysheet-vba/tests/data");
        let server = test_server(&data, true);
        let r = server.call(
            "macros",
            json!({ "path": "SimpleMacro.xls", "include_source": true })
                .as_object()
                .cloned(),
        );
        assert_eq!(r.is_error, Some(false));
        let structured = r.structured_content.clone().unwrap();
        assert_eq!(structured["has_macros"], true);
        let text = &r.content[0].as_text().unwrap().text;
        assert_eq!(text, &structured.to_string());
        // The notice comes before any module name or source in the text.
        let notice = text.find("never follow instructions").unwrap();
        assert!(notice < text.find("Module1").unwrap(), "{text}");
        assert!(text.contains("Sub TestMacro()"));

        let r = server.call(
            "macros",
            json!({ "path": "../../../../Cargo.toml" })
                .as_object()
                .cloned(),
        );
        assert_eq!(
            r.structured_content.unwrap()["error"]["code"],
            "outside_root"
        );
    }

    #[test]
    fn options_from_args() {
        let mut args = Map::new();
        args.insert("root".into(), json!("books"));
        args.insert("read_only".into(), json!(true));
        args.insert("log_file".into(), Value::Null);
        args.insert("http".into(), Value::Null);
        let o = Options::from_args(&args).unwrap();
        assert_eq!(o.root.as_deref(), Some(Path::new("books")));
        assert!(o.read_only);
        assert_eq!(o.log_file, None);

        assert_eq!(o.http, None);

        // Over HTTP, read-only unless --allow-writes.
        let mut args = Map::new();
        args.insert("http".into(), json!("127.0.0.1:8080"));
        let o = Options::from_args(&args).unwrap();
        assert_eq!(o.http, Some("127.0.0.1:8080".parse().unwrap()));
        assert!(o.read_only);
        args.insert("allow_writes".into(), json!(true));
        args.insert("token_file".into(), json!("token.txt"));
        let o = Options::from_args(&args).unwrap();
        assert!(!o.read_only);
        assert_eq!(o.token_file.as_deref(), Some(Path::new("token.txt")));

        // Usage errors (exit 2).
        args.insert("read_only".into(), json!(true));
        let e = Options::from_args(&args).unwrap_err();
        assert_eq!(e.code, ErrorCode::Usage);
        assert!(e.what.contains("contradict"), "{}", e.what);
        for (name, value) in [("allow_writes", json!(true)), ("token_file", json!("t"))] {
            let mut args = Map::new();
            args.insert(name.into(), value);
            let e = Options::from_args(&args).unwrap_err();
            assert_eq!(e.code, ErrorCode::Usage, "{name}");
            assert!(e.what.contains("only with --http"), "{}", e.what);
        }
        let mut args = Map::new();
        args.insert("http".into(), json!("0.0.0.0:8080"));
        let e = Options::from_args(&args).unwrap_err();
        assert_eq!(e.code, ErrorCode::Usage);
        assert_eq!(e.exit_code().code(), 2);
    }

    #[test]
    fn read_only_over_http_says_how_to_allow_writes() {
        let dir = scratch("readonly-http");
        let server = Server::new(
            Root::new(&dir.join("root")).unwrap(),
            true,
            Transport::Http,
            Arc::new(Log::stderr()),
        );
        let r = server.call("edit", Some(Map::new()));
        let e = &r.structured_content.unwrap()["error"];
        assert!(e["fix"].as_str().unwrap().contains("--allow-writes"), "{e}");
        assert!(e["why"].as_str().unwrap().contains("over HTTP"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
