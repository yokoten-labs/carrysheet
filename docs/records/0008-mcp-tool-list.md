# 0008 MCP tool list: a small default list, more tools in toolsets you turn on

Status: accepted
Date: 2026-10-20 (UTC)

## Context

Issue 62: the MCP `tools/list` answer is close to its 10,000-character budget, and more tools are planned. This record decides how the list grows. The facts below led to the decision at the end.

### Where the 10,000 comes from

It is **our own number, not a client limit.** It first appears in the design rules in `docs/agents.md`: "All tool definitions together stay under ~10k characters." The command layer made it a hard lint rule (`MAX_TOOLS_CHARS` in `crates/carrysheet-commands/src/schema/lint.rs`). No note from that time gives a reason. The reason that holds today is record 0005: the same tools must work for a local model with a 4k–8k context, and some clients load every tool into every request.

### What we measured

Each tool is counted as compact JSON (name, description, input schema, annotations), the same way the lint counts. Debug builds of `carrysheet-cli mcp serve`, asked for `tools/list` over stdio, 2026-10-20. We measured three states of the code: the first eight tools; the same eight with the GridCraft kernel added; and nine tools, with `macros`.

| State | Tools | Total chars | Descriptions | Input schemas | of which argument text | Annotations |
|---|---|---|---|---|---|---|
| Eight tools | 8 | 8,617 | 1,440 | 5,950 | 2,245 | 715 |
| Eight tools, with the GridCraft kernel | 8 | 8,779 | 1,486 | 6,066 | 2,361 | 715 |
| Nine tools, with `macros` | 9 | 9,745 | 1,683 | 6,682 | 2,594 | 804 |

Per tool, with nine tools:

| Tool | Chars | Arguments |
|---|---|---|
| edit | 2,076 (2,212 with the GridCraft kernel) | 6, with a nested op object |
| read | 1,490 | 7 |
| convert | 1,330 (1,370 with the GridCraft kernel) | 7 |
| macros | 1,128 | 4 |
| sanitize | 969 | 4 |
| eval | 724 | 3 |
| check | 697 | 2 |
| overview | 681 | 2 |
| feedback | 650 | 2 |

Where the characters go:
- **Argument descriptions: 2,594** (27%). The largest single part.
- **Strict-schema shape: 1,535** (16%). OpenAI strict mode needs every argument in `required`, so optional ones are `anyOf [type, null]` (914), listed in `required` (302), and every object says `additionalProperties: false` (319). This is the price of one schema that Claude, Gemini and OpenAI strict mode all accept (record 0005). We keep it.
- **Tool descriptions: 1,683** (17%).
- **Annotations: 804** (8%), all four set on every tool, as ChatGPT requires.
- The rest is names, types and enums.

In tokens: the 9,745-character list is **2,312 tokens** with the Qwen 3.5 tokenizer, run locally (about 4.2 characters per token). For a local model with an 8k context, that is 29% of the context before the user says anything.

### What is planned

Estimates, if every planned feature is added to one flat list:

| Feature | Change to the tool list | Estimate |
|---|---|---|
| macros (issue 22) | new tool | +1,128 (measured) |
| query (issue 25) | new tool: path, sql, limits, cursor | +1,200 |
| connections (issue 26) | listed by overview and check | +250 |
| comments (issue 64) | `include_comments` on read, three edit ops | +500 |
| diff (issue 65) | new tool: old, new, paging | +1,000 |
| archives (issue 49) | an `entry` argument on about 10 tools that take a path | +1,700 (167 each) |
| trace (record 0007, draft) | new tool, plus a `trace` flag on edit | +1,200 |
| **Total** | about 12 tools | **about 15,800 chars, about 3,700 tokens** |

Issue 69 may also add output schemas to mark file text as data. We declare none today; if we do, the list grows again.

### What clients do (checked 2026-10-20)

Tool count limits do not matter to us: we will have about 12 tools, far under every limit. Size matters only in clients that load all tools up front.

