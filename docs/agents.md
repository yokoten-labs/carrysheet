# Working with AI agents

**Status: accepted.** Design rules for `carrysheet-cli` and its MCP server, so they work well with many agents: Claude Code, Claude Desktop, Codex, Cursor, VS Code Copilot, Gemini CLI, Windsurf, Cline, Zed, JetBrains, Goose, and local models through LM Studio or Ollama hosts.

Evidence: [research/mcp-client-compat.md](../research/mcp-client-compat.md), the decision records ([0005](records/0005-every-agent-equal.md), [0006](records/0006-follow-habits.md), and records 0007 and 0008 named below), and lessons from earlier agent tools.

## Principles

Carrysheet aims to be **AI-friendly, human-friendly and MCP-friendly**. The rules below are what that means in practice.

1. **One command layer.** The CLI, the MCP server and the desktop app call the same validated commands. CLI help, the JSON schemas and the MCP tool list are generated from one struct per command, so they can't drift.
2. **Docs are not proof.** A client is supported only after a real tool call worked in it. We publish a support table with client version, date and model, and re-check it, because clients change monthly.
3. **Design with each model, not around one.** Before a release, ask each agent what it can do with the interface, run a cheap small test, then the full run.
4. **Assume clients fail quietly.** They skip untrusted config without a word, drop results, strip env variables and truncate output. Our side must be the one that is loud and clear.
5. **Honest results.** If a formula can't be computed, say which cells and why. Never guess, and never pass off a cached value as freshly computed.

## Command line

- `--json` on every command. With it, stdout holds JSON only; errors are JSON too (`{"ok": false, "error": {"code", "what", "fix", "why"}}`, where `code` is a stable snake_case id such as `confirmation_required`) and also go to stderr. As text: error, fix, why.
- Fixed exit codes: 0 done, 1 failed, 2 usage error, 3 nothing to do.
- Never prompt. A call that needs a confirmation fails with a three-part error that names `--confirm`, `--confirm-loss` and `--yes` (see Confirmation below).
- `--dry-run` on every command that changes a file. It prints the change list.
- Unknown flags are refused, with a "did you mean" suggestion. `--json=VALUE` is refused too (exit 2).
- Home folders are shown as `~` in every message, including echoed arguments.
- Output under 80 columns, no colour or escape codes when piped.
- Paths shown as `~\...`, never the full user folder.

## MCP server

### Protocol

- rmcp, serving both spec versions: 2025-11-25 (with `initialize`) and 2026-07-28 (stateless, `server/discover`).
- No session state. A workbook is named by its path in every call. Never call an argument `session_id` (one client bridge strips it).
- `carrysheet-cli mcp serve` starts the server, over stdio only. MCP over HTTP left with record 0010: the binary holds no network code, and `deny.toml` bans HTTP and TLS crates. A client that speaks only HTTP needs a stdio-to-HTTP bridge; none is named here until one is tested.
- stdout carries only JSON-RPC: UTF-8, LF, one message per line. Logs go to stderr only (start, stop, failed calls, Carrysheet's own warnings such as a wide `--root`, and panics, written in line; rmcp's internal diagnostics are not logged, so a client should read or discard stderr), cleaned like text from a file: control, bidi and invisible characters escaped, the home folder shown as `~`. Every crate denies `println!`.
- Calls run off the protocol thread, so `ping` and cancellation keep working during a slow call. Answers can come back out of order; clients match them by id.
- Starts in well under 1 s (Codex gives up after 10 s; Windows process start is slow).

### Tools

Few, workflow-level tools. Names of built tools match `COMMANDS` in `crates/carrysheet/src/args.rs`, which holds one struct per command.

| Tool | Does | Changes files | Toolset |
|---|---|---|---|
| `overview` | Sheets, used ranges, cell, formula and error counts, defined names, and whether the file has macros (never run; kept on save where the format can hold them). For .xls and .ods files, and a zip it cannot list, it says macros were not checked. The first call on any workbook | No | `core` |
| `read` | A range as a compact table with A1 addresses, values or formulas, paged | No | `core` |
| `eval` | Compute a formula against the workbook without saving | No | `core` |
| `edit` | A batch of operations: set values or formulas, insert or delete rows, rename sheets. `dry_run` returns the change list; the batch is all or nothing | Yes | `core` |
| `convert` | Save as xlsx or csv, to a new file | Yes (new file) | `core` |
| `find` | Search values or formulas, returns addresses. Not built | No | not decided |
| `undo` | Restore the snapshot taken before an `edit`. Not built | Yes | not decided |

