# MCP over HTTP (`mcp serve --http`)

Text for `docs/agents.md`, which lives on the `agent-compat` branch. When
both branches are merged, move this into agents.md as marked below and
delete this file.

## In "MCP server › Protocol", replace the `--http` bullet with

- `carrysheet-cli mcp serve` starts the server. stdio by default. `--http 127.0.0.1:PORT` (or `[::1]:PORT`) serves streamable HTTP at `/mcp`, for conformance tests and tunnels. Port 0 picks a free port; the address is printed on stderr. Any other address, `0.0.0.0` included, is refused as a usage error (exit 2). Over HTTP the server is stateless too: no `Mcp-Session-Id`, the same two spec versions, plain JSON answers.

## In "MCP server › Safety", add after "Read-only mode"

- **HTTP on this computer only.** A web page can reach a server on 127.0.0.1 through DNS rebinding: it points its own host name at 127.0.0.1 and sends requests from the browser. So in HTTP mode:
  - The server listens on 127.0.0.1 or [::1] only.
  - `Host` must be `127.0.0.1:PORT`, `localhost:PORT` or `[::1]:PORT`, and `Origin` must be absent or one of those with `http://`. Anything else gets 403.
  - Every request needs `Authorization: Bearer <token>`. The token is 256 bits from the operating system's random source, made at start, shown once on stderr and never logged. `--token-file PATH` writes it to a file instead: owner-only (mode 0600) on Linux and macOS; on Windows the file gets its folder's default permissions, so put it in your own profile folder. A missing or wrong token gets 401; tokens are compared in constant time.
  - Read-only by default, as with `--read-only`. `--allow-writes` offers the tools that change files, with a warning in the log.
  - Limits, so a client cannot wear the server down:
    - Each request is checked in turn: a POST (405) to `/mcp` (404) of JSON (415), accepting JSON and event streams (406), with a body of at most 1 MiB (413) that arrives within 10 s (408). Workbooks are named by path, so no real call needs more than a few kilobytes.
    - Bodies share a budget of 2 MiB: a request reserves its declared length before its body is read (503 when the budget is used up; for a body of about 64 KB or more, the client may see the connection reset instead of the 503, because the server closes it without reading the body. Real MCP requests are far smaller.) and keeps it while its call runs. Parsing JSON takes many times a body's size: measured on Linux x64 with a body that is one long array, one 1 MiB body raised the server's memory by about 97 MB, two at once by about 159 MB, and eight sent at once by about 111 MB (two were let in, six refused before their bodies). With the earlier 4 MiB limit and no budget, one took about 0.4 GB and eight about 1.1 GB.
    - 8 work slots (503 when all are taken). A tool call keeps its slot until its work ends, even when the client has gone or the request timed out after 120 s (504), so abandoned calls cannot pile up threads.
    - 256 connections. A connection must pass the token check within 3 s, every refusal closes its connection, and at the limit the oldest connection without a token check is closed to make room. A real client is safe as long as fewer than 256 new connections arrive between its connect and its request, so it has tens of milliseconds, depending on the CPU. Measured with scripts opening connections as fast as they could: at about 3,000 a second, every client got through with up to 50 ms between connect and request; at about 4,600 a second, half the clients that waited 50 ms were closed. Once checked, a connection that sends nothing for 10 s is closed.
    - Enough open files for those connections. On Linux and macOS the server raises its soft open-file limit toward the hard limit at start (macOS starts programs with 256), and if the limit is still low, it allows fewer connections: 64 fewer than the limit, at least 8. So a new connection is always accepted and can make room. The log says how many connections it allows.
    - No CORS headers, so a browser page cannot read an answer.
  - Refusals, including rmcp's own transport errors, are short JSON errors (`code`, `what`, `fix`, `why`) that never repeat anything from the request. JSON-RPC errors keep their code with a fixed message. The JSON-RPC `id` is repeated, as JSON-RPC requires. A tool's error result names the argument or value at fault (an unknown tool or argument, a bad cell reference, a missing file), because the client needs it to fix the call; tool errors are cut short: `what` to 100 characters, `fix` and `why` to 200. The log gets one line per minute summing up refused requests and failed accepts, never one line each, and never what a request held; a tool call is logged with its name only if it is a real tool.
  - On stdio the same parser runs with no size limit. That is fine: a stdio client is a process the user started, which could read the files itself.
  - stdout stays empty; logs go to stderr or `--log-file`.

## Example

```
$ carrysheet-cli mcp serve --root ~/Documents/books --http 127.0.0.1:0
carrysheet-cli: MCP over HTTP at http://127.0.0.1:53817/mcp
carrysheet-cli: token (shown once, never logged): 4f0c…
carrysheet-cli: clients send the header Authorization: Bearer <token>
```

A client then sends JSON-RPC as `POST http://127.0.0.1:53817/mcp` with
`Authorization: Bearer 4f0c…`, `Content-Type: application/json` and
`Accept: application/json, text/event-stream`. Clients on the 2026-07-28
revision also send `MCP-Protocol-Version` and `Mcp-Method` headers, as that
revision requires.

The server runs until Ctrl+C (or SIGTERM on Linux and macOS). It then waits up
to 5 s for running tool calls and logs any still running, writes out its log,
and removes the `--token-file`, if the file still holds its token. Calls
still running stop with the process; that is safe because saves are atomic
(temporary file, then rename; see "Never overwrite by default").
After a hard kill the file stays; its token is no use without the server
that made it.
