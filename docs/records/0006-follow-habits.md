# 0006 Follow people's habits; define our own only for safety or where none exist

Status: accepted
Date: 2026-10-20 (UTC)

## Context

Carrysheet has three principles: AI-friendly, human-friendly, MCP-friendly. The same question keeps coming back: should a feature work the way people already know, or the way we think is better? The owner asked on 2026-10-20 whether to adapt to users' habits or make our own definitions, and asked for the answer to be a fourth principle so it is not argued again.

Carrysheet's value is the bridge: between old and new computers, and between Excel and agents. Each habit we break is a cost the user pays, while compatibility is a cost we pay. Some habits do harm, though:
- Excel turns gene names such as SEPT1 into dates and cuts long numbers.
- It shows saved passwords in connection strings.
- It runs macros on open.

Some needs have no habit at all: how an agent edits a workbook, or a full change history that works offline.

## Decision

**Follow people's habits by default.** Break one only when:

1. **the habit causes harm:** data is lost or changed silently, secrets are exposed, or untrusted code runs; or
2. **there is no habit to follow.**

When we break a habit or define something new:
- **Say why**, at the moment it matters. For example: "hidden, because it may contain a password".
- **Offer the familiar way in one step:** one flag or one click.
- **Follow an industry standard when there is one**, before inventing anything: Excel's own file parts, git, MCP, W3C PROV, FDA 21 CFR Part 11.

Habits include agents' habits too: JSON, MCP, git and plain command lines.

## Examples, decided 2026-10-20

| Decision | Follow or define | Why |
|---|---|---|
| Comments in Excel's threaded-comment format (issue 64) | Follow | They open and can be answered in Excel |
| Version control through git, not our own system (issues 65, 66) | Follow | Developers and agents already use it |
| Regional number and date conventions (issue 7) | Follow | Each region reads and writes its own way |
| Data connections show parsed fields; raw text and M code are hidden (issue 26) | Define | Secrets; one flag shows the raw text |
| SQL command text shown, hidden whole if it may hold a credential (issue 26) | Follow, with a safety exception | Useful and usually harmless; a partial mask is not trusted |
| CSV import never converts in a way that loses data | Define | Gene names, leading zeros and long numbers |
| Macros kept and listed, never run | Define | Untrusted code |

## Options not taken

- **Always follow habits.** We would copy Excel's data-loss and secret-exposure problems, which are some of the reasons people want something else.
- **Our own definitions throughout.** Every new rule is something to learn. Users don't move to a tool that makes them relearn spreadsheets, and agents are trained on the common formats.
- **Decide case by case with no rule.** The same argument repeats, and the answers drift.

## Consequences

- Easier: one test for design questions. Does following the habit cause harm, and is there a habit to follow?
- Harder: every break needs a "why" message and a one-step way back, which is extra work on every such feature.
- To check: each feature issue that breaks a habit names the reason (harm or no habit) and the one-step way back.
