# MCP across agent clients: what breaks (2026-10-20)

Research by an agent from vendor docs, GitHub issues and third-party tests. **[3p]** marks third-party claims not confirmed at a primary source. Clients change monthly: re-check before relying on any line here.

## Spec and SDK

- Current spec is **2026-07-28** ([changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)). No `initialize` handshake or sessions; servers must implement `server/discover`; cross-call state goes in explicit handles passed as tool arguments. Server-to-client requests become "input_required" round trips. Roots, Sampling and Logging are deprecated. Schemas may use any JSON Schema 2020-12 keyword. Servers should list tools in a fixed order.
- **rmcp 3.5** (official Rust SDK) implements 2026-07-28 with fallback to 2025-11-25. The Rust SDK is "beta" for the new spec, not Tier 1.
- Most clients still speak 2025-11-25 or older, so a server must handle both lifecycles.
- Tool annotations (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`) are hints for UX, not enforcement.

## Support

Tools are the only primitive every client supports (24 of 24 in a [3p survey](https://www.developersdigest.tech/blog/mcp-client-features-explained-2026)); resources 15, prompts 13, elicitation 13, roots 4, sampling 2. ChatGPT accepts remote HTTPS servers only.

## Known breakage

| Area | What happens | Source |
|---|---|---|
| Tool count | Older Cursor versions used only the first ~40 tools [3p]; since early 2026 Cursor loads tool names up front and details on demand. Windsurf 100 across all servers; VS Code 128 per request incl. built-ins. Size matters more than count: see record 0008 for which clients load every tool up front | [Cursor forum](https://forum.cursor.com/t/tools-limited-to-40-total/67976), [Cursor blog](https://cursor.com/blog/dynamic-context-discovery), [VS Code](https://code.visualstudio.com/docs/agents/run/tools) |
| Names | Claude Code drops tools whose full name exceeds 128 chars or whose top-level property names fall outside `[A-Za-z0-9_.-]{1,64}`; Gemini shortens names over 63 chars; OpenAI function names max 64 | [Claude Code](https://code.claude.com/docs/en/mcp), [Gemini CLI](https://geminicli.com/docs/tools/mcp-server/) |
| Argument names | An argument named `session_id` was stripped by a Claude Desktop bridge, leaving Excel processes orphaned | [sbroenne #979](https://github.com/sbroenne/mcp-server-excel/issues/979) |
| Schemas | Claude rejects root `anyOf/oneOf/allOf`; Gemini rejects `$ref/$defs`, type arrays, arrays without `items`, `items` on non-arrays; Gemini CLI strips `$schema` and `additionalProperties`; OpenAI strict needs every property in `required` | [gemini-cli #13326](https://github.com/google-gemini/gemini-cli/issues/13326), [negokaz #77](https://github.com/negokaz/excel-mcp-server/issues/77), [haris-musa PR #207](https://github.com/haris-musa/excel-mcp-server/pull/207) |
| Output size | Claude Code warns at 10k tokens, caps at 25k; text over 50,000 chars goes to a file unless `_meta["anthropic/maxResultSizeChars"]` raises it; descriptions and server instructions cut at 4,096 chars when loaded up front, 16,384 through tool search (on by default); errors may keep only head and tail [3p] | [Claude Code](https://code.claude.com/docs/en/mcp) |
| structuredContent | Cursor silently drops results that exist only in `structuredContent`; errors if `outputSchema` is declared but missing | [Cursor forum](https://forum.cursor.com/t/mcp-tool-results-containing-only-structuredcontent-are-silently-dropped/167346) |
| Timeouts | Codex: start 10 s, call 60 s; Cline 60 s; Gemini 10 min; Claude Code backgrounds after 2 min | [Codex config](https://developers.openai.com/codex/config-reference) |
| stdout | Any stray stdout kills stdio; CRLF translation on Windows corrupted messages; non-UTF-8 code pages mangled text; a CJK path failed in an Excel server | [python-sdk #2908](https://github.com/modelcontextprotocol/python-sdk/pull/2908), [negokaz #5](https://github.com/negokaz/excel-mcp-server/issues/5) |
| Windows launch | `npx`/`.cmd` shims fail without `cmd /c` (a native .exe avoids this); MSIX Claude Desktop drops user PATH and virtualizes AppData | [claude-code #58510](https://github.com/anthropics/claude-code/issues/58510) |
| Concurrency | Windows Codex app starts one stdio server set per thread: 353 processes observed | [codex #53285](https://github.com/openai/codex/issues/53285) |
| Env | Codex passes only listed env vars [3p]; Gemini and others strip custom env | |

## Install formats

| Client | How |
|---|---|
| Claude Code | `claude mcp add carrysheet -- carrysheet-cli mcp serve`; `.mcp.json` for a project; plugins |
| Claude Desktop | `%APPDATA%\Claude\claude_desktop_config.json` or a `.mcpb` bundle |
| Codex | `codex mcp add carrysheet -- carrysheet-cli mcp serve` → `~/.codex/config.toml`; project config only in trusted folders; misspelled sections silently ignored |
| VS Code | `.vscode/mcp.json`, key `servers`; `vscode:mcp/install?{json}` link; `code --add-mcp` |
| Cursor | `mcp.json` (`mcpServers`); `cursor://anysphere.cursor-deeplink/mcp/install?name=…&config=<base64>` |
| Gemini CLI | `gemini mcp add carrysheet carrysheet-cli mcp serve`; `settings.json` allows comments. Replaced by Antigravity CLI on 2026-06-18 (record 0008) |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Zed | `context_servers` |
| JetBrains AI | Settings → MCP, or import from Claude |
| Cline | `cline_mcp_settings.json` with `timeout`, `autoApprove` |
| Goose | `config.yaml`, `extensions` |
| LM Studio | Cursor-style `mcp.json`; `lmstudio://add_mcp` link |
| ChatGPT | Remote HTTPS only; local needs a tunnel |

Official MCP Registry (preview) takes `server.json` with `registryType: "cargo"` (README needs a visible `mcp-name:` line) or `"mcpb"` (release URL + SHA-256). MCPB supports `server.type: "binary"` and a `directory` user setting. winget portable packages land on PATH. Windows' on-device agent registry (`odr.exe mcp add`) takes MCPB or MSIX.

## Tool design guidance

- Anthropic, [Writing effective tools for agents](https://www.anthropic.com/engineering/writing-tools-for-agents): few workflow-level tools, no overlap, readable identifiers, `response_format` concise/detailed, paginate and say how to narrow, errors that say what to fix, evaluate on real multi-step tasks.
- OpenAI: clear names and enums so invalid states can't be expressed; ChatGPT apps must set `readOnlyHint`, `destructiveHint` and `openWorldHint` explicitly on every tool.
- Claude Code: `_meta["anthropic/requiresUserInteraction"]` forces approval every time; server `instructions` matter under tool search.
- haris-musa/excel-mcp-server lessons: streaming reads cut memory from 857 MB to 22 MB on 200k rows; trimming schemas and dropping pretty-printed JSON cut `tools/list` by about half; stdio mode had no path confinement (#149); a case-sensitive formula deny list was bypassed; a file path allowed command injection (#128).
- CLI vs MCP cost: one widely cited [3p benchmark](https://www.firecrawl.dev/blog/mcp-vs-cli) found CLI + a skill far cheaper in tokens than MCP. Ship both, plus a skill that teaches the CLI.
- AGENTS.md (Linux Foundation) and Agent Skills (`SKILL.md`) are read by most major agents; Claude Code and Codex both have plugin formats that bundle a skill and an MCP config.

## Testing

- [modelcontextprotocol/conformance](https://github.com/modelcontextprotocol/conformance): server tests need an HTTP URL; run against both spec versions with an expected-failures baseline.
- MCP Inspector CLI: `npx @modelcontextprotocol/inspector --cli carrysheet-cli mcp serve --method tools/list`.
- Lint every schema for the Claude, Gemini and OpenAI-strict rules on every change (locally through `scripts/check.mjs`; in CI once the repository is public).
- Run real clients headless (`claude -p`, `codex exec`, `gemini -p`) against golden workbooks; record 0005 asks for one client each from Anthropic, OpenAI and Google, plus one local open model, before each release.