Rules:

- Names are short `snake_case`. A tool description has at most 300 characters, starts with the key words, and says when to call the tool. An argument description says what it is and its default, and nothing the schema already says; longer guidance goes in the error's `fix` and in `--help`.
- Tools are listed in a fixed order.
- Every tool sets all four annotations explicitly: `readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`.
- Archive members are opened by path, as Windows Explorer shows a zip as a folder: `reports.zip/q3.xlsx`. A real folder of that name wins if both exist. When a zip holds several spreadsheets and the path names none, the error lists their paths (issue 49). No tool has an `entry` argument.

### Toolsets

Decided in record 0008 (`docs/records/0008-mcp-tool-list.md`, in PR 88 until it is merged).

- **The default list stays at 10,000 characters or less**, counted as compact JSON (name, description, input schema, annotations). The default list is what a client gets with no flags. The number is ours, not a client limit: it keeps the tools usable for a local model with a 4k–8k context and for clients that load every tool into every request. A new record can change it.
- Planned: `carrysheet-cli mcp serve --toolsets default,trace --tools diff`. The arguments of `mcp serve` today are `root` and `read_only`; `--toolsets` and `--tools` are not built yet. `default` is used when no toolset is given; `all` turns on everything. An unknown name stops the server at start with a three-part error. Clients can only hide tools on their side, never add one, so the server's default list is the small one.
- `--read-only` wins over both: a write tool stays hidden whatever `--toolsets` or `--tools` say.
- **Fold first.** A new feature becomes an argument or an `edit` op of an existing tool when it is the same verb on the same file. A new tool needs a verb no tool has, and goes in a toolset that is off by default unless it belongs in `default`.
- Toolsets off by default are named in one line of the server instructions. When a call meets something an off tool handles, its `hint` names the CLI command and the toolset. Safety facts are never behind a toolset: `overview` says that a file has macros, that they are never run and whether a save keeps them, or that it could not check.
- The CLI keeps every command whatever the toolsets, so an agent with a shell can always run them.
- The lint fails when the default list passes 10,000 characters, one tool passes 2,500, or `all` passes 20,000. A test on the real server sums `tools/list` from `mcp serve` with no flags.

| Toolset | Tools | On by default |
|---|---|---|
| `core` (`default`) | overview, read, eval, edit, convert | Yes |
| `diff` | diff, when built (issue 65) | No |
| `trace` | trace, when built (record 0007) | No |

### Schemas that every client accepts

- The root is one plain object. No `anyOf`/`oneOf`/`allOf` at the root, no `$ref`/`$defs`, no type lists like `["string","null"]`, no reliance on `format` or `pattern`.
- Every array has `items`; only arrays have `items`.
- Property names match `[a-z0-9_]{1,64}`.
- Modes are enums. Everything is validated again on our side.
- `anyOf` is only for nullable values: single-type branches, then exactly `{"type":"null"}` last. Values keep their JSON type: `3` is a number, `"3"` is text.
- Compatible with OpenAI strict mode: every property listed in `required`, optional ones nullable, `additionalProperties: false`.
- A lint (`crates/carrysheet/tests/lint/mod.rs`, test code only) checks the Claude, Gemini and OpenAI rules on every change; it runs as a test in `cargo test --workspace`.

### Output size

- Reads return a bounded window by default (200 rows, 50 columns, at most about 10,000 cells), as a compact table, plus `next_cursor` and a hint on how to narrow the request. Pass `next_cursor` back as `cursor` and keep the other limits the same; pages go across a band of rows, then down.
- Long text is cut, never dropped quietly: a cell's text or formula at most 2,000 characters in `read`, and less when a page holds many cells, so one answer carries at most 200,000 characters of cell text. The answer then has a `cut` field with the cells cut (up to 20 named, plus the total) and a hint on how to get more: read fewer cells at a time, or get the rest of one cell with `eval`, like `MID(Sheet1!A1,2001,2000)`.
- `response_format` of `read`: `concise` (default) or `detailed`. Concise matters for local models with 4k–8k context.
- The same data goes in the text content and in `structuredContent` (Cursor drops structured-only results). `outputSchema` is declared only where structured output always comes back.
- Bulk reads set `_meta["anthropic/maxResultSizeChars"]`. Whole sheets are exported to a file and returned as a path, not inlined.

