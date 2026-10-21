# 0001 Vision: the agent does the work, people review and decide

Status: accepted
Date: 2026-10-20 (UTC)

## Context

The owner stated the vision on 2026-10-20: compress the distance between saying what you want and having it done. Carrysheet is a spreadsheet built for agents first. A graphical interface that is easy for people comes after.

Today's spreadsheets are built for people, and agents are added later. An agent that edits an .xlsx file usually goes through a library such as openpyxl. That library does not calculate formulas, drops macros and data connections on save, and has no guard against hostile files.

The release order already follows this vision: `carrysheet-cli` with an MCP server ships first, and the desktop app comes later (issue 30).

## Decision

1. **Mission:** compress the distance between communication and execution.
2. **Split of work:** the agent does the work; people review it and decide.
3. **What makes us different**, and what we say instead of only "agent-first":
   - formulas are calculated correctly without Excel;
   - opening and saving a file loses nothing;
   - a hostile file cannot harm the user;
   - answers stay small enough for an agent's context.
4. **The first graphical window is a review view, not a full editor** (issue 43). It shows which cells an agent changed and why. A person keeps or undoes each change.
5. **Choices made now keep the later interface possible.** The kernel choice (issue 37) counts what a person feels in an interface, for example the time to recalculate after editing one cell. It does not count only agent workloads.

## Options not taken

- **People first, agents later.** Excel and Google Sheets already own that path, and a solo project cannot compete there.
- **Agent-only, no interface.** People pay, and people want to see and approve what an agent did. Without that view the product stops at developers.
- **A full spreadsheet editor as the first interface.** It is much larger than a review view, and it is further from what the command layer already has (change log, `dry_run`, snapshots, `undo`).

## Consequences

- Easier: one clear test for features. Does it shorten the path from request to result, or make an agent's work easier to check?
- Easier: the first desktop release is small and builds on the change log and `undo`.
- Harder: the change log must hold enough to explain each change: tool, arguments, the agent's note and before/after values. That is now a requirement, not a nice-to-have.
- To check: the kernel choice (issue 37) weighs interface response time, and Pro features are ones a person running agents pays for.
