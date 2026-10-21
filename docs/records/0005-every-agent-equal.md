# 0005 Every agent is treated the same

Status: accepted
Date: 2026-10-20 (UTC)

## Context

Record 0001 made Carrysheet agent-first. The owner took it one step further on 2026-10-20: a spreadsheet where no AI and no AI company is treated worse than another.

Much of the design already points this way: MCP and a command line that any client can call, tool schemas that pass the strictest rules of Claude, Gemini and OpenAI, and answers small enough for local models. A principle that is only a slogan does not hold, so this record turns it into rules that can be checked.

## Decision

1. **Same tools, same limits, same price for every agent.** Nothing is allowed, throttled or priced by client name, user agent or model. The change log records the client name to help find problems, never to treat clients differently.
2. **Vendor-specific features only add, never gate.** A hint that one vendor reads (for example `anthropic/maxResultSizeChars`) may be set, but every client without it must still work fully.
3. **Tested across vendors.** Before each release, a real tool call must work in at least one client each from Anthropic, OpenAI and Google, and with one local open model (for example through Ollama). A client that fails is our bug to fix, or a limit we document in the support table.
4. **People choose the model.** AI features in the desktop app accept any OpenAI-compatible endpoint, Anthropic, Gemini, and local Ollama or LM Studio. The first run does not pick a vendor for the user.
5. **No exclusive deals.** No AI company gets better access, earlier features or a better place in the product than another.
6. **Neutral guides.** Instructions for agents (`AGENTS.md`, `SKILL.md`, `agents.md` on the website) use shared formats and are written for every agent, not for one.

## Options not taken

- **Tune for one vendor first, add others later.** It is cheaper today, but every later client starts behind, and users of other vendors notice.
- **Same results for every model.** Not possible: a small local model makes more mistakes than a large one. We promise the same tools and fallbacks (paging, small answers, clear errors), not the same outcome.

## Consequences

- Easier: one rule for every feature and partnership: does it treat any agent worse?
- Harder: testing across vendors costs time and money on every release. Today almost all real testing uses Claude, so rule 3 is the gap to close first.
- Not covered by this rule: safety. Confirmations, hostile-file limits and treating file text as data apply equally to every agent. We cannot control platforms that limit third-party tools on their side.
