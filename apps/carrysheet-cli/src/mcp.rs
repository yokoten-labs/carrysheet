//! `mcp serve`: the MCP server over stdio, built on rmcp.
//!
//! Tools come from `carrysheet::args` in table order, with all four annotations; a call runs
//! the same `validate_args` and [`run::run`] as the command line, with every path kept inside
//! `--root`. Both lifecycles rmcp implements: `initialize` (2025-11-25 and older) and
//! `server/discover` (2026-07-28). stdout carries only JSON-RPC; the log goes to stderr.

use std::io::Write as _;
use std::path::Path;
use std::time::Instant;

use carrysheet::args::{Command, McpServe, PATHS, mcp_tools, tool_definition};
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

/// Longest log line kept, in characters.
pub(crate) const MAX_LOG_LINE: usize = 2_000;

/// Writes one line to stderr, never stdout: home shown as `~`, and control and invisible
/// characters escaped as for file text, so a client cannot forge a line or hide escape codes.
pub fn log(message: &str) {
    let _ = std::io::stderr()
        .lock()
        .write_all(log_line(message).as_bytes());
}

pub(crate) fn log_line(message: &str) -> String {
    let line = escape(&redact_home(message), MAX_LOG_LINE);
    format!("carrysheet-cli: {line}\n")
}

/// The server: the folder it may use and the commands it offers as tools, in table order.
/// A clone per call is two small fields.
#[derive(Clone)]
pub struct Server {
    root: Root,
    pub(crate) commands: Vec<&'static Command>,
}

/// The commands offered as tools: every MCP tool in the table; with `--read-only`, only
/// those that never change files and say so in `readOnlyHint`.
pub fn offered(read_only: bool) -> Vec<&'static Command> {
    mcp_tools()
        .filter(|c| !read_only || (!c.changes_files() && c.is_read_only()))
        .collect()
}

impl Server {
    pub fn new(root: Root, read_only: bool) -> Server {
        let commands = offered(read_only);
        Server { root, commands }
    }

    /// The tool definitions as rmcp's type. The JSON comes from the argument structs, so
    /// nothing here can drift from `commands --json`.
    pub(crate) fn tools(&self) -> Vec<Tool> {
        let tools = self
            .commands
            .iter()
            .map(|c| serde_json::from_value::<Tool>(tool_definition(c)));
        tools
            .collect::<Result<_, _>>()
            .expect("tool definitions from the argument structs are valid MCP tools")
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
        let Some(command) = self.commands.iter().find(|c| c.name == name) else {
            return Err(self.unknown_tool(name));
        };
        let mut args = validate_args(command, &Value::Object(arguments.unwrap_or_default()))?;
        self.confine(&mut args)?;
        // Never asks; keeps the agents' time limit (limits::MAX_CALL_SECONDS).
        // Writes stay inside the root even if a link changes meanwhile.
        let ctx = Context {
            root: Some(self.root.dir()),
            ..Context::default()
        };
        run::run(command, &args, &ctx).map(|done| done.data)
    }

    /// Replaces every path argument with the same path resolved inside the
    /// root, or refuses the call.
    fn confine(&self, args: &mut Map<String, Value>) -> Result<(), CommandError> {
        for name in PATHS {
            if let Some(Value::String(given)) = args.get(*name) {
                let resolved = self.resolve(given)?;
                args.insert((*name).to_owned(), Value::String(resolved));
            }
        }
        Ok(())
    }

    fn resolve(&self, given: &str) -> Result<String, CommandError> {
        let path = self.root.resolve(given)?;
        path.into_os_string().into_string().map_err(|_| {
            CommandError::invalid_argument(
                "path",
                "the resolved path is not valid Unicode text",
                "Rename the file or folder so its name is plain text.",
            )
        })
    }

    /// The refusal of a name not offered. Only `--read-only` hides a real MCP tool.
    fn unknown_tool(&self, name: &str) -> CommandError {
        let names: Vec<_> = self.commands.iter().map(|c| c.name).collect();
        let mut fix = format!("Use one of: {}.", names.join(", "));
        let (what, why) = if mcp_tools().any(|c| c.name == name) {
            fix.push_str(" To change files, restart the server without --read-only.");
            let why =
                "the server was started with --read-only, which hides tools that change files";
            (
                format!("the tool `{name}` is not offered in read-only mode"),
                why,
            )
        } else {
            let why = "the name is not in this server's tool list";
            (format!("there is no tool named `{name}`"), why)
        };
        CommandError::new(ErrorCode::InvalidArgument, what, why, fix)
    }
}

/// The longest `what` of a tool error, so `fix` starts in the first 200 characters, which
/// is all some clients keep.
const MAX_WHAT: usize = 100;

/// The longest `fix` and `why` of a tool error: they name the client's own value.
const MAX_DETAIL: usize = 200;

/// The command line's error envelope (home shown as `~`), with `what` cut to [`MAX_WHAT`]
/// characters and `fix` and `why` to [`MAX_DETAIL`].
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
        Ok(ListToolsResult::with_all_items(self.tools()))
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
                    "Try the call again; if it happens again, report it at https://github.com/yokoten-labs/carrysheet/issues.",
                );
                CallToolResult::structured_error(error_envelope(&err))
            });
        Ok(result.into())
    }
}

/// Opens the root: `--root`, or else the current folder, but never a whole drive or the home
/// folder, where clients often start servers. With `--root` those get a warning for the log.
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
/// `args` are the checked arguments of `mcp serve`.
pub fn serve(args: &Map<String, Value>) -> Result<(), CommandError> {
    let options: McpServe = serde_json::from_value(Value::Object(args.clone())).map_err(|e| {
        let fix = "Run `carrysheet-cli mcp serve --help` for the arguments.";
        CommandError::invalid_argument("arguments", e.to_string(), fix)
    })?;
    let read_only = options.read_only == Some(true);
    let (root, warning) = open_root(options.root.as_deref().map(Path::new))?;
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
    let server = Server::new(root.clone(), read_only);
    let versions = server.supported_protocol_versions();
    log(&format!(
        "mcp serve {} over stdio: root {}, {} tools{}, MCP {} to {}",
        env!("CARGO_PKG_VERSION"),
        display_path(&root.dir()),
        server.commands.len(),
        if read_only { " (read-only)" } else { "" },
        versions.first().map_or("?", |v| v.as_str()),
        versions.last().map_or("?", |v| v.as_str()),
    ));
    if let Some(warning) = warning {
        log(&warning);
    }
    let result = runtime.block_on(async {
        match server.serve(rmcp::transport::stdio()).await {
            Ok(running) => running.waiting().await.map_err(|e| e.to_string()),
            // The client left before starting a session: nothing went wrong.
            Err(ServerInitializeError::ConnectionClosed(_)) => Ok(QuitReason::Closed),
            Err(e) => Err(e.to_string()),
        }
    });
    // Do not wait for the thread still blocked reading stdin.
    runtime.shutdown_background();
    match result {
        Ok(QuitReason::Closed) => log("mcp serve stopped: the client closed stdin"),
        Ok(reason) => log(&format!("mcp serve stopped: {reason:?}")),
        Err(why) => {
            log(&format!("mcp serve failed: {why}"));
            return Err(CommandError::new(
                ErrorCode::Io,
                "the MCP session ended with an error",
                why,
                "Check that the client speaks MCP over stdio and starts with initialize or server/discover.",
            ));
        }
    }
    Ok(())
}