| Client | Loads all tools up front? | Limits and controls |
|---|---|---|
| Claude Code | No. Tool search is on by default: only tool names and server instructions load at start. It loads everything up front with `ENABLE_TOOL_SEARCH=false`, through most proxies (`ANTHROPIC_BASE_URL`), and on older models. | No tool count cap. Descriptions and server instructions cut at 4,096 chars when loaded up front, 16,384 when loaded through search. Refetches the list on `list_changed`. [docs](https://code.claude.com/docs/en/mcp) |
| Claude API | Optional `defer_loading` with the tool search tool. | Anthropic advises tool search at 10 or more tools or over 10k tokens; selection gets worse past 30–50 tools. [docs](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool) |
| Claude Desktop | Not confirmed. Third-party guides mention tool search; no official page found. | |
| OpenAI Codex | Has tool search. The switches that turned it off (`tool_search = false`, `tool_search_always_defer_mcp_tools = false`) are retired, so MCP tools appear to be deferred always (our reading, not stated directly). | `enabled_tools` / `disabled_tools` per server; 10 s start, 60 s per call. [config reference](https://developers.openai.com/codex/config-reference) |
| ChatGPT (developer mode) | Not documented. | Tools are turned on and off per app in its settings; no documented count limit. [developer mode](https://developers.openai.com/api/docs/guides/developer-mode) |
| Cursor | Since early 2026, "dynamic context discovery": tool names up front, details on demand. Older versions used only the first ~40 tools. | Per-tool toggles. [blog](https://cursor.com/blog/dynamic-context-discovery), [forum](https://forum.cursor.com/t/tools-limited-to-40-total/67976) |
| VS Code Copilot | Yes, unless "virtual tools" group them. | At most 128 tools per request; `github.copilot.chat.virtualTools.threshold`; tools picker. [docs](https://code.visualstudio.com/docs/agents/run/tools) |
| Gemini CLI | Yes: all tools found at start go into the registry; `list_changed` not mentioned. Gemini CLI was replaced by Antigravity CLI on 2026-06-18. | `includeTools` / `excludeTools`; names over 63 chars shortened. [docs](https://geminicli.com/docs/tools/mcp-server/) |
| Antigravity | Not confirmed. | 100 tools per server reported [3p]. [forum](https://discuss.ai.google.dev/t/critical-dx-issue-antigravity-ide-hardcoded-mcp-tool-limit-100-configuration-fragmentation-with-gemini-cli/143969) |
| Local models (Ollama, LM Studio, through any client) | Yes. | The context window: often 4k–32k. |

So the big hosted agents mostly defer tools now, and size costs them little. The clients that pay the full price are the ones record 0005 asks us to protect: local models, Gemini and Antigravity, VS Code, and Claude behind a proxy.

Every client can only remove tools on its side (Codex, Gemini, VS Code, Cursor, ChatGPT). None can add a tool the server did not list. So the server's default list must be the small one.

### What the MCP spec offers

- **Pagination** of `tools/list` (`cursor`, `nextCursor`), in 2025-11-25 and 2026-07-28. Clients fetch all pages before use, so it does not make the model's list smaller. [2025-11-25 tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools)
- **`listChanged`**: a server may change its list and send `notifications/tools/list_changed`. In 2026-07-28 the client must open `subscriptions/listen` to get it, and "list endpoints (`tools/list`, ...) no longer vary per-connection". [2026-07-28 changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)
- **Tool order**: 2026-07-28 says servers SHOULD list tools in a fixed order, so clients can cache and prompt caches hit. `tools/list` results also carry `ttlMs` and `cacheScope`.
- **Annotations** are hints only.
- **Groups**: none in the spec. The Progressive Disclosure Working Group is studying grouping, tool search and code mode as extensions, and has not picked one. [charter](https://modelcontextprotocol.io/community/working-groups/progressive-disclosure)

2026-07-28 is the released version: the draft changelog on the spec repository says changes "accumulate here" after it and is empty.

### What other big servers do

- **GitHub's MCP server** ([README](https://github.com/github/github-mcp-server)): `--toolsets repos,issues,...` (or `GITHUB_TOOLSETS`); a `default` toolset (context, repos, issues, pull_requests, users) when none is given; `all` for everything; `--tools` adds single tools; `--read-only` wins over both; an unknown name stops the server at start. Its README: "Enabling only the toolsets that you need can help the LLM with tool choice and reduce the context size." A beta `--dynamic-toolsets` mode gives the model `list_available_toolsets`, `get_toolset_tools` and `enable_toolset`, which add tools while it runs.
- **GitHub Copilot** cut its built-in tools to a 13-tool core and groups the rest as virtual tools. [blog](https://github.blog/ai-and-ml/github-copilot/how-were-making-github-copilot-smarter-with-fewer-tools/)
- **haris-musa/excel-mcp-server** halved its `tools/list` by trimming schemas and dropping pretty-printed JSON (from `research/mcp-client-compat.md`).

## Options

Sizes assume the planned features above.

| | Option | Default list | Works in every client? | Agents | Record 0005 | Record 0006 |
|---|---|---|---|---|---|---|
| a | **Toolsets**: a small default list; `--toolsets` and `--tools` add more | ≤ 10,000 | Yes: only server arguments in the client's config | Default tools are always visible. Others need the user to change the config; the agent can't. | Same defaults for all; turning a set on is the same for all | Follows GitHub's flags, the best-known pattern |
| b | **Core plus a `help`/`describe` tool** that returns the rest | about 6,000 | Partly. A model can only call tools that are in the list, so the rest needs a generic `run(command, args)` tool | Two calls for every hidden tool. `run` loses per-tool schemas and annotations: it must be marked as destructive, so clients ask for approval even for reads | Hurts weak and local models most: they must guess arguments without a schema | Defines our own pattern; none is standard |
| c | **Shorter descriptions**, detail moved into errors and help | about 13,500 if trimmed by 15% | Yes | Less guidance before the first call; errors teach on the second | Neutral | Neutral |
| d | **Dynamic**: the list changes with `listChanged` when a file type is opened | varies | No. Gemini CLI does not refresh; 2026-07-28 says list endpoints no longer vary per connection, and needs `subscriptions/listen` | Tools appear and disappear mid-task; every change breaks the prompt cache | Clients without `listChanged` get a different product | Against the spec's direction |
| e1 | **Fold before adding**: a feature that belongs to an existing verb becomes an argument or an edit op, not a tool (issue 64 already does this) | saves a tool each time, about 700–1,000 | Yes | Fewer, larger tools, as Anthropic's tool guide advises | Neutral | Follows the agents' habit of workflow-level tools |
| e2 | **Archive members by path**: `reports.zip/q3.xlsx`, as Windows Explorer shows a zip as a folder, instead of an `entry` argument on every tool | saves about 1,700 | Yes | One idea to learn, no new argument | Neutral | Follows a habit people already have |
| e3 | **Raise the budget** to fit everything | about 16,000 | Yes, but costs 3,700 tokens per request in clients that don't defer | Local models lose about half of an 8k context | Works against the small-model promise | Neutral |

## Decision

The owner accepted this on 2026-10-20, with `macros` in its own toolset (not in `default`) and archive members opened by path.

A combination: **(a) toolsets, with (e1), (e2) and (c) to keep the default list small.**

1. **The default list stays at 10,000 characters or less.** The default list is what a client gets with no flags. This keeps the promise to local models and to clients that load every tool. The number is ours (see Context) and can change by a new record.
2. **Fold first.** A new feature becomes an argument or an edit op of an existing tool when it is the same verb on the same file. A new tool needs a verb no tool has. Already decided this way: comments (issue 64). Decided: connections in `overview` and `check` (issue 26 already says so).
3. **Toolsets, named like GitHub's.** `carrysheet-cli mcp serve --toolsets default,macros --tools diff`. `default` is used when no toolset is given; `all` turns on everything; `--read-only` wins over both; an unknown name stops the server at start with a three-part error. The sets:

   | Toolset | Tools | Default? |
   |---|---|---|
   | `core` | overview, read, eval, check, edit, convert, query, feedback | yes (this is `default`) |
   | `macros` | macros | no |
   | `share` | sanitize | no |
   | `diff` | diff | no |
   | `trace` | trace | no |

   Estimated `default`: about 9,300 characters after the trim in point 5, with archives by path (point 4). The CLI keeps every command whatever the toolsets, so an agent with a shell can always run them.
4. **Archives by path (issue 49).** `reports.zip/q3.xlsx` opens the member; a real folder of that name wins if both exist; with several spreadsheets the error lists the paths. This replaces the planned `entry` argument and saves about 1,700 characters.
5. **Trim once, by rules** (c): a tool description has at most 300 characters and says what it does and when to call it; an argument description says what the argument is and its default, and nothing the schema already says; longer guidance goes in the error's `fix` and in `--help`. Aim: 15% off the description text.
6. **Say what is off.** The server instructions name the toolsets that are off in one line ("More tools: start with --toolsets macros,share,diff,trace"). Claude Code reads server instructions when it searches for tools. When a call meets something an off tool handles (for example `check` finds macros), its `hint` names the CLI command and the toolset.
7. **No dynamic lists** (d) and **no `run` dispatcher** (b) for now. Revisit when the MCP Progressive Disclosure Working Group publishes an extension that most clients support.

### The test

The lint already fails when all MCP tools together pass `MAX_TOOLS_CHARS` (`lint.rs`, the test `the_mcp_tool_list_leaves_room`). Change it to measure the default list, and add a guard on the real server:

- `crates/carrysheet-commands`: `default_tool_list_fits_budget` sums `tool_definition(c)` over the tools in the `default` toolset and fails over `MAX_TOOLS_CHARS` (10,000), with a message that says what happened, why (this record), and the fix (move a tool to a toolset, fold it into another tool, or shorten descriptions). It prints the size and the room left.
- `crates/carrysheet-commands`: `every_tool_fits` fails when one tool passes 2,500 characters (edit is 2,212 today), and `all_toolsets_fit` fails when `all` passes 20,000, so the opt-in sets stay sane too.
- `apps/carrysheet-cli/tests/mcp.rs`: `mcp serve` with no flags, over stdio; the sum of the tools in `tools/list` must be 10,000 or less. This catches anything the server adds outside the table.

### Migration

1. Command table: each MCP command gets a `toolset` field. `mcp_tools()` takes the chosen toolsets.
2. `mcp serve`: `--toolsets` and `--tools`, with `default` and `all`. `--read-only` keeps hiding write tools.
3. Tests above; the lint's size check moves to the default list.
4. The trim pass on descriptions (point 5), with before and after numbers in the PR.
5. Server instructions and hints (point 6).
6. `docs/agents.md`: replace "under ~10k characters" with the rule from this record and the reason.
7. Issue 49: archive paths instead of `entry`. Issues 25, 26, 64, 65 and record 0007: each names its toolset, or says why it folds into an existing tool.
8. The support table (record 0005, rule 3) records, for each client, whether it defers tools; this does not change the defaults.

## Options not taken

- **(b) a `help` tool with a generic `run` tool.** It loses schemas and annotations, so reads need approval and weak models guess arguments.
- **(d) a dynamic list.** Not every client refreshes, the 2026-07-28 spec moves away from lists that change per connection, and each change breaks prompt caching.
- **(e3) a bigger budget.** It is the easy answer, but local models pay for it on every request.

## Consequences

- Easier: the default list has room to grow, and the rule for a new tool is clear: fold it, or put it in a toolset.
- Harder: users who want macros, diff or trace in MCP add one flag to their client config. The CLI and the hints tell them how.
- To check:
  - `check` reports macro risk on its own, since the `macros` tool is off by default: safety facts are never behind a toolset;
  - the trim keeps tool choice good: one real call per client before release (record 0005, rule 3);
  - Claude Desktop, ChatGPT and Antigravity: whether they load all tools up front (not confirmed here).
