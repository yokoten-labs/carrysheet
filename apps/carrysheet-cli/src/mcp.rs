//! `mcp serve`: the MCP server over stdio, built on rmcp.
//!
//! - Tools come from the command table, in its fixed order, with all four
//!   annotations set. `--read-only` leaves out every tool that changes files.
//! - A tool call goes through the same `validate_args` and [`run::run`] as
//!   the command line. Its result carries the same JSON as compact text and
//!   as `structuredContent`; a failure is `isError: true` with `fix` first.
//! - Every path argument must stay inside `--root` (default: the current
//!   folder) and is handed on resolved.
//! - stdout carries only JSON-RPC. The log goes to stderr, cleaned like
//!   text from a file.
//! - Both lifecycles, as rmcp implements them: `initialize` for 2025-11-25
//!   and older, and `server/discover` with metadata on each request for
//!   2026-07-28. rmcp's default version list (2024-11-05 to 2026-07-28) is
//!   kept, so clients still on older revisions can connect.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use carrysheet::commands::schema::tool_definition;
use carrysheet::commands::table::{ArgType, Command, mcp_tools};
use carrysheet::commands::{CommandError, ErrorCode, validate_args};
use carrysheet::text::escape;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{QuitReason, RequestContext, ServerInitializeError};
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Map, Value};

use crate::files::{Root, display_path, home_dir, redact_error, redact_home};
use crate::output::ok_envelope;
use crate::run::{self, Context};

/// Told to the client at the start; some clients show it to the model.
const INSTRUCTIONS: &str = "Carrysheet reads, converts and edits spreadsheet files \
(xlsx, xlsm, xlsb, xls, csv). Call overview first on any workbook. Paths are relative to the \
server's root folder; files outside it are refused. Name the workbook by its path in every call.";

/// What `mcp serve` was started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// `--root`; without it, the current folder.
    pub root: Option<PathBuf>,
    /// Offer only tools that never change files.
    pub read_only: bool,
}

impl Options {
    /// Reads the validated `mcp serve` arguments.
    pub fn from_args(args: &Map<String, Value>) -> Result<Options, CommandError> {
        Ok(Options {
            root: args.get("root").and_then(Value::as_str).map(PathBuf::from),
            read_only: args.get("read_only").and_then(Value::as_bool) == Some(true),
        })
    }
}

/// Longest log line kept, in characters.
pub(crate) const MAX_LOG_LINE: usize = 2_000;

/// Writes one line to stderr, never stdout: the home folder shown as `~`,
/// and control and invisible characters escaped as for text from a file,
/// so a client's text cannot forge a line or hide escape codes.
pub fn log(message: &str) {
    let _ = std::io::stderr()
        .lock()
        .write_all(log_line(message).as_bytes());
}

pub(crate) fn log_line(message: &str) -> String {
    let line = escape(&redact_home(message), MAX_LOG_LINE);
    format!("carrysheet-cli: {line}\n")
}

/// The server: the tools it offers and the folder it may use.
#[derive(Clone)]
pub struct Server {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) struct Inner {
    root: Root,
    read_only: bool,
    /// The offered commands, in table order, and their tool definitions.
    pub(crate) commands: Vec<&'static Command>,
    pub(crate) tools: Vec<Tool>,
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
    pub fn new(root: Root, read_only: bool) -> Server {
        let commands = offered(read_only);
        let tools = commands.iter().map(|c| tool(c)).collect();
        Server {
            inner: Arc::new(Inner {
                root,
                read_only,
                commands,
                tools,
            }),
        }
    }