### Errors

- Tool failures return `isError: true`, not a protocol error, so the session survives.
- The fix comes in the first ~200 characters, with an example call. Some clients keep only the start and end of an error.
- Codes for files: `file_not_found`, `unsupported_format`, `damaged_file` and `limit_exceeded` (all exit 1). A damaged or over-limit file is treated as hostile: the fix never says to open it in Excel to repair it.
- `fix` and `hint` never contain text from the file. Where an error names something from the file, it is quoted and cut.

### Safety

- **Folder confinement.** `--root` (default: the current folder). Paths are checked as text first, before any file is touched: network paths (`\\host\share`, WebDAV), device paths (`\\.\`, `\\?\GLOBALROOT`), other drives, reserved names (`NUL`, `COM1`) and `..` above the root are refused at once, so a path can never make Windows send credentials to another machine. Then links and junctions are followed one by one, each checked again. A refusal is `outside_root`.
- Without `--root`, a drive root, `/` or the home folder is too wide: the server refuses to start and asks for `--root`. An explicit `--root /` starts with a warning in the log.
- **Read-only mode.** `--read-only` hides every tool that changes files or is not marked read-only, and refuses them by name, whatever `--toolsets` and `--tools` say. It is set in the server's own arguments, not an env variable, because several clients strip env.
- **Never overwrite by default.** `edit` writes to the original only with `overwrite: true`; otherwise to a new version beside it. Saves are atomic (temp file in the same folder, then replace).
- **Snapshots** before every change make `undo` cheap.
- **Confirmation**, in two separate kinds, so agreeing to delete rows never also agrees to drop macros:
  - `confirm: true` (`--confirm`) for destructive edits: deleting sheets, rows or columns, and overwriting a file.
  - `confirm_loss: true` (`--confirm-loss`) for a save that would lose content (macros, charts…). `dry_run` lists that content in `save.lost`.
  - `--yes` gives both on the command line. Nobody is ever asked, at a terminal or not. MCP adds `_meta["anthropic/requiresUserInteraction"]` where supported.
  - Without the right one the call fails with `confirmation_required` (exit 1), and the fix says to call again with the same arguments and it set. `dry_run` never needs either.
- **Dangerous formulas** (WEBSERVICE, external links, DDE, HYPERLINK to files) are refused when written by an agent. The check reads GridCraft's parse tree of the formula, the tree the kernel stores, in any letter case. A formula that does not parse is refused too, with a fix that says to correct it.
- **CSV import keeps what the file says** (record 0006). A field becomes a number only when the number prints back as the same text: `007`, `1E5`, `+1`, long numbers, gene names such as `SEPT1` and dates stay text. A plain decimal such as `12.50` becomes a number with a `0.00` format, so it sums and saves back as `12.50`. More than 15 significant digits (`4111111111111111`) or 30 decimals stay text. A byte order mark decides the encoding; otherwise valid UTF-8 is UTF-8, and other bytes are guessed (GBK, Big5, Shift_JIS, Windows-1252...). Bytes the encoding cannot hold are a `damaged_file` error, never replacement characters. A guessed encoding is named in the `overview` warnings; a guess on a very short file can be wrong, so save such a file as UTF-8.
- **CSV export escapes formula-like text** by default: text starting with `=`, `+`, `-` or `@` gets a `'` before it, so spreadsheet apps don't run it. `csv_raw: true` (`--csv-raw`) writes it as it is.
- **Data connections show an allow-list, never masked raw text** (issue 26). Results show only fields a parser recognized: provider, host and port, database, the kinds of sign-in present (never values), and file paths and URLs split into parts.
  - Hosts are never masked. A path or URL is shown parsed, or hidden whole with the reason when it may carry a credential; it is never partly masked.
  - SQL is shown whole only when it is a plain read (one `SELECT` or `WITH`, nothing that writes, runs or connects); otherwise it is hidden whole, with the reason.
  - Raw texts (connection strings, full URLs, M code) come only with `include_raw: true` (`--raw`), masked as well as patterns can, and the answer warns that this masking is best effort. A text over its limit stays hidden even then, because a cut could end inside a secret.
- **Cell text is data, not instructions.** Results mark workbook content clearly, so a sheet saying "ignore your instructions" stays text. Control, bidi and invisible characters from the file are escaped, and names are cut at 64 characters.
- **Provenance** (proposed in record 0007; not yet accepted). A trail of changes, off by default because it holds people's names and files are shared.
  - On for one call with `--trace`, for a file (saved in the file, so it stays on), or for every file in the user's settings.
  - Each entry: who (the person's name if set; the client and model, labelled as self-reported), UTC time, sheet and cell, value and formula before and after, the note why, the command and its arguments, and the source the value came from.
  - Stored inside the workbook as a custom XML part. csv, and .xls for now, say that the trail can't be saved there.
  - `overview` shows that a file carries a trail, how many entries, and that it holds names; one command removes it before sharing.

### Many processes at once

Some clients start one server per conversation thread (hundreds were observed). Use OS file locks, detect that a file changed since it was read (hash or modified time), and return a clear "locked by …" or "changed since you read it" error.

### Platforms

The CLI and MCP server ship for three tiers of platforms from the first release. Linux matters because cloud agent sandboxes run it; macOS because many agent users work on Macs. A desktop app for Windows and macOS comes later; its first window is a review view (issue 43).

| Tier | Platforms | What we do |
|---|---|---|
| 1 | Windows x64, Linux x64, macOS arm64 | Build, test fully and package every release |
| 2 | Windows x86, Linux arm64, macOS x64 | Build every release, lighter testing |
| 3 | FreeBSD and others | Keep it compiling; community testing and packaging |

### Windows

- A native exe, launched by absolute path with an args array. No shell, no `cmd /c`, no npx.
- Unicode and CJK paths, spaces, `\` and `/` all work.
- MSIX-hosted clients lose user PATH and virtualize AppData: the docs show absolute-path configs for them.

## Installing into clients

Planned, not built. The `mcp install` and `mcp doctor` placeholders were removed with record 0010 (issue 102); they come back only under the line cap. The plan:

- `carrysheet-cli mcp install <client>` and `uninstall <client>`, for every client in the research table.
  - Changes only our own entry and keeps everything else.
  - Checks before writing; never leaves a half-written file.
  - Running it twice changes nothing and says so.
  - A config file it can't parse is skipped with one line and exit 1.
  - Files that allow comments (JSONC) are not rewritten; it prints the snippet to paste instead.
  - Never creates config folders for clients the user doesn't have.
  - Warns when Carrysheet is installed twice in one client (plugin and manual).
- `carrysheet-cli mcp doctor` checks each client: config parses, the entry points at this exe, the folder is trusted where trust can be read. It prints the one step that fixes a problem, and says "can't check" rather than "ok" when it can't see.
- One-click links where clients have them (VS Code, Cursor, LM Studio), a `.mcpb` bundle for Claude Desktop, a winget portable package, and entries in the official MCP Registry (cargo and mcpb).
- A setup page written for agents at carrysheet.com/agents.md, served as `text/markdown`, so a user can say "read this and set Carrysheet up".

## Beyond MCP

- A `SKILL.md` that teaches the CLI (`--json`, exit codes, `--dry-run`, `--yes`), installed where each agent looks for skills.
- A Claude Code plugin and a Codex plugin, each bundling the skill and the MCP config.
- `AGENTS.md` in this repo for agents working on Carrysheet itself; `CLAUDE.md` imports it.

## Testing

CI runs plain cargo commands on every pull request (fmt, clippy, tests, cargo-deny, wasm, Windows builds, release privacy scan); see `.github/workflows/ci.yml`. You can run the same commands locally.

- The schema lint and the tool-list size tests above run as part of it.
- Tests use a temporary home folder and `TZ=UTC`, and never read the developer's real client configs.

Planned for CI (issue 52):

- Official MCP conformance suite against both spec versions (through a stdio-to-HTTP bridge).
- MCP Inspector CLI smoke test over stdio on Windows x64 and x86 runners.
- Headless runs of Claude Code, Codex and Gemini CLI doing scripted tasks on golden workbooks, with checked results.
- The published support table is updated from these runs.

## Open questions

- Screenshots or rendering a range to an image, so agents can check their work visually. Competitors have it; it costs size.