    /// Runs one tool call and turns the outcome into a tool result. Never a
    /// protocol error, so the client's session survives every failure.
    pub fn call(&self, name: &str, arguments: Option<Map<String, Value>>) -> CallToolResult {
        let started = Instant::now();
        let outcome = self.run_tool(name, arguments);
        // Only failures are logged: stderr is written in line with the call,
        // so a client that never reads it must not stall a stream of good
        // calls. The name comes from the client: log it only if it is a real
        // tool, so a long or multi-line name cannot fill or forge log lines.
        if let Err(e) = &outcome {
            let shown = if mcp_tools().any(|c| c.name == name) {
                name
            } else {
                "(unknown tool)"
            };
            log(&format!(
                "tools/call {shown}: {} in {} ms",
                e.code.as_str(),
                started.elapsed().as_millis()
            ));
        }
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
        // Writes stay inside the root even if a link changes meanwhile.
        let root = Some(self.inner.root.dir());
        let ctx = Context {
            root,
            ..Context::default()
        };
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
            CommandError::new(
                ErrorCode::InvalidArgument,
                format!("the tool `{name}` is not offered in read-only mode"),
                "the server was started with --read-only, which hides tools that change files",
                format!(
                    "Use one of: {}. To change files, restart the server without --read-only.",
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
    let mut envelope = redact_error(e).to_envelope();
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
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // Tool work touches files and may take a while: keep it off the
        // runtime's only thread, so ping and cancellation still get answers.
        let server = self.clone();
        let name = request.name.into_owned();
        let arguments = request.arguments;
        let result = tokio::task::spawn_blocking(move || server.call(&name, arguments))
            .await
            .unwrap_or_else(|e| {
                let err = CommandError::new(
                    ErrorCode::Internal,
                    "the tool call stopped unexpectedly",
                    e.to_string(),
                    STOPPED_FIX,
                );
                CallToolResult::structured_error(error_envelope(&err))
            });
        Ok(result.into())
    }
}

/// The fix for a tool call whose thread stopped.
const STOPPED_FIX: &str = "Try the call again; if it happens again, report it at https://github.com/yokoten-labs/carrysheet/issues.";

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

/// Runs the server on stdin and stdout until the client closes stdin.
pub fn serve(options: &Options) -> Result<(), CommandError> {
    let (root, warning) = open_root(options.root.as_deref())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| {
            CommandError::new(
                ErrorCode::Internal,
                "the MCP server could not start",
                e.to_string(),
                "Report this at https://github.com/yokoten-labs/carrysheet/issues.",
            )
        })?;
    // A panic (a damaged file can make a dependency panic; the call is
    // caught and fails cleanly) is logged like everything else, with
    // control characters escaped, instead of raw on stderr.
    std::panic::set_hook(Box::new(|info| log(&format!("panic (caught): {info}"))));
    let server = Server::new(root.clone(), options.read_only);
    let outcome = serve_stdio(&runtime, server, &root, warning);
    // Do not wait for the thread still blocked reading stdin.
    runtime.shutdown_background();
    outcome
}

/// The first log line: version, root, tools and protocol versions.
fn log_start(server: &Server, root: &Root, warning: Option<String>) {
    let versions = server.supported_protocol_versions();
    log(&format!(
        "mcp serve {} over stdio: root {}, {} tools{}, MCP {} to {}",
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
        log(&warning);
    }
}

fn serve_stdio(
    runtime: &tokio::runtime::Runtime,
    server: Server,
    root: &Root,
    warning: Option<String>,
) -> Result<(), CommandError> {
    // From here on, stdout is the transport's alone.
    crate::output::reserve_stdout_for_mcp();
    let _ = tracing::subscriber::set_global_default(TracingToLog);
    log_start(&server, root, warning);

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
            log(&format!("mcp serve stopped: {}", quit_reason(&reason)));
            Ok(())
        }
        Err(why) => {
            log(&format!("mcp serve failed: {why}"));
            Err(CommandError::new(
                ErrorCode::Io,
                "the MCP session ended with an error",
                why,
                "Check that the client speaks MCP over stdio and starts with initialize or server/discover.",
            ))
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

/// Sends warnings and errors that rmcp reports through `tracing` to the log,
/// so problems in the transport are not silent. Spans are ignored.
struct TracingToLog;

impl tracing::Subscriber for TracingToLog {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.is_event() && *metadata.level() <= tracing::Level::WARN
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::WARN)
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields(String::new());
        event.record(&mut fields);
        let meta = event.metadata();
        log(&format!("{} {}:{}", meta.level(), meta.target(), fields.0));
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// Collects an event's fields as ` message key=value ...`.
struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let _ = match field.name() {
            "message" => write!(self.0, " {value:?}"),
            name => write!(self.0, " {name}={value:?}"),
        };
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        let _ = match field.name() {
            "message" => write!(self.0, " {value}"),
            name => write!(self.0, " {name}={value:?}"),
        };
    }
}
